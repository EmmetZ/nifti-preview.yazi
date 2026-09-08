# nifti-preview.yazi

NIfTI-1 (`.nii` and `.nii.gz`) grayscale previewer for Yazi. It resamples
oblique acquisitions onto an upright RAS grid, uses the radiological convention
for axial and coronal images, and supports `J`/`K` slice navigation.

The current package contains a Linux x86_64 helper in `assets/` and does not
install anything into `PATH`.

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
]
prepend_preloaders = [
  { url = "*.nii", run = "nifti-preview" },
  { url = "*.nii.gz", run = "nifti-preview" },
]
```

Route `J` and `K` through the plugin in `keymap.toml`:

```toml
[mgr]
prepend_keymap = [
  { on = "K", run = "plugin nifti-preview -1", desc = "Previous NIfTI slice / seek preview up" },
  { on = "J", run = "plugin nifti-preview 1", desc = "Next NIfTI slice / seek preview down" },
]
```

## Upgrade

```sh
ya pkg upgrade EmmetZ/nifti-preview.yazi:nifti-preview
```
