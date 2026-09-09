#!/usr/bin/env bash
# Generate cargo-sources.json for Flatpak build
# This file is required for offline builds with Flatpak

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
OUTPUT="$SCRIPT_DIR/cargo-sources.json"

# Check for required tools
if ! command -v cargo &> /dev/null; then
    echo "Error: cargo not found" >&2
    exit 1
fi

if ! command -v flatpak-cargo-generator &> /dev/null; then
    echo "Warning: flatpak-cargo-generator not found"
    echo "Install from: https://github.com/nickvdyck/flatpak-cargo-generator"
    echo ""
    echo "Alternatively, you can use cargo2Flatpak:"
    echo "  pip3 install cargo2flatpak"
    echo "  cargo2flatpak --manifest-path=$PROJECT_ROOT/Cargo.lock --output=$OUTPUT"
    exit 1
fi

echo "Generating cargo-sources.json..."
flatpak-cargo-generator \
    --manifest-path="$PROJECT_ROOT/Cargo.lock" \
    --output="$OUTPUT"

echo "Generated: $OUTPUT"
echo ""
echo "Next steps:"
echo "1. Review the generated file"
echo "2. Run: flatpak-builder --force-clean build-dir flatpak/io.github.makis_san.Rocker.yml"
