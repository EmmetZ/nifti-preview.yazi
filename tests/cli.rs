use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use tempfile::TempDir;

fn fixture(rank: u16, dims: [u16; 4], datatype: i16, bitpix: i16, gzip: bool, path: &Path) {
    let mut bytes = vec![0_u8; 352];
    put_i32(&mut bytes, 0, 348);
    put_u16(&mut bytes, 40, rank);
    for (index, dimension) in dims.into_iter().enumerate() {
        put_u16(&mut bytes, 42 + index * 2, dimension);
    }
    put_i16(&mut bytes, 70, datatype);
    put_i16(&mut bytes, 72, bitpix);
    put_f32(&mut bytes, 76, 1.0);
    put_f32(&mut bytes, 80, 1.0);
    put_f32(&mut bytes, 84, 1.0);
    put_f32(&mut bytes, 88, 3.0);
    put_f32(&mut bytes, 92, 1.0);
    put_f32(&mut bytes, 108, 352.0);
    put_i16(&mut bytes, 254, 1);
    put_f32(&mut bytes, 280, -1.0);
    put_f32(&mut bytes, 300, 1.0);
    put_f32(&mut bytes, 320, 3.0);
    bytes[344..348].copy_from_slice(b"n+1\0");

    let voxel_count = dims.into_iter().map(usize::from).product::<usize>();
    match datatype {
        2 => bytes.extend((0..voxel_count).map(|value| value as u8)),
        4 => {
            for value in 0..voxel_count {
                bytes.extend_from_slice(&(value as i16).to_le_bytes());
            }
        }
        16 => {
            for value in 0..voxel_count {
                bytes.extend_from_slice(&(value as f32).to_le_bytes());
            }
        }
        32 | 128 => bytes.resize(bytes.len() + voxel_count * (bitpix as usize / 8), 0),
        _ => {}
    }

    if gzip {
        let file = fs::File::create(path).unwrap();
        let mut encoder = GzEncoder::new(file, Compression::fast());
        encoder.write_all(&bytes).unwrap();
        encoder.finish().unwrap();
    } else {
        fs::write(path, bytes).unwrap();
    }
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_i16(bytes: &mut [u8], offset: usize, value: i16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_i32(bytes: &mut [u8], offset: usize, value: i32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_f32(bytes: &mut [u8], offset: usize, value: f32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_yazi-nifti-preview"))
        .args(arguments)
        .output()
        .unwrap()
}

fn json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    serde_json::from_str(&stdout).unwrap()
}

#[test]
fn probe_and_render_nii_and_gzip() {
    for (name, gzip) in [("basic.nii", false), ("basic.nii.gz", true)] {
        let directory = TempDir::new().unwrap();
        let input = directory.path().join(name);
        fixture(3, [4, 3, 2, 1], 4, 16, gzip, &input);
        let result = json(&run(&["probe", "--input", input.to_str().unwrap()]));
        assert_eq!(result["schema"], 1);
        assert_eq!(result["plane"], "axial");
        assert_eq!(result["slice_count"], 2);

        let rendered = json(&run(&["render", "--input", input.to_str().unwrap()]));
        let image = PathBuf::from(rendered["image"].as_str().unwrap());
        assert!(image.is_file());
        let pixels = image::open(&image).unwrap().to_luma8();
        assert_eq!(pixels.dimensions(), (4, 3));
        assert_eq!(pixels.get_pixel(0, 0).0[0], 222);
        assert_eq!(pixels.get_pixel(3, 0).0[0], 255);
        assert_eq!(pixels.get_pixel(0, 2).0[0], 128);
        let cache_dir = PathBuf::from(rendered["image"].as_str().unwrap())
            .parent()
            .unwrap()
            .to_owned();
        assert!(!cache_dir.join("volume.u8").exists());
        assert!(cache_dir.join("complete").is_file());
    }
}

#[test]
fn four_dimensional_input_uses_only_first_volume() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("four-d.nii");
    fixture(4, [3, 2, 2, 2], 16, 32, false, &input);
    let result = json(&run(&["render", "--input", input.to_str().unwrap()]));
    assert_eq!(result["slice_count"], 2);
    let three_d = directory.path().join("three-d.nii");
    fixture(3, [3, 2, 2, 1], 16, 32, false, &three_d);
    assert_eq!(rendered_pixels(&input), rendered_pixels(&three_d));
}

fn rendered_pixels(input: &Path) -> image::GrayImage {
    let result = json(&run(&["render", "--input", input.to_str().unwrap()]));
    image::open(result["image"].as_str().unwrap())
        .unwrap()
        .to_luma8()
}

#[test]
fn scaling_matches_float_reference_and_handles_undefined_parameters() {
    let directory = TempDir::new().unwrap();
    let plain = directory.path().join("plain.nii");
    fixture(3, [4, 3, 2, 1], 4, 16, false, &plain);
    let plain_bytes = fs::read(&plain).unwrap();
    let plain_pixels = rendered_pixels(&plain);

    let reference = directory.path().join("reference.nii");
    fixture(3, [4, 3, 2, 1], 16, 32, false, &reference);
    let mut bytes = fs::read(&reference).unwrap();
    for value in 0..24 {
        put_f32(&mut bytes, 352 + value * 4, value as f32 * 2.5 - 10.0);
    }
    fs::write(&reference, bytes).unwrap();
    let reference_pixels = rendered_pixels(&reference);
    assert_ne!(
        reference_pixels, plain_pixels,
        "scaling must change the tissue sample"
    );

    let scaled = directory.path().join("scaled.nii");
    let mut bytes = plain_bytes.clone();
    put_f32(&mut bytes, 112, 2.5);
    put_f32(&mut bytes, 116, -10.0);
    fs::write(&scaled, bytes).unwrap();
    assert_eq!(rendered_pixels(&scaled), reference_pixels);

    for (index, slope) in [0.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY]
        .into_iter()
        .enumerate()
    {
        let input = directory
            .path()
            .join(format!("undefined-slope-{index}.nii"));
        let mut bytes = plain_bytes.clone();
        put_f32(&mut bytes, 112, slope);
        put_f32(&mut bytes, 116, 100.0);
        fs::write(&input, bytes).unwrap();
        assert_eq!(rendered_pixels(&input), plain_pixels);
    }
    // The preview is lenient about undefined intercepts with a valid slope.
    let input = directory.path().join("undefined-intercept.nii");
    let mut bytes = plain_bytes;
    put_f32(&mut bytes, 112, 1.0);
    put_f32(&mut bytes, 116, f32::NAN);
    fs::write(&input, bytes).unwrap();
    assert_eq!(rendered_pixels(&input), plain_pixels);
}

#[test]
fn scaling_precedes_f32_cast_for_large_integers() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("large-integers.nii");
    fixture(3, [4, 3, 2, 1], 4, 16, false, &input);
    let mut bytes = fs::read(&input).unwrap();
    bytes.truncate(352);
    put_i16(&mut bytes, 70, 8); // int32
    put_i16(&mut bytes, 72, 32);
    put_f32(&mut bytes, 112, 1.0);
    put_f32(&mut bytes, 116, -16_777_216.0);
    for value in 0..24_i32 {
        bytes.extend_from_slice(&(16_777_216 + value).to_le_bytes());
    }
    fs::write(&input, bytes).unwrap();
    let reference = directory.path().join("reference.nii");
    fixture(3, [4, 3, 2, 1], 4, 16, false, &reference);
    assert_eq!(rendered_pixels(&input), rendered_pixels(&reference));
}

#[test]
fn voxel_offset_padding_is_skipped_in_plain_and_gzip_files() {
    let directory = TempDir::new().unwrap();
    let plain = directory.path().join("plain.nii");
    fixture(3, [4, 3, 2, 1], 4, 16, false, &plain);
    let bytes = fs::read(&plain).unwrap();
    let expected = rendered_pixels(&plain);
    for (name, gzip) in [("padded.nii", false), ("padded.nii.gz", true)] {
        let input = directory.path().join(name);
        let mut padded = bytes[..352].to_vec();
        put_f32(&mut padded, 108, 416.0);
        padded.resize(416, 0);
        padded.extend_from_slice(&bytes[352..]);
        if gzip {
            let mut encoder =
                GzEncoder::new(fs::File::create(&input).unwrap(), Compression::fast());
            encoder.write_all(&padded).unwrap();
            encoder.finish().unwrap();
        } else {
            fs::write(&input, padded).unwrap();
        }
        assert_eq!(rendered_pixels(&input), expected);
    }
    let short = directory.path().join("short-offset.nii");
    let mut bytes = bytes;
    put_f32(&mut bytes, 108, 0.0);
    fs::write(&short, &bytes).unwrap();
    assert_eq!(rendered_pixels(&short), expected);
    let truncated = directory.path().join("truncated-offset.nii");
    bytes.truncate(352);
    put_f32(&mut bytes, 108, 416.0);
    fs::write(&truncated, bytes).unwrap();
    let output = run(&["render", "--input", truncated.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("file ends before vox_offset"));
}

#[test]
fn invalid_voxels_are_black_without_poisoning_finite_neighbors() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("invalid-voxels.nii");
    fixture(3, [4, 3, 2, 1], 16, 32, false, &input);
    let mut bytes = fs::read(&input).unwrap();
    for (index, value) in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY]
        .into_iter()
        .enumerate()
    {
        put_f32(&mut bytes, 352 + (20 + index) * 4, value);
    }
    fs::write(&input, bytes).unwrap();
    let pixels = rendered_pixels(&input);
    assert_eq!(pixels.get_pixel(0, 0).0[0], 0);
    assert_eq!(pixels.get_pixel(1, 0).0[0], 0);
    assert_eq!(pixels.get_pixel(2, 0).0[0], 0);
    assert_eq!(pixels.get_pixel(3, 0).0[0], 255);
    assert!(pixels.get_pixel(1, 1).0[0] > 0);
}

#[test]
fn invalid_voxels_stay_black_in_narrow_windows() {
    let directory = TempDir::new().unwrap();
    // Exercise lower bounds that round upward, downward, or stay exact when
    // converted to f32, for both positive and negative intensity windows.
    for (dims, lower_count) in [([8, 8, 2, 1], 1), ([8, 5, 2, 1], 1), ([8, 8, 2, 1], 2)] {
        let count = dims.into_iter().map(usize::from).product::<usize>();
        for lower in [1.0_f32, -1.0] {
            let upper = lower.next_up();
            for (index, invalid) in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY]
                .into_iter()
                .enumerate()
            {
                let input = directory
                    .path()
                    .join(format!("narrow-{count}-{lower_count}-{lower}-{index}.nii"));
                fixture(3, dims, 16, 32, false, &input);
                let mut bytes = fs::read(&input).unwrap();
                for voxel in 0..count {
                    let value = if voxel == count - 1 {
                        invalid
                    } else if voxel < lower_count {
                        lower
                    } else {
                        upper
                    };
                    put_f32(&mut bytes, 352 + voxel * 4, value);
                }
                fs::write(&input, bytes).unwrap();
                let pixels = rendered_pixels(&input);
                assert_eq!(pixels.get_pixel(7, 0).0[0], 0, "{}", input.display());
                assert_eq!(pixels.get_pixel(6, 0).0[0], 255, "{}", input.display());
            }
        }
    }
}

#[test]
fn rejects_bad_dimensions_datatypes_and_slice_bounds() {
    let directory = TempDir::new().unwrap();
    let two_d = directory.path().join("two-d.nii");
    fixture(2, [3, 2, 1, 1], 2, 8, false, &two_d);
    assert!(
        !run(&["probe", "--input", two_d.to_str().unwrap()])
            .status
            .success()
    );

    for (name, datatype, bitpix) in [("rgb.nii", 128, 24), ("complex.nii", 32, 64)] {
        let input = directory.path().join(name);
        fixture(3, [2, 2, 2, 1], datatype, bitpix, false, &input);
        let output = run(&["probe", "--input", input.to_str().unwrap()]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported NIfTI datatype"));
    }

    let valid = directory.path().join("valid.nii");
    fixture(3, [2, 2, 2, 1], 2, 8, false, &valid);
    let output = run(&["render", "--input", valid.to_str().unwrap(), "--slice", "2"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("out of range"));
}

#[test]
fn handles_spaces_unicode_and_concurrent_first_render() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("脑 scan.nii.gz");
    fixture(3, [16, 12, 5, 1], 2, 8, true, &input);
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_yazi-nifti-preview"));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let binary = binary.clone();
            let input = input.clone();
            thread::spawn(move || {
                Command::new(binary)
                    .args(["render", "--input", input.to_str().unwrap()])
                    .output()
                    .unwrap()
            })
        })
        .collect();
    let images: Vec<_> = handles
        .into_iter()
        .map(|handle| {
            json(&handle.join().unwrap())["image"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert!(images.iter().all(|image| image == &images[0]));
}

#[test]
fn source_update_changes_cache_key() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("updated.nii");
    fixture(3, [4, 3, 2, 1], 2, 8, false, &input);
    let first = json(&run(&["render", "--input", input.to_str().unwrap()]));
    let first_image = first["image"].as_str().unwrap().to_owned();
    let mut bytes = fs::read(&input).unwrap();
    bytes.push(0);
    fs::write(&input, bytes).unwrap();
    let second = json(&run(&["render", "--input", input.to_str().unwrap()]));
    assert_ne!(first_image, second["image"].as_str().unwrap());
}

#[test]
fn corrupted_and_nifti_two_inputs_fail_without_stdout_json() {
    let directory = TempDir::new().unwrap();
    let corrupt = directory.path().join("corrupt.nii");
    fs::write(&corrupt, b"not a nifti file").unwrap();
    let output = run(&["probe", "--input", corrupt.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());

    let nifti_two = directory.path().join("nifti-two.nii");
    let mut bytes = vec![0_u8; 544];
    put_i32(&mut bytes, 0, 540);
    bytes[4..12].copy_from_slice(b"n+2\0\r\n\x1a\n");
    fs::write(&nifti_two, bytes).unwrap();
    let output = run(&["probe", "--input", nifti_two.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn vfs_cache_paths_use_the_logical_filename_for_format_detection() {
    for (logical_name, gzip) in [("remote.nii", false), ("remote.nii.gz", true)] {
        let directory = TempDir::new().unwrap();
        let cached = directory.path().join("7f6c18d09c604f8cb11c1bc8e72f1154");
        fixture(3, [4, 3, 2, 1], 4, 16, gzip, &cached);

        let probed = json(&run(&[
            "probe",
            "--input",
            cached.to_str().unwrap(),
            "--name",
            logical_name,
        ]));
        assert_eq!(probed["slice_count"], 2);

        let rendered = json(&run(&[
            "render",
            "--input",
            cached.to_str().unwrap(),
            "--name",
            logical_name,
        ]));
        assert!(Path::new(rendered["image"].as_str().unwrap()).is_file());
    }
}

#[test]
fn extensionless_input_without_a_logical_filename_is_rejected() {
    let directory = TempDir::new().unwrap();
    let cached = directory.path().join("7f6c18d09c604f8cb11c1bc8e72f1154");
    fixture(3, [2, 2, 2, 1], 2, 8, false, &cached);

    let output = run(&["probe", "--input", cached.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported input"));
}

/// Writes a 1 mm LIA-conformed MGH volume (FreeSurfer's default orientation)
/// with uchar voxels counting up from zero.
fn mgh_fixture(dims: [i32; 3], gzip: bool, path: &Path) {
    let mut bytes = vec![0_u8; 284];
    let mut put = |offset: usize, value: [u8; 4]| bytes[offset..offset + 4].copy_from_slice(&value);
    put(0, 1_i32.to_be_bytes());
    for (axis, dimension) in dims.into_iter().enumerate() {
        put(4 + axis * 4, dimension.to_be_bytes());
    }
    put(16, 1_i32.to_be_bytes());
    put(20, 0_i32.to_be_bytes());
    let floats = [
        1.0_f32, 1.0, 1.0, -1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0,
    ];
    for (index, value) in floats.into_iter().enumerate() {
        put(30 + index * 4, value.to_be_bytes());
    }
    bytes[28..30].copy_from_slice(&1_i16.to_be_bytes());
    let voxel_count = dims
        .into_iter()
        .map(|value| value as usize)
        .product::<usize>();
    bytes.extend((0..voxel_count).map(|value| value as u8));

    if gzip {
        let file = fs::File::create(path).unwrap();
        let mut encoder = GzEncoder::new(file, Compression::fast());
        encoder.write_all(&bytes).unwrap();
        encoder.finish().unwrap();
    } else {
        fs::write(path, bytes).unwrap();
    }
}

#[test]
fn probe_and_render_mgh_and_mgz() {
    for (name, gzip) in [("T1.mgh", false), ("T1.mgz", true), ("T1.mgh.gz", true)] {
        let directory = TempDir::new().unwrap();
        let input = directory.path().join(name);
        // LIA: voxel x -> left, y -> inferior, z -> anterior. The axial plane
        // therefore slices along voxel y.
        mgh_fixture([4, 3, 5], gzip, &input);
        let result = json(&run(&["probe", "--input", input.to_str().unwrap()]));
        assert_eq!(result["plane"], "axial");
        assert_eq!(result["slice_count"], 3);
        assert_eq!(result["default_slice"], 1);

        let rendered = json(&run(&["render", "--input", input.to_str().unwrap()]));
        let pixels = image::open(rendered["image"].as_str().unwrap())
            .unwrap()
            .to_luma8();
        // Width spans R-L (voxel x), height spans A-P (voxel z).
        assert_eq!(pixels.dimensions(), (4, 5));
    }
}

#[test]
fn mgz_orientation_places_anterior_at_top_and_right_on_left() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("orient.mgz");
    mgh_fixture([4, 3, 5], true, &input);
    let rendered = json(&run(&[
        "render",
        "--input",
        input.to_str().unwrap(),
        "--slice",
        "1",
    ]));
    let pixels = image::open(rendered["image"].as_str().unwrap())
        .unwrap()
        .to_luma8();
    // Voxel value = x + 4 * (y + 3 * z). Top row is the most anterior voxel
    // plane (z = 4); the radiological left edge is patient right, which in
    // LIA is voxel x = 0.
    let top_left = pixels.get_pixel(0, 0).0[0];
    let top_right = pixels.get_pixel(3, 0).0[0];
    let bottom_left = pixels.get_pixel(0, 4).0[0];
    assert!(top_left < top_right, "{top_left} >= {top_right}");
    assert!(top_left > bottom_left, "{top_left} <= {bottom_left}");
}

#[test]
fn vfs_mgz_uses_logical_filename_and_rejects_bad_type() {
    let directory = TempDir::new().unwrap();
    let cached = directory.path().join("0d1f6f3ba0d84bd1");
    mgh_fixture([2, 2, 2], true, &cached);
    let probed = json(&run(&[
        "probe",
        "--input",
        cached.to_str().unwrap(),
        "--name",
        "brain.mgz",
    ]));
    assert_eq!(probed["slice_count"], 2);

    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(&cached).unwrap())
        .read_to_end(&mut bytes)
        .unwrap();
    bytes[20..24].copy_from_slice(&7_i32.to_be_bytes());
    let bad = directory.path().join("bad.mgh");
    fs::write(&bad, bytes).unwrap();
    let output = run(&["probe", "--input", bad.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported MGH datatype"));
}

fn mgh_header(dims: [i32; 3]) -> Vec<u8> {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("header.mgh");
    mgh_fixture([1, 1, 1], false, &path);
    let mut bytes = fs::read(&path).unwrap();
    bytes.truncate(284);
    for (axis, dimension) in dims.into_iter().enumerate() {
        bytes[4 + axis * 4..8 + axis * 4].copy_from_slice(&dimension.to_be_bytes());
    }
    bytes
}

#[test]
fn oversized_headers_fail_cleanly_without_allocating() {
    let directory = TempDir::new().unwrap();
    let mgz = directory.path().join("huge.mgz");
    let mut encoder = GzEncoder::new(fs::File::create(&mgz).unwrap(), Compression::fast());
    encoder.write_all(&mgh_header([4096, 4096, 4096])).unwrap();
    encoder.finish().unwrap();

    let nii = directory.path().join("huge.nii");
    fixture(3, [1, 1, 1, 1], 2, 8, false, &nii);
    let mut bytes = fs::read(&nii).unwrap();
    bytes.truncate(352);
    for axis in 0..3 {
        put_u16(&mut bytes, 42 + axis * 2, 4096);
    }
    fs::write(&nii, bytes).unwrap();

    for input in [&mgz, &nii] {
        for command in ["probe", "render"] {
            let output = run(&[command, "--input", input.to_str().unwrap()]);
            assert_eq!(output.status.code(), Some(1), "{}", input.display());
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).contains("preview limit"));
        }
    }
}

#[test]
fn truncated_mgz_body_reports_truncation() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("short.mgz");
    let mut bytes = mgh_header([64, 64, 64]);
    bytes.resize(bytes.len() + 1000, 0);
    let mut encoder = GzEncoder::new(fs::File::create(&input).unwrap(), Compression::fast());
    encoder.write_all(&bytes).unwrap();
    encoder.finish().unwrap();

    let output = run(&["render", "--input", input.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("file is truncated"));
}

#[test]
fn gzip_content_is_detected_regardless_of_extension() {
    let directory = TempDir::new().unwrap();
    let mgh = directory.path().join("compressed.mgh");
    mgh_fixture([4, 3, 5], true, &mgh);
    let nii = directory.path().join("compressed.nii");
    fixture(3, [4, 3, 2, 1], 2, 8, true, &nii);
    let plain_mgz = directory.path().join("plain.mgz");
    mgh_fixture([4, 3, 5], false, &plain_mgz);

    for input in [&mgh, &nii, &plain_mgz] {
        let rendered = json(&run(&["render", "--input", input.to_str().unwrap()]));
        assert!(Path::new(rendered["image"].as_str().unwrap()).is_file());
    }
}
