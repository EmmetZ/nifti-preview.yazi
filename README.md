# yazi-nifti-preview

Self-contained NIfTI-1 (`.nii` and `.nii.gz`) preview plugin for Yazi. The
bundled Rust helper renders an upright RAS image, supports `J`/`K` slice
navigation, and does not require a separate executable in `PATH`.

## Install

```sh
ya pkg add EmmetZ/nifti-preview.yazi:nifti-preview
```

## Configure

Register the previewer before generic gzip rules in `yazi.toml`:

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

## Releases

Pushing an annotated semantic-version tag triggers the GitHub Actions release
workflow. It tests the locked dependency graph, builds the Linux x86_64 helper,
and attaches a compressed binary plus `SHA256SUMS` to the GitHub Release.

```sh
git tag -a v0.1.0 -m "v0.1.0"
git push origin v0.1.0
```
