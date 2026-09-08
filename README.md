# yazi-nifti-preview

Rust helper source and release package for the `nifti-preview.yazi` Yazi plugin.

## Build the package

```sh
./scripts/build-package.sh
```

For local installation, copy only the release package:

```sh
cp -a nifti-preview.yazi ~/.config/yazi/plugins/
```

The package directory contains only the three files recognized by `ya pkg` and
the bundled executable under `assets/`. See its README for publication and
configuration instructions.

Register the plugin before generic gzip previewers:

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

The Rust crate remains outside the installed plugin directory.
