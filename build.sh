#!/usr/bin/env bash
set -euo pipefail

TARGET_ARM="aarch64-apple-darwin"
TARGET_INTEL="x86_64-apple-darwin"
BINARY="docker-adapter"
OUT_DIR="dist"

rustup target add "$TARGET_ARM"
rustup target add "$TARGET_INTEL"

mkdir -p "$OUT_DIR"

echo "Building for macOS ARM ($TARGET_ARM)..."
cargo build --release --target "$TARGET_ARM"
cp "target/$TARGET_ARM/release/$BINARY" "$OUT_DIR/${BINARY}-aarch64-apple-darwin"

echo "Building for macOS Intel ($TARGET_INTEL)..."
cargo build --release --target "$TARGET_INTEL"
cp "target/$TARGET_INTEL/release/$BINARY" "$OUT_DIR/${BINARY}-x86_64-apple-darwin"

echo "Creating universal binary..."
lipo -create -output "$OUT_DIR/$BINARY-apple-darwin" \
  "$OUT_DIR/${BINARY}-aarch64-apple-darwin" \
  "$OUT_DIR/${BINARY}-x86_64-apple-darwin"

echo ""
echo "Done! Binaries in $OUT_DIR/:"
ls -lh "$OUT_DIR/"
lipo -info "$OUT_DIR/$BINARY-apple-darwin"
