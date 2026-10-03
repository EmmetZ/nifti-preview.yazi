use flate2::bufread::GzDecoder;
use image::{GrayImage, ImageBuffer, Luma};
use nifti::header::MAGIC_CODE_NI1;
use nifti::volume::ndarray::IntoNdArray;
use nifti::{NiftiError, NiftiHeader, NiftiType, StreamedNiftiVolume};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

mod mgh;

const SCHEMA: u8 = 1;
const CACHE_VERSION: &str = "ras-resampled-v6";
const MAX_IMAGE_EDGE: u32 = 2048;
/// Upper bound on voxels per volume. The f32 volume and window statistics
/// need roughly 8 bytes per voxel, in addition to temporary decoding buffers.
/// Reject corrupt headers before allocating their claimed volume size.
const MAX_VOXELS: usize = 1 << 28;
const MAX_CACHE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 12;
const CACHE_ACTIVITY_GRACE: Duration = Duration::from_secs(60);
const LOCK_STALE_AFTER: Duration = Duration::from_secs(30);
const LOCK_WAIT_LIMIT: Duration = Duration::from_secs(15);
/// Voxels whose magnitude does not exceed this are background and excluded
/// from the display window statistics.
const BACKGROUND_EPSILON: f64 = 1e-8;
/// Display window percentiles over non-background voxels.
const WINDOW_PERCENTILES: (f64, f64) = (0.5, 99.5);
/// Number of levels in matplotlib's gray colormap.
const GRAY_LEVELS: f64 = 256.0;
const NIFTI1_HEADER_SIZE: u64 = 348;
const NIFTI1_MIN_DATA_OFFSET: u64 = 352;

#[derive(Debug, Error)]
pub enum Error {
    #[error("unsupported input: expected a .nii, .nii.gz, .mgh or .mgz file")]
    UnsupportedPath,
    #[error("unsupported NIfTI dimensions: expected 3D or 4D, found {0}D")]
    UnsupportedDimensions(u16),
    #[error("unsupported NIfTI datatype: {0:?}")]
    UnsupportedDatatype(NiftiType),
    #[error("slice {requested} is out of range 0..{count}")]
    SliceOutOfRange { requested: usize, count: usize },
    #[error("unsupported MGH datatype code: {0}")]
    UnsupportedMghType(i32),
    #[error("invalid MGH file: {0}")]
    InvalidMgh(&'static str),
    #[error("volume has {0} voxels, more than the preview limit of {MAX_VOXELS}")]
    VolumeTooLarge(u128),
    #[error("invalid volume geometry: {0}")]
    InvalidGeometry(&'static str),
    #[error("timed out waiting for cache lock: {0}")]
    LockTimeout(PathBuf),
    #[error("NIfTI error: {0}")]
    Nifti(#[from] nifti::NiftiError),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputFormat {
    Nii,
    NiiGz,
    Mgh,
    Mgz,
}

impl InputFormat {
    fn cache_tag(self) -> &'static [u8] {
        match self {
            Self::Nii => b"nii",
            Self::NiiGz => b"nii.gz",
            Self::Mgh => b"mgh",
            Self::Mgz => b"mgz",
        }
    }
}

enum InputReader {
    Plain(BufReader<File>),
    Gzip(GzDecoder<BufReader<File>>),
}

impl Read for InputReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(reader) => reader.read(buffer),
            Self::Gzip(reader) => reader.read(buffer),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Plane {
    Axial,
    Coronal,
    Sagittal,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeResult {
    pub schema: u8,
    pub plane: Plane,
    pub slice_count: usize,
    pub default_slice: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RenderResult {
    pub schema: u8,
    pub image: PathBuf,
    pub plane: Plane,
    pub slice: usize,
    pub slice_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheMeta {
    schema: u8,
    plane: Plane,
    dims: [usize; 3],
    spacing: [f32; 3],
    world_to_voxel: [[f64; 4]; 3],
    world_min: [f64; 3],
    world_max: [f64; 3],
    horizontal_world: usize,
    vertical_world: usize,
    slice_world: usize,
    output_width: u32,
    output_height: u32,
    slice_count: usize,
}

impl CacheMeta {
    fn slice_count(&self) -> usize {
        self.slice_count
    }

    fn probe(&self) -> ProbeResult {
        let count = self.slice_count();
        ProbeResult {
            schema: SCHEMA,
            plane: self.plane,
            slice_count: count,
            default_slice: count / 2,
        }
    }
}

pub fn probe(input: &Path) -> Result<ProbeResult> {
    probe_named(input, None)
}

pub fn probe_named(input: &Path, logical_name: Option<&str>) -> Result<ProbeResult> {
    let format = input_format(input, logical_name)?;
    let key = cache_key(input, format)?;
    let cache_dir = cache_root().join(key);
    if let Some(meta) = read_meta(&cache_dir)? {
        return Ok(meta.probe());
    }
    let meta = match format {
        InputFormat::Nii | InputFormat::NiiGz => geometry_from_header(&read_header(input)?)?,
        InputFormat::Mgh | InputFormat::Mgz => {
            geometry_from_mgh(&mgh::read_header(&mut open_input(input)?)?)?
        }
    };
    Ok(meta.probe())
}

pub fn render(input: &Path, requested_slice: Option<usize>) -> Result<RenderResult> {
    render_named(input, None, requested_slice)
}

pub fn render_named(
    input: &Path,
    logical_name: Option<&str>,
    requested_slice: Option<usize>,
) -> Result<RenderResult> {
    let format = input_format(input, logical_name)?;
    let key = cache_key(input, format)?;
    let root = cache_root();
    fs::create_dir_all(&root)?;
    let cache_dir = root.join(&key);
    ensure_image_cache(input, format, &root, &cache_dir, &key)?;
    let meta = read_meta(&cache_dir)?.ok_or(Error::InvalidGeometry("missing cache metadata"))?;
    let slice = requested_slice.unwrap_or_else(|| meta.slice_count() / 2);
    if slice >= meta.slice_count() {
        return Err(Error::SliceOutOfRange {
            requested: slice,
            count: meta.slice_count(),
        });
    }

    let image = cache_dir.join(format!("slice-{slice:04}.png"));
    if !image.is_file() {
        return Err(Error::InvalidGeometry("cached slice is missing"));
    }
    touch_access(&cache_dir, Some(slice))?;
    prune_cache(&root, &key)?;

    Ok(RenderResult {
        schema: SCHEMA,
        image,
        plane: meta.plane,
        slice,
        slice_count: meta.slice_count(),
    })
}

fn input_format(path: &Path, logical_name: Option<&str>) -> Result<InputFormat> {
    let name = logical_name
        .map(str::to_owned)
        .or_else(|| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .map(|name| name.to_ascii_lowercase())
        .ok_or(Error::UnsupportedPath)?;
    if name.ends_with(".nii.gz") {
        Ok(InputFormat::NiiGz)
    } else if name.ends_with(".nii") {
        Ok(InputFormat::Nii)
    } else if name.ends_with(".mgz") || name.ends_with(".mgh.gz") {
        Ok(InputFormat::Mgz)
    } else if name.ends_with(".mgh") {
        Ok(InputFormat::Mgh)
    } else {
        Err(Error::UnsupportedPath)
    }
}

/// Opens the input, decompressing based on the gzip magic bytes rather than
/// the extension, so mislabeled `.nii`/`.mgh` files still load.
fn open_input(path: &Path) -> Result<InputReader> {
    let mut file = BufReader::new(File::open(path)?);
    Ok(if file.fill_buf()?.starts_with(&[0x1f, 0x8b]) {
        InputReader::Gzip(GzDecoder::new(file))
    } else {
        InputReader::Plain(file)
    })
}

fn read_header(path: &Path) -> Result<NiftiHeader> {
    read_header_from(&mut open_input(path)?)
}

fn read_header_from(reader: &mut impl Read) -> Result<NiftiHeader> {
    let mut header = NiftiHeader::from_reader(reader)?;
    if &header.magic == MAGIC_CODE_NI1 {
        return Err(NiftiError::NoVolumeData.into());
    }
    validate_header(&header)?;
    sanitize_scaling(&mut header);
    Ok(header)
}

/// Applies nibabel's rules for intensity scaling: a zero or non-finite slope
/// disables scaling entirely. nibabel rejects a valid slope paired with a
/// non-finite intercept; a preview treats that intercept as zero instead.
fn sanitize_scaling(header: &mut NiftiHeader) {
    if header.scl_slope == 0.0 || !header.scl_slope.is_finite() {
        header.scl_slope = 1.0;
        header.scl_inter = 0.0;
    } else if !header.scl_inter.is_finite() {
        header.scl_inter = 0.0;
    }
}

/// Byte offset of the voxel data, honouring `vox_offset` the way nibabel
/// does: values below the single-file minimum of 352 are raised to it.
fn data_offset(header: &NiftiHeader) -> u64 {
    let offset = header.vox_offset;
    if offset.is_finite() && offset > NIFTI1_MIN_DATA_OFFSET as f32 {
        offset as u64
    } else {
        NIFTI1_MIN_DATA_OFFSET
    }
}

fn validate_header(header: &NiftiHeader) -> Result<()> {
    let rank = header.dim[0];
    if !(rank == 3 || rank == 4) {
        return Err(Error::UnsupportedDimensions(rank));
    }
    if header.dim[1..=3].contains(&0) {
        return Err(Error::InvalidGeometry("zero-sized spatial dimension"));
    }
    if rank == 4 && header.dim[4] == 0 {
        return Err(Error::InvalidGeometry("zero-sized fourth dimension"));
    }
    match header.data_type()? {
        NiftiType::Uint8
        | NiftiType::Int8
        | NiftiType::Uint16
        | NiftiType::Int16
        | NiftiType::Uint32
        | NiftiType::Int32
        | NiftiType::Uint64
        | NiftiType::Int64
        | NiftiType::Float32
        | NiftiType::Float64 => Ok(()),
        other => Err(Error::UnsupportedDatatype(other)),
    }
}

fn geometry_from_header(header: &NiftiHeader) -> Result<CacheMeta> {
    validate_header(header)?;
    let dims = [
        header.dim[1] as usize,
        header.dim[2] as usize,
        header.dim[3] as usize,
    ];
    let fallback_spacing = std::array::from_fn(|axis| header.pixdim[axis + 1].abs());
    geometry_from_affine(dims, selected_affine(header), fallback_spacing)
}

fn geometry_from_mgh(header: &mgh::MghHeader) -> Result<CacheMeta> {
    geometry_from_affine(header.dims, header.affine, header.spacing)
}

fn geometry_from_affine(
    dims: [usize; 3],
    affine: [[f64; 4]; 4],
    fallback_spacing: [f32; 3],
) -> Result<CacheMeta> {
    let voxels = dims
        .iter()
        .map(|&dimension| dimension as u128)
        .product::<u128>();
    if voxels > MAX_VOXELS as u128 {
        return Err(Error::VolumeTooLarge(voxels));
    }
    let vectors = axis_vectors(&affine);
    let mut spacing = [0.0_f32; 3];
    for voxel_axis in 0..3 {
        spacing[voxel_axis] = vectors[voxel_axis]
            .iter()
            .map(|value| value * value)
            .sum::<f64>()
            .sqrt() as f32;
        if !spacing[voxel_axis].is_finite() || spacing[voxel_axis] <= 0.0 {
            spacing[voxel_axis] = fallback_spacing[voxel_axis];
        }
        if !spacing[voxel_axis].is_finite() || spacing[voxel_axis] <= 0.0 {
            spacing[voxel_axis] = 1.0;
        }
    }

    let world_for_voxel = best_axis_permutation(&vectors);
    let slice_axis = choose_slice_axis(&spacing, &dims, &world_for_voxel);
    let plane = match world_for_voxel[slice_axis] {
        0 => Plane::Sagittal,
        1 => Plane::Coronal,
        _ => Plane::Axial,
    };
    let (horizontal_world, vertical_world, slice_world) = match plane {
        Plane::Axial => (0, 1, 2),
        Plane::Coronal => (0, 2, 1),
        Plane::Sagittal => (1, 2, 0),
    };
    let world_spacing: [f64; 3] = std::array::from_fn(|world_axis| {
        let voxel_axis = axis_for_world(&world_for_voxel, world_axis).unwrap_or(world_axis);
        f64::from(spacing[voxel_axis])
    });
    let (world_min, world_max) = world_bounds(&affine, &dims);
    let desired_width = sample_count(
        world_max[horizontal_world] - world_min[horizontal_world],
        world_spacing[horizontal_world],
    );
    let desired_height = sample_count(
        world_max[vertical_world] - world_min[vertical_world],
        world_spacing[vertical_world],
    );
    let (output_width, output_height) = fit_dimensions(desired_width, desired_height);
    let slice_count = sample_count(
        world_max[slice_world] - world_min[slice_world],
        world_spacing[slice_world],
    )
    .round()
    .max(1.0) as usize;

    Ok(CacheMeta {
        schema: SCHEMA,
        plane,
        dims,
        spacing,
        world_to_voxel: invert_affine(&affine)?,
        world_min,
        world_max,
        horizontal_world,
        vertical_world,
        slice_world,
        output_width,
        output_height,
        slice_count,
    })
}

fn selected_affine(header: &NiftiHeader) -> [[f64; 4]; 4] {
    if (1..=5).contains(&header.sform_code) {
        let affine = header.sform_affine::<f64>();
        let affine = std::array::from_fn(|row| std::array::from_fn(|column| affine[(row, column)]));
        if valid_axis_vectors(&axis_vectors(&affine)) {
            return affine;
        }
    }
    if (1..=5).contains(&header.qform_code) {
        let affine = header.qform_affine::<f64>();
        let affine = std::array::from_fn(|row| std::array::from_fn(|column| affine[(row, column)]));
        if valid_axis_vectors(&axis_vectors(&affine)) {
            return affine;
        }
    }
    let mut affine = [[0.0; 4]; 4];
    for (axis, row) in affine.iter_mut().enumerate().take(3) {
        let spacing = header.pixdim[axis + 1].abs();
        row[axis] = if spacing.is_finite() && spacing > 0.0 {
            f64::from(spacing)
        } else {
            1.0
        };
    }
    affine[3][3] = 1.0;
    affine
}

fn axis_vectors(affine: &[[f64; 4]; 4]) -> [[f64; 3]; 3] {
    std::array::from_fn(|voxel_axis| {
        std::array::from_fn(|world_axis| affine[world_axis][voxel_axis])
    })
}

pub(crate) fn valid_axis_vectors(vectors: &[[f64; 3]; 3]) -> bool {
    if vectors.iter().flatten().any(|value| !value.is_finite()) {
        return false;
    }
    let [a, b, c] = *vectors;
    let determinant = a[0] * (b[1] * c[2] - b[2] * c[1]) - b[0] * (a[1] * c[2] - a[2] * c[1])
        + c[0] * (a[1] * b[2] - a[2] * b[1]);
    determinant.abs() > f64::EPSILON
}

fn best_axis_permutation(vectors: &[[f64; 3]; 3]) -> [usize; 3] {
    const PERMUTATIONS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    PERMUTATIONS
        .into_iter()
        .max_by(|a, b| {
            let score = |permutation: &[usize; 3]| {
                (0..3)
                    .map(|i| vectors[i][permutation[i]].abs())
                    .sum::<f64>()
            };
            score(a).partial_cmp(&score(b)).unwrap_or(Ordering::Equal)
        })
        .unwrap_or([0, 1, 2])
}

fn choose_slice_axis(spacing: &[f32; 3], dims: &[usize; 3], world: &[usize; 3]) -> usize {
    let max_spacing = spacing.iter().copied().fold(0.0_f32, f32::max);
    let mut candidates: Vec<usize> = (0..3)
        .filter(|&i| spacing[i] >= max_spacing * 0.95)
        .collect();
    candidates.sort_by_key(|&i| {
        let plane_priority = match world[i] {
            2 => 0,
            1 => 1,
            _ => 2,
        };
        (plane_priority, dims[i])
    });
    candidates[0]
}

fn axis_for_world(mapping: &[usize; 3], world_axis: usize) -> Result<usize> {
    mapping
        .iter()
        .position(|&axis| axis == world_axis)
        .ok_or(Error::InvalidGeometry("affine axes are not independent"))
}

fn sample_count(range: f64, spacing: f64) -> f64 {
    (range.max(0.0) / spacing.max(f64::EPSILON)).ceil() + 1.0
}

fn fit_dimensions(width: f64, height: f64) -> (u32, u32) {
    let width = width.max(1.0);
    let height = height.max(1.0);
    let scale = (MAX_IMAGE_EDGE as f64 / width.max(height)).min(1.0);
    (
        (width * scale).round().max(1.0) as u32,
        (height * scale).round().max(1.0) as u32,
    )
}

fn world_bounds(affine: &[[f64; 4]; 4], dims: &[usize; 3]) -> ([f64; 3], [f64; 3]) {
    let mut minimum = [f64::INFINITY; 3];
    let mut maximum = [f64::NEG_INFINITY; 3];
    for corner in 0..8 {
        let voxel = std::array::from_fn::<_, 3, _>(|axis| {
            if corner & (1 << axis) == 0 {
                0.0
            } else {
                dims[axis].saturating_sub(1) as f64
            }
        });
        for world_axis in 0..3 {
            let value = affine[world_axis][3]
                + (0..3)
                    .map(|voxel_axis| affine[world_axis][voxel_axis] * voxel[voxel_axis])
                    .sum::<f64>();
            minimum[world_axis] = minimum[world_axis].min(value);
            maximum[world_axis] = maximum[world_axis].max(value);
        }
    }
    (minimum, maximum)
}

fn invert_affine(affine: &[[f64; 4]; 4]) -> Result<[[f64; 4]; 3]> {
    let a = std::array::from_fn::<_, 3, _>(|row| {
        std::array::from_fn::<_, 3, _>(|column| affine[row][column])
    });
    let determinant = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if !determinant.is_finite() || determinant.abs() <= f64::EPSILON {
        return Err(Error::InvalidGeometry("affine is singular"));
    }
    let inverse = [
        [
            (a[1][1] * a[2][2] - a[1][2] * a[2][1]) / determinant,
            (a[0][2] * a[2][1] - a[0][1] * a[2][2]) / determinant,
            (a[0][1] * a[1][2] - a[0][2] * a[1][1]) / determinant,
        ],
        [
            (a[1][2] * a[2][0] - a[1][0] * a[2][2]) / determinant,
            (a[0][0] * a[2][2] - a[0][2] * a[2][0]) / determinant,
            (a[0][2] * a[1][0] - a[0][0] * a[1][2]) / determinant,
        ],
        [
            (a[1][0] * a[2][1] - a[1][1] * a[2][0]) / determinant,
            (a[0][1] * a[2][0] - a[0][0] * a[2][1]) / determinant,
            (a[0][0] * a[1][1] - a[0][1] * a[1][0]) / determinant,
        ],
    ];
    Ok(std::array::from_fn(|row| {
        let offset = -(0..3)
            .map(|column| inverse[row][column] * affine[column][3])
            .sum::<f64>();
        [inverse[row][0], inverse[row][1], inverse[row][2], offset]
    }))
}

fn ensure_image_cache(
    input: &Path,
    format: InputFormat,
    root: &Path,
    cache_dir: &Path,
    key: &str,
) -> Result<()> {
    if read_meta(cache_dir)?.is_some() && cache_dir.join("complete").is_file() {
        return Ok(());
    }
    let _lock = CacheLock::acquire(root, key)?;
    if read_meta(cache_dir)?.is_some() && cache_dir.join("complete").is_file() {
        return Ok(());
    }

    fs::create_dir_all(cache_dir)?;
    let (meta, mut values) = match format {
        InputFormat::Nii | InputFormat::NiiGz => load_nifti(input)?,
        InputFormat::Mgh | InputFormat::Mgz => load_mgh(input)?,
    };
    let window = display_window(&values);
    // Invalid voxels render black without poisoning neighboring samples during
    // interpolation (even a zero-weight NaN would otherwise propagate).
    let mut background = window.0 as f32;
    // Round toward negative infinity: rounding above the lower bound can turn
    // an invalid voxel white when the display window is very narrow.
    if f64::from(background) > window.0 {
        background = background.next_down();
    }
    for value in &mut values {
        if !value.is_finite() {
            *value = background;
        }
    }
    for slice in 0..meta.slice_count() {
        let image = cache_dir.join(format!("slice-{slice:04}.png"));
        if !image.is_file() {
            write_png_atomic(&image, &render_slice(&values, window, &meta, slice)?)?;
        }
    }
    write_atomic(&cache_dir.join("meta.json"), &serde_json::to_vec(&meta)?)?;
    write_atomic(&cache_dir.join("complete"), b"")?;
    touch_access(cache_dir, None)?;
    Ok(())
}

fn load_mgh(input: &Path) -> Result<(CacheMeta, Vec<f32>)> {
    let mut reader = open_input(input)?;
    let header = mgh::read_header(&mut reader)?;
    let meta = geometry_from_mgh(&header)?;
    let values = mgh::read_first_frame(&mut reader, &header)?;
    Ok((meta, values))
}

/// Loads the first volume as scaled `f32`, matching
/// `np.asarray(nib.load(path).dataobj, dtype=np.float32)`.
fn load_nifti(input: &Path) -> Result<(CacheMeta, Vec<f32>)> {
    let mut reader = open_input(input)?;
    let header = read_header_from(&mut reader)?;
    let meta = geometry_from_header(&header)?;
    // Skip extensions and padding up to vox_offset.
    let skip = data_offset(&header) - NIFTI1_HEADER_SIZE;
    if io::copy(&mut (&mut reader).take(skip), &mut io::sink())? != skip {
        return Err(Error::InvalidGeometry("file ends before vox_offset"));
    }
    let rank = header.dim[0];
    let mut values = Vec::new();
    for slice in StreamedNiftiVolume::from_reader(reader, &header)? {
        // Scale before the final f32 cast, so large stored integers are not
        // rounded before slope/intercept are applied.
        let array = slice?.into_ndarray::<f64>()?;
        let raw = array
            .as_slice_memory_order()
            .ok_or(Error::InvalidGeometry("NIfTI volume is not contiguous"))?;
        values.extend(raw.iter().map(|&value| value as f32));
        if rank == 4 {
            break;
        }
    }
    if values.len() != meta.dims.iter().product::<usize>() {
        return Err(Error::InvalidGeometry(
            "first volume size does not match header",
        ));
    }
    Ok((meta, values))
}

fn touch_access(cache_dir: &Path, slice: Option<usize>) -> Result<()> {
    fs::write(
        cache_dir.join("access"),
        slice.map(|value| value.to_string()).unwrap_or_default(),
    )?;
    Ok(())
}

fn prune_cache(root: &Path, current_key: &str) -> Result<()> {
    let _lock = CacheLock::acquire(root, "prune")?;
    let mut entries = Vec::new();
    for item in fs::read_dir(root)? {
        let item = item?;
        if !item.file_type()?.is_dir() {
            continue;
        }
        let path = item.path();
        let key = item.file_name().to_string_lossy().into_owned();
        let modified = fs::metadata(path.join("access"))
            .or_else(|_| fs::metadata(&path))?
            .modified()
            .unwrap_or(UNIX_EPOCH);
        match directory_size(&path) {
            Ok(size) => entries.push((
                key,
                path.clone(),
                size,
                modified,
                path.join("volume.u8").is_file(),
            )),
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    entries.sort_by_key(|entry| entry.3);
    let mut retained = Vec::with_capacity(entries.len());
    for (key, path, size, modified, legacy) in entries {
        let removable_legacy = legacy
            && key != current_key
            && !root.join(format!("{key}.lock")).is_file()
            && !cache_recently_active(&path)?;
        if removable_legacy {
            match fs::remove_dir_all(&path) {
                Ok(()) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            }
        }
        retained.push((key, path, size, modified, legacy));
    }
    let entries = retained;
    let mut total = entries.iter().map(|entry| entry.2).sum::<u64>();
    let mut count = entries.len();
    for (key, path, size, _, _) in entries {
        if count <= MAX_CACHE_ENTRIES && total <= MAX_CACHE_BYTES {
            break;
        }
        if key == current_key {
            continue;
        }
        if root.join(format!("{key}.lock")).is_file() || cache_recently_active(&path)? {
            continue;
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => {
                total = total.saturating_sub(size);
                count = count.saturating_sub(1);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn cache_recently_active(path: &Path) -> Result<bool> {
    Ok(fs::metadata(path.join("access"))
        .or_else(|_| fs::metadata(path))?
        .modified()
        .ok()
        .and_then(|time| time.elapsed().ok())
        .is_some_and(|age| age < CACHE_ACTIVITY_GRACE))
}

fn directory_size(path: &Path) -> Result<u64> {
    let mut total = 0_u64;
    for item in fs::read_dir(path)? {
        let item = item?;
        let metadata = item.metadata()?;
        total = total.saturating_add(if metadata.is_dir() {
            directory_size(&item.path())?
        } else {
            metadata.len()
        });
    }
    Ok(total)
}

/// Robust, volume-wide contrast matching run_ANT_reg.py. Unlike the script,
/// binary masks fall back to the full finite range to remain visible.
fn display_window(values: &[f32]) -> (f64, f64) {
    let mut tissue: Vec<f32> = values
        .iter()
        .copied()
        .filter(|value| value.is_finite() && f64::from(*value).abs() > BACKGROUND_EPSILON)
        .collect();
    if tissue.is_empty() {
        tissue.extend(values.iter().copied().filter(|value| value.is_finite()));
    }
    if tissue.is_empty() {
        return (0.0, 1.0);
    }
    let mut low = percentile(&mut tissue, WINDOW_PERCENTILES.0);
    let mut high = percentile(&mut tissue, WINDOW_PERCENTILES.1);
    if high <= low {
        (low, high) = finite_range(&tissue);
    }
    if high <= low {
        (low, high) = finite_range(values);
    }
    if high <= low {
        // Use a relative increment as well: low + 1 can equal low for very
        // large f32 constants when promoted to f64.
        high = low + 1.0;
        if high <= low {
            high = low + low.abs();
        }
    }
    (low, high)
}

fn finite_range(values: &[f32]) -> (f64, f64) {
    values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), value| {
            (low.min(f64::from(value)), high.max(f64::from(value)))
        })
}

/// NumPy's default linear percentile, using selection rather than a full sort.
/// The sample must be nonempty and contain only finite values.
fn percentile(values: &mut [f32], percent: f64) -> f64 {
    let position = (values.len() - 1) as f64 * percent / 100.0;
    let index = position.floor() as usize;
    let fraction = position - index as f64;
    let (_, lower, rest) = values.select_nth_unstable_by(index, f32::total_cmp);
    let lower = f64::from(*lower);
    if fraction == 0.0 {
        return lower;
    }
    let upper = rest.iter().copied().min_by(f32::total_cmp).unwrap();
    lower + (f64::from(upper) - lower) * fraction
}

/// Linear normalization followed by matplotlib's 256-entry gray LUT indexing.
fn to_gray(value: f64, (low, high): (f64, f64)) -> u8 {
    if !value.is_finite() {
        return 0;
    }
    let index = (((value - low) / (high - low)).clamp(0.0, 1.0) * GRAY_LEVELS).min(255.0) as u8;
    // The LUT uses linspace(0, 1, 256), then truncates gray * 255 to a byte.
    // Preserve its floating-point rounding (e.g. index 33 becomes byte 32).
    (f64::from(index) * (1.0 / 255.0) * 255.0) as u8
}

fn render_slice(
    data: &[f32],
    window: (f64, f64),
    meta: &CacheMeta,
    slice: usize,
) -> Result<GrayImage> {
    if data.len() != meta.dims.iter().product::<usize>() {
        return Err(Error::InvalidGeometry("cached volume size is invalid"));
    }
    let mut image = ImageBuffer::<Luma<u8>, Vec<u8>>::new(meta.output_width, meta.output_height);
    for row in 0..meta.output_height {
        for column in 0..meta.output_width {
            let mut world = [0.0; 3];
            world[meta.horizontal_world] = grid_coordinate(
                meta.world_min[meta.horizontal_world],
                meta.world_max[meta.horizontal_world],
                column as usize,
                meta.output_width as usize,
                true,
            );
            world[meta.vertical_world] = grid_coordinate(
                meta.world_min[meta.vertical_world],
                meta.world_max[meta.vertical_world],
                row as usize,
                meta.output_height as usize,
                true,
            );
            world[meta.slice_world] = grid_coordinate(
                meta.world_min[meta.slice_world],
                meta.world_max[meta.slice_world],
                slice,
                meta.slice_count,
                false,
            );
            let voxel = std::array::from_fn(|axis| {
                meta.world_to_voxel[axis][3]
                    + (0..3)
                        .map(|world_axis| meta.world_to_voxel[axis][world_axis] * world[world_axis])
                        .sum::<f64>()
            });
            image.put_pixel(
                column,
                row,
                Luma([sample_trilinear(data, &meta.dims, voxel)
                    .map(|value| to_gray(value, window))
                    .unwrap_or(0)]),
            );
        }
    }
    Ok(image)
}

fn grid_coordinate(minimum: f64, maximum: f64, index: usize, count: usize, reverse: bool) -> f64 {
    if count <= 1 {
        return (minimum + maximum) * 0.5;
    }
    let fraction = index as f64 / (count - 1) as f64;
    if reverse {
        maximum - (maximum - minimum) * fraction
    } else {
        minimum + (maximum - minimum) * fraction
    }
}

fn sample_trilinear(data: &[f32], dims: &[usize; 3], voxel: [f64; 3]) -> Option<f64> {
    const TOLERANCE: f64 = 1e-6;
    if (0..3).any(|axis| {
        !voxel[axis].is_finite()
            || voxel[axis] < -TOLERANCE
            || voxel[axis] > dims[axis].saturating_sub(1) as f64 + TOLERANCE
    }) {
        return None;
    }
    let voxel = std::array::from_fn::<_, 3, _>(|axis| {
        voxel[axis].clamp(0.0, dims[axis].saturating_sub(1) as f64)
    });
    let lower = voxel.map(|value| value.floor() as usize);
    let upper = std::array::from_fn::<_, 3, _>(|axis| (lower[axis] + 1).min(dims[axis] - 1));
    let fraction = std::array::from_fn::<_, 3, _>(|axis| voxel[axis] - lower[axis] as f64);
    let value_at = |x: usize, y: usize, z: usize| f64::from(data[x + dims[0] * (y + dims[1] * z)]);
    let interpolate = |a: f64, b: f64, amount: f64| a + (b - a) * amount;
    let lower_y = interpolate(
        interpolate(
            value_at(lower[0], lower[1], lower[2]),
            value_at(upper[0], lower[1], lower[2]),
            fraction[0],
        ),
        interpolate(
            value_at(lower[0], upper[1], lower[2]),
            value_at(upper[0], upper[1], lower[2]),
            fraction[0],
        ),
        fraction[1],
    );
    let upper_y = interpolate(
        interpolate(
            value_at(lower[0], lower[1], upper[2]),
            value_at(upper[0], lower[1], upper[2]),
            fraction[0],
        ),
        interpolate(
            value_at(lower[0], upper[1], upper[2]),
            value_at(upper[0], upper[1], upper[2]),
            fraction[0],
        ),
        fraction[1],
    );
    Some(interpolate(lower_y, upper_y, fraction[2]))
}

fn cache_root() -> PathBuf {
    std::env::temp_dir().join("yazi-nifti-preview")
}

fn cache_key(path: &Path, format: InputFormat) -> Result<String> {
    let canonical = fs::canonicalize(path)?;
    let metadata = fs::metadata(&canonical)?;
    let modified = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let mut hash = Sha256::new();
    hash.update(CACHE_VERSION.as_bytes());
    hash.update(format.cache_tag());
    hash.update(canonical.as_os_str().as_encoded_bytes());
    hash.update(metadata.len().to_le_bytes());
    hash.update(modified.as_secs().to_le_bytes());
    hash.update(modified.subsec_nanos().to_le_bytes());
    let digest = hash.finalize();
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn read_meta(cache_dir: &Path) -> Result<Option<CacheMeta>> {
    match fs::read(cache_dir.join("meta.json")) {
        Ok(bytes) => match serde_json::from_slice::<CacheMeta>(&bytes) {
            Ok(meta) if meta.schema == SCHEMA => Ok(Some(meta)),
            Ok(_) | Err(_) => Ok(None),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = temporary_path(path, "tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn write_png_atomic(path: &Path, image: &GrayImage) -> Result<()> {
    let temporary = temporary_path(path, "tmp.png");
    image.save_with_format(&temporary, image::ImageFormat::Png)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn temporary_path(path: &Path, extension: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    path.with_extension(format!("{extension}-{}-{nonce}", std::process::id()))
}

struct CacheLock {
    path: PathBuf,
}

impl CacheLock {
    fn acquire(root: &Path, name: &str) -> Result<Self> {
        fs::create_dir_all(root)?;
        let path = root.join(format!("{name}.lock"));
        let started = SystemTime::now();
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    writeln!(
                        file,
                        "pid={} created={}",
                        std::process::id(),
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs()
                    )?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(&path)
                        .and_then(|meta| meta.modified())
                        .ok()
                        .and_then(|time| time.elapsed().ok())
                        .is_some_and(|age| age > LOCK_STALE_AFTER);
                    if stale {
                        match fs::remove_file(&path) {
                            Ok(()) => continue,
                            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                            Err(error) => return Err(error.into()),
                        }
                    }
                    if started.elapsed().unwrap_or_default() > LOCK_WAIT_LIMIT {
                        return Err(Error::LockTimeout(path));
                    }
                    thread::sleep(Duration::from_millis(40));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_excludes_background_and_invalid_values() {
        let mut values = vec![0.0; 700];
        values.extend((1..=300).map(|value| value as f32));
        let expected = (2.495, 298.505);
        let window = display_window(&values);
        assert!((window.0 - expected.0).abs() < 1e-10);
        assert!((window.1 - expected.1).abs() < 1e-10);
        values.extend([1e-9, -1e-9, f32::NAN, f32::INFINITY, f32::NEG_INFINITY]);
        assert_eq!(display_window(&values), window);
    }

    #[test]
    fn percentiles_use_linear_interpolation() {
        let mut values = [4.0, 2.0, 1.0, 3.0];
        assert!((percentile(&mut values, 0.5) - 1.015).abs() < 1e-12);
        assert!((percentile(&mut values, 99.5) - 3.985).abs() < 1e-12);
        assert_eq!(percentile(&mut values, 0.0), 1.0);
        assert_eq!(percentile(&mut values, 100.0), 4.0);
        assert_eq!(percentile(&mut [7.0], 99.5), 7.0);
    }

    #[test]
    fn window_fallbacks_keep_masks_visible_and_constants_black() {
        assert_eq!(display_window(&[0.0, 1.0, 1.0, f32::NAN]), (0.0, 1.0));
        assert_eq!(display_window(&[0.0, 0.0]), (0.0, 1.0));
        assert_eq!(display_window(&[7.0, 7.0, f32::NAN]), (7.0, 8.0));
        assert_eq!(display_window(&[f32::NAN, f32::INFINITY]), (0.0, 1.0));
        assert_eq!(display_window(&[]), (0.0, 1.0));
        // Percentiles collapse even though rare outliers retain a tissue range.
        let mut values = vec![7.0; 1000];
        values.extend([1.0, 20.0]);
        assert_eq!(display_window(&values), (1.0, 20.0));
        for value in [7.0, -7.0, f32::MAX, f32::MIN] {
            let window = display_window(&[value; 2]);
            assert!(window.1 > window.0);
            assert_eq!(to_gray(f64::from(value), window), 0);
        }
    }

    #[test]
    fn gray_mapping_matches_matplotlib_lut_and_clipping() {
        let window = (10.0, 20.0);
        assert_eq!(to_gray(0.0, window), 0);
        assert_eq!(to_gray(10.0, window), 0);
        assert_eq!(to_gray(15.0, window), 128);
        assert_eq!(to_gray(20.0, window), 255);
        assert_eq!(to_gray(30.0, window), 255);
        assert_eq!(to_gray(f64::NAN, window), 0);
        assert_eq!(to_gray(f64::INFINITY, window), 0);
        assert_eq!(to_gray(33.0, (0.0, 256.0)), 32);
        assert_eq!(to_gray(66.0, (0.0, 256.0)), 65);
    }

    #[test]
    fn thickest_axis_wins_then_anatomical_plane_breaks_ties() {
        assert_eq!(
            choose_slice_axis(&[1.0, 3.0, 1.0], &[20, 30, 40], &[0, 1, 2]),
            1
        );
        assert_eq!(
            choose_slice_axis(&[2.0, 2.05, 1.0], &[12, 8, 4], &[0, 1, 2]),
            1
        );
    }

    #[test]
    fn isotropic_volume_defaults_to_axial_even_when_another_axis_is_shorter() {
        assert_eq!(
            choose_slice_axis(&[0.7115, 0.7102, 0.7102], &[281, 352, 352], &[0, 1, 2]),
            2
        );
    }

    #[test]
    fn plane_priority_breaks_complete_tie() {
        assert_eq!(choose_slice_axis(&[1.0; 3], &[10; 3], &[0, 1, 2]), 2);
    }

    #[test]
    fn best_permutation_handles_swapped_axes() {
        let vectors = [[0.0, 0.0, 2.0], [-3.0, 0.0, 0.0], [0.0, 4.0, 0.0]];
        assert_eq!(best_axis_permutation(&vectors), [2, 0, 1]);
    }

    #[test]
    fn invalid_sform_falls_back_to_valid_qform() {
        let mut header = NiftiHeader {
            dim: [3, 2, 2, 2, 1, 1, 1, 1],
            datatype: NiftiType::Uint8 as i16,
            bitpix: 8,
            sform_code: 1,
            qform_code: 1,
            srow_x: [f32::NAN; 4],
            pixdim: [1.0; 8],
            ..NiftiHeader::default()
        };
        header.quatern_b = 0.0;
        header.quatern_c = 0.0;
        header.quatern_d = 0.0;
        let vectors = axis_vectors(&selected_affine(&header));
        assert!(valid_axis_vectors(&vectors));
        assert!(vectors.iter().flatten().all(|value| value.is_finite()));
    }

    #[test]
    fn stale_cache_lock_can_be_reclaimed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("stale.lock");
        let file = fs::File::create(&path).unwrap();
        let stale_time = SystemTime::now() - LOCK_STALE_AFTER - Duration::from_secs(1);
        file.set_times(fs::FileTimes::new().set_modified(stale_time))
            .unwrap();
        drop(file);

        let lock = CacheLock::acquire(directory.path(), "stale").unwrap();
        assert!(lock.path.is_file());
    }

    #[test]
    fn legacy_volume_cache_is_reclaimed() {
        let directory = tempfile::tempdir().unwrap();
        let legacy = directory.path().join("legacy");
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("volume.u8"), [0_u8; 32]).unwrap();
        let file = fs::File::open(&legacy).unwrap();
        let stale_time = SystemTime::now() - CACHE_ACTIVITY_GRACE - Duration::from_secs(1);
        file.set_times(fs::FileTimes::new().set_modified(stale_time))
            .unwrap();

        prune_cache(directory.path(), "current").unwrap();
        assert!(!legacy.exists());
    }

    #[test]
    fn radiological_flips_are_applied_to_slice() {
        let meta = CacheMeta {
            schema: 1,
            plane: Plane::Axial,
            dims: [2, 2, 1],
            spacing: [1.0; 3],
            world_to_voxel: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            world_min: [0.0; 3],
            world_max: [1.0, 1.0, 0.0],
            horizontal_world: 0,
            vertical_world: 1,
            slice_world: 2,
            output_width: 2,
            output_height: 2,
            slice_count: 1,
        };
        assert_eq!(
            render_slice(&[1.0, 2.0, 3.0, 4.0], (0.0, 256.0), &meta, 0)
                .unwrap()
                .into_raw(),
            [4, 3, 2, 1]
        );
        // A sample halfway between -10 and 30 must be interpolated to 10
        // before clipping to the display window; clipping first gives 5.
        let interpolated_meta = CacheMeta {
            output_width: 3,
            ..meta.clone()
        };
        let pixels = render_slice(
            &[-10.0, 30.0, -10.0, 30.0],
            (0.0, 20.0),
            &interpolated_meta,
            0,
        )
        .unwrap();
        assert_eq!(pixels.get_pixel(1, 0).0[0], 128);
        // Out-of-FOV must stay black even when the window spans negative data.
        let outside_meta = CacheMeta {
            world_min: [-1.0, 0.0, 0.0],
            ..meta
        };
        let pixels = render_slice(&[0.0; 4], (-10.0, 10.0), &outside_meta, 0).unwrap();
        assert_eq!(pixels.get_pixel(1, 0).0[0], 0);
    }

    #[test]
    fn affine_inverse_restores_voxel_coordinates() {
        let affine = [
            [0.7, 0.1, -0.2, -80.0],
            [-0.1, 0.8, 0.05, -110.0],
            [0.2, 0.0, 0.75, -140.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let inverse = invert_affine(&affine).unwrap();
        let voxel = [123.0, 87.0, 210.0];
        let world: [f64; 3] = std::array::from_fn(|row| {
            affine[row][3]
                + (0..3)
                    .map(|column| affine[row][column] * voxel[column])
                    .sum::<f64>()
        });
        let restored: [f64; 3] = std::array::from_fn(|row| {
            inverse[row][3]
                + (0..3)
                    .map(|column| inverse[row][column] * world[column])
                    .sum::<f64>()
        });
        for axis in 0..3 {
            assert!((restored[axis] - voxel[axis]).abs() < 1e-10);
        }
    }

    #[test]
    fn trilinear_sampling_interpolates_all_eight_neighbors() {
        let data = [0.0, 10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0];
        assert_eq!(sample_trilinear(&data, &[2, 2, 2], [0.5; 3]), Some(35.0));
        assert_eq!(sample_trilinear(&data, &[2, 2, 2], [0.25; 3]), Some(17.5));
        assert_eq!(sample_trilinear(&data, &[2, 2, 2], [-1.0, 0.0, 0.0]), None);
        assert_eq!(sample_trilinear(&data, &[2, 2, 2], [f64::NAN; 3]), None);
    }
}
