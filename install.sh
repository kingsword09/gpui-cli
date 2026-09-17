#!/usr/bin/env bash
set -euo pipefail

echo "=== Installing GPUI CLI ==="

# Check for Cargo / Rust
if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo is not installed. Please install Rust from https://rustup.rs/"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Building and installing gpui binary to cargo bin path..."
cargo install --path "$SCRIPT_DIR" --force --locked

echo ""
echo "✨ Installation successful!"
echo "Run 'gpui --help' or 'gpui init' to get started."
