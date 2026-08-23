#!/bin/sh
# SPDX-License-Identifier: MPL-2.0

set -eu

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../../.." && pwd)
artifact="$repository_root/crates/blitz-runtime/assets/runtime/v1/runtime.blitzbundle"
temporary_directory=$(mktemp -d)
saved_artifact="$temporary_directory/runtime.blitzbundle"
generated_artifact="$temporary_directory/generated.blitzbundle"
bootstrap_target="$temporary_directory/target"

restore() {
  if [ -f "$saved_artifact" ]; then
    mv "$saved_artifact" "$artifact"
  fi
  rm -rf "$temporary_directory"
}
trap restore EXIT HUP INT TERM

mv "$artifact" "$saved_artifact"
CARGO_TARGET_DIR="$bootstrap_target" cargo run \
  --manifest-path "$repository_root/Cargo.toml" \
  --locked \
  -p blitz-cli \
  -- precompile runtime --output "$generated_artifact"
cmp "$saved_artifact" "$generated_artifact"
