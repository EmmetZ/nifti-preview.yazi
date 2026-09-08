#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
asset="$root/nifti-preview.yazi/assets/yazi-nifti-preview"

cargo build --manifest-path "$root/Cargo.toml" --release --locked
mkdir -p "$(dirname -- "$asset")"
cp "$root/target/release/yazi-nifti-preview" "$asset"
chmod 755 "$asset"
