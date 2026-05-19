#!/bin/bash
set -e

INSTALL_BIN="${INSTALL_BIN:-$HOME/.local/bin}"
FEIBAI_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/feibai"

echo "[feibai] Building..."
cargo build --release

echo "[feibai] Installing binary to $INSTALL_BIN/"
mkdir -p "$INSTALL_BIN"
cp target/release/feibai-wl "$INSTALL_BIN/"

echo "[feibai] Installing dict to $FEIBAI_DIR/"
mkdir -p "$FEIBAI_DIR"
if [ ! -f "$FEIBAI_DIR/feibai.base.dict.yaml" ]; then
    cp data/dicts/feibai.base.dict.yaml "$FEIBAI_DIR/"
else
    echo "  (base dict already exists, skipping)"
fi

echo ""
echo "Done! Make sure $INSTALL_BIN is in your PATH."
echo ""
echo "Usage:"
echo "  feibai-wl"
echo ""
echo "Config: $FEIBAI_DIR/config.toml"
echo "Dicts:  $FEIBAI_DIR/*.dict.yaml"
