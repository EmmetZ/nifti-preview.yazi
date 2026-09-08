# nifti-preview.yazi

NIfTI-1 (`.nii` and `.nii.gz`) grayscale previewer for Yazi. It selects one
principal anatomical plane, resamples oblique acquisitions onto an upright RAS
grid, shows the middle slice initially, and uses `J`/`K` to move by one slice.
Axial and coronal images use the radiological convention (patient left appears
on screen right).

The current package contains a Linux x86_64 helper in `assets/` and does not
install anything into `PATH`.

## Package installation

Install it with:

```sh
ya pkg add EmmetZ/nifti-preview.yazi:nifti-preview
```

Upgrade it with:

```sh
ya pkg upgrade EmmetZ/nifti-preview.yazi:nifti-preview
```

Register it before generic gzip previewers in `yazi.toml`:

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

Route the default preview seek keys through the plugin in `keymap.toml`. For
non-NIfTI files these bindings forward to Yazi's original five-unit seek:

```toml
[mgr]
prepend_keymap = [
  { on = "K", run = "plugin nifti-preview -1", desc = "Previous NIfTI slice / seek preview up" },
  { on = "J", run = "plugin nifti-preview 1", desc = "Next NIfTI slice / seek preview down" },
]
```
