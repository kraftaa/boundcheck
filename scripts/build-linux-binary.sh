#!/bin/sh
# Build a Linux boundarycheck binary (for --isolation docker on macOS, or for
# images older than the host). Uses Docker; the result is
# target/linux-<arch>/release/boundarycheck, built against Debian bookworm's
# glibc 2.36 so it runs in bookworm-or-newer images.
set -eu
cd "$(dirname "$0")/.."
arch=$(uname -m)
mkdir -p "target/linux-$arch" target/linux-cargo-registry
# Build output and the crate cache stay on the host (container disks are often small).
docker run --rm -v "$PWD":/src -w /src \
  -v "$PWD/target/linux-cargo-registry":/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR="/src/target/linux-$arch" \
  rust:1-bookworm cargo build --release --locked
echo "built target/linux-$arch/release/boundarycheck"
