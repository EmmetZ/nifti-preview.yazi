# nifti-preview.yazi

NIfTI-1 (`.nii`, `.nii.gz`) and FreeSurfer MGH (`.mgh`, `.mgz`, `.mgh.gz`)
grayscale previewer for Yazi. It resamples oblique acquisitions onto an upright
RAS grid, uses the radiological convention for axial and coronal images, and
supports `J`/`K` slice navigation.

The current package contains a Linux x86_64 helper in `assets/` and does not
install anything into `PATH`.

Yazi deploys package files as read-only. On Unix, the plugin restores the
bundled helper's user execute permission automatically when it first previews a
volume.

## Rendering

The helper computes one intensity window for the first 3D volume and reuses it
across slices. Following `run_ANT_reg.py`, it excludes non-finite values and
background (`abs(value) <= 1e-8`), then uses the 0.5th and 99.5th percentiles with
linear interpolation. If the window collapses, it falls back to tissue min/max,
then the full finite range so binary masks remain visible. Constant volumes
render black.

NIfTI intensity scaling is applied before storing float32 voxels. Zero or
non-finite slopes disable scaling; a non-finite intercept with a valid slope is
treated as zero. Extensions and padding are skipped using `vox_offset`.
Trilinear interpolation runs on float intensities before window clipping and
mapping through the 256-entry grayscale LUT. Invalid voxels and areas outside
the field of view render black.

The plugin keeps affine-aware radiological RAS orientation and physical voxel
spacing. The script's voxel-order flips, three-volume shared window, figure
layout, and DPI-dependent Matplotlib resampling are specific to its comparison
figures and are not used for single-file previews.

## Install

Install it with:

```sh
ya pkg add EmmetZ/nifti-preview.yazi:nifti-preview
```

## Configure

Register the previewer before generic gzip previewers in `yazi.toml`:

```toml
[plugin]
prepend_previewers = [
  { url = "*.nii", run = "nifti-preview" },
  { url = "*.nii.gz", run = "nifti-preview" },
  { url = "*.mgz", run = "nifti-preview" },
  { url = "*.mgh", run = "nifti-preview" },
  { url = "*.mgh.gz", run = "nifti-preview" },
]
prepend_preloaders = [
  { url = "*.nii", run = "nifti-preview" },
  { url = "*.nii.gz", run = "nifti-preview" },
  { url = "*.mgz", run = "nifti-preview" },
  { url = "*.mgh", run = "nifti-preview" },
  { url = "*.mgh.gz", run = "nifti-preview" },
]
```

Route `J` and `K` through the plugin in `keymap.toml`:

```toml
[mgr]
prepend_keymap = [
  { on = "K", run = "plugin nifti-preview -1", desc = "Previous volume slice / seek preview up" },
  { on = "J", run = "plugin nifti-preview 1", desc = "Next volume slice / seek preview down" },
]
```

## Upgrade

```sh
ya pkg upgrade EmmetZ/nifti-preview.yazi:nifti-preview
```
