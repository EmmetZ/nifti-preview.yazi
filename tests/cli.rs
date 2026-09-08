use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::Value;
use std::fs;
use std::io::Write;
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
        assert_eq!(pixels.get_pixel(0, 2).0[0], 133);
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
