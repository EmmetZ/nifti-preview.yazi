//! FreeSurfer MGH (`.mgh`) and gzip-compressed MGZ (`.mgz`) volume reader.
//!
//! Layout: a 284-byte big-endian header followed by voxel data in
//! column-major order (x fastest, frames slowest).

use crate::{Error, Result, valid_axis_vectors};
use std::io::{self, Read};

const HEADER_SIZE: usize = 284;
const VERSION: i32 = 1;
/// FreeSurfer's default conformed orientation (LIA) used when the header
/// carries no valid direction cosines.
const DEFAULT_DIRECTIONS: [[f64; 3]; 3] = [[-1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MghType {
    Uchar,
    Int,
    Float,
    Short,
    Ushort,
}

impl MghType {
    fn from_code(code: i32) -> Result<Self> {
        match code {
            0 => Ok(Self::Uchar),
            1 => Ok(Self::Int),
            3 => Ok(Self::Float),
            4 => Ok(Self::Short),
            10 => Ok(Self::Ushort),
            other => Err(Error::UnsupportedMghType(other)),
        }
    }

    fn size(self) -> usize {
        match self {
            Self::Uchar => 1,
            Self::Short | Self::Ushort => 2,
            Self::Int | Self::Float => 4,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MghHeader {
    pub(crate) dims: [usize; 3],
    pub(crate) data_type: MghType,
    pub(crate) spacing: [f32; 3],
    pub(crate) affine: [[f64; 4]; 4],
    voxel_count: usize,
}

pub(crate) fn read_header(reader: &mut impl Read) -> Result<MghHeader> {
    let mut bytes = [0_u8; HEADER_SIZE];
    reader.read_exact(&mut bytes).map_err(truncated)?;
    let int = |offset: usize| i32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let float = |offset: usize| f32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap());

    if int(0) != VERSION {
        return Err(Error::InvalidMgh("unsupported MGH version"));
    }
    let raw_dims = [int(4), int(8), int(12)];
    if raw_dims.iter().any(|&dimension| dimension <= 0) {
        return Err(Error::InvalidGeometry("zero-sized spatial dimension"));
    }
    if int(16) <= 0 {
        return Err(Error::InvalidMgh("no frames"));
    }
    let dims = raw_dims.map(|dimension| dimension as usize);
    let data_type = MghType::from_code(int(20))?;
    let voxel_count = dims
        .iter()
        .try_fold(1_usize, |total, &dimension| total.checked_mul(dimension))
        .filter(|count| count.checked_mul(data_type.size()).is_some())
        .ok_or(Error::InvalidMgh("volume is too large"))?;

    let good_ras = i16::from_be_bytes([bytes[28], bytes[29]]) > 0;
    let (spacing, directions, center) = if good_ras {
        let spacing = std::array::from_fn(|axis| {
            let value = float(30 + axis * 4);
            if value.is_finite() && value > 0.0 {
                value
            } else {
                1.0
            }
        });
        let directions: [[f64; 3]; 3] = std::array::from_fn(|voxel_axis| {
            std::array::from_fn(|world_axis| {
                f64::from(float(42 + (voxel_axis * 3 + world_axis) * 4))
            })
        });
        let directions = if valid_axis_vectors(&directions) {
            directions
        } else {
            DEFAULT_DIRECTIONS
        };
        let center: [f64; 3] = std::array::from_fn(|axis| f64::from(float(78 + axis * 4)));
        let center = if center.iter().all(|value| value.is_finite()) {
            center
        } else {
            [0.0; 3]
        };
        (spacing, directions, center)
    } else {
        ([1.0; 3], DEFAULT_DIRECTIONS, [0.0; 3])
    };

    Ok(MghHeader {
        dims,
        data_type,
        spacing,
        affine: affine(&dims, &spacing, &directions, &center),
        voxel_count,
    })
}

/// Builds the voxel-to-RAS affine. `center` is the world position of voxel
/// `dims / 2`, matching FreeSurfer and nibabel.
fn affine(
    dims: &[usize; 3],
    spacing: &[f32; 3],
    directions: &[[f64; 3]; 3],
    center: &[f64; 3],
) -> [[f64; 4]; 4] {
    let mut affine = [[0.0; 4]; 4];
    for world_axis in 0..3 {
        for voxel_axis in 0..3 {
            affine[world_axis][voxel_axis] =
                directions[voxel_axis][world_axis] * f64::from(spacing[voxel_axis]);
        }
        affine[world_axis][3] = center[world_axis]
            - (0..3)
                .map(|voxel_axis| affine[world_axis][voxel_axis] * dims[voxel_axis] as f64 / 2.0)
                .sum::<f64>();
    }
    affine[3][3] = 1.0;
    affine
}

/// Reads the first frame. The reader must be positioned right after the header.
///
/// Data is decoded in fixed-size chunks so memory grows with the bytes actually
/// present; a header claiming more voxels than the file holds fails as
/// truncated instead of allocating the claimed size up front.
pub(crate) fn read_first_frame(reader: &mut impl Read, header: &MghHeader) -> Result<Vec<f32>> {
    const CHUNK_VOXELS: usize = 1 << 16;
    let size = header.data_type.size();
    let decode: fn(&[u8]) -> f32 = match header.data_type {
        MghType::Uchar => |bytes| f32::from(bytes[0]),
        MghType::Short => |bytes| f32::from(i16::from_be_bytes([bytes[0], bytes[1]])),
        MghType::Ushort => |bytes| f32::from(u16::from_be_bytes([bytes[0], bytes[1]])),
        MghType::Int => |bytes| i32::from_be_bytes(bytes.try_into().unwrap()) as f32,
        MghType::Float => |bytes| f32::from_be_bytes(bytes.try_into().unwrap()),
    };
    let mut values = Vec::new();
    let mut chunk = vec![0_u8; CHUNK_VOXELS.min(header.voxel_count) * size];
    let mut remaining = header.voxel_count;
    while remaining > 0 {
        let count = remaining.min(CHUNK_VOXELS);
        let bytes = &mut chunk[..count * size];
        reader.read_exact(bytes).map_err(truncated)?;
        values.extend(bytes.chunks_exact(size).map(decode));
        remaining -= count;
    }
    Ok(values)
}

fn truncated(error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        Error::InvalidMgh("file is truncated")
    } else {
        error.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_bytes(dims: [i32; 3], data_type: i32, good_ras: i16) -> Vec<u8> {
        let mut bytes = vec![0_u8; HEADER_SIZE];
        let mut put =
            |offset: usize, value: [u8; 4]| bytes[offset..offset + 4].copy_from_slice(&value);
        put(0, VERSION.to_be_bytes());
        for (axis, dimension) in dims.into_iter().enumerate() {
            put(4 + axis * 4, dimension.to_be_bytes());
        }
        put(16, 1_i32.to_be_bytes());
        put(20, data_type.to_be_bytes());
        // Conformed 1 mm LIA, as written by FreeSurfer's mri_convert --conform.
        let floats = [
            1.0_f32, 1.0, 1.0, -1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0,
        ];
        for (index, value) in floats.into_iter().enumerate() {
            put(30 + index * 4, value.to_be_bytes());
        }
        bytes[28..30].copy_from_slice(&good_ras.to_be_bytes());
        bytes
    }

    #[test]
    fn conformed_header_matches_freesurfer_affine() {
        let bytes = header_bytes([256, 256, 256], 0, 1);
        let header = read_header(&mut bytes.as_slice()).unwrap();
        assert_eq!(header.dims, [256; 3]);
        assert_eq!(header.data_type, MghType::Uchar);
        assert_eq!(
            header.affine,
            [
                [-1.0, 0.0, 0.0, 128.0],
                [0.0, 0.0, 1.0, -128.0],
                [0.0, -1.0, 0.0, 128.0],
                [0.0, 0.0, 0.0, 1.0],
            ]
        );
    }

    #[test]
    fn missing_ras_information_defaults_to_lia() {
        let mut bytes = header_bytes([4, 4, 4], 3, 0);
        // Garbage direction cosines must be ignored when goodRASFlag is unset.
        bytes[42..46].copy_from_slice(&f32::NAN.to_be_bytes());
        let header = read_header(&mut bytes.as_slice()).unwrap();
        assert_eq!(header.spacing, [1.0; 3]);
        assert_eq!(header.affine[0], [-1.0, 0.0, 0.0, 2.0]);
        assert_eq!(header.affine[1], [0.0, 0.0, 1.0, -2.0]);
        assert_eq!(header.affine[2], [0.0, -1.0, 0.0, 2.0]);
    }

    #[test]
    fn reads_big_endian_first_frame() {
        let bytes = header_bytes([2, 1, 1], 4, 1);
        let header = read_header(&mut bytes.as_slice()).unwrap();
        let data = [0xff_u8, 0xfe, 0x01, 0x00, 0xaa, 0xbb];
        assert_eq!(
            read_first_frame(&mut data.as_slice(), &header).unwrap(),
            [-2.0, 256.0]
        );
    }

    #[test]
    fn rejects_unknown_types_versions_and_truncation() {
        let bytes = header_bytes([2, 2, 2], 7, 1);
        assert!(matches!(
            read_header(&mut bytes.as_slice()),
            Err(Error::UnsupportedMghType(7))
        ));

        let mut bytes = header_bytes([2, 2, 2], 0, 1);
        bytes[0..4].copy_from_slice(&2_i32.to_be_bytes());
        assert!(matches!(
            read_header(&mut bytes.as_slice()),
            Err(Error::InvalidMgh(_))
        ));

        let bytes = header_bytes([2, 2, 2], 0, 1);
        let header = read_header(&mut bytes.as_slice()).unwrap();
        assert!(matches!(
            read_first_frame(&mut [0_u8; 7].as_slice(), &header),
            Err(Error::InvalidMgh(_))
        ));
        assert!(matches!(
            read_header(&mut [0_u8; 10].as_slice()),
            Err(Error::InvalidMgh(_))
        ));
    }
}
