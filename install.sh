#!/bin/bash
set -e

INSTALL_BIN="${INSTALL_BIN:-$HOME/.local/bin}"
FEIBAI_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/feibai"

echo "[feibai] Building..."
cargo build --release

echo "[feibai] Installing binary to $INSTALL_BIN/"
mkdir -p "$INSTALL_BIN"
cp target/release/feibai "$INSTALL_BIN/"

echo "[feibai] Installing dict to $FEIBAI_DIR/"
mkdir -p "$FEIBAI_DIR"
if [ ! -f "$FEIBAI_DIR/feibai.base.dict.yaml" ]; then
    cp data/dicts/feibai.base.dict.yaml "$FEIBAI_DIR/"
else
    echo "  (base dict already exists, skipping)"
fi

# Install IBus component XML (for GNOME/KDE support)
IBUS_COMPONENT_DIR="/usr/share/ibus/component"
if [ -d "$IBUS_COMPONENT_DIR" ]; then
    echo "[feibai] Installing IBus component (requires sudo)..."
    sudo cp data/feibai.xml "$IBUS_COMPONENT_DIR/"
    echo "  IBus: restart ibus-daemon and select 'Feibai Pinyin' in settings"
elif [ -d "$HOME/.local/share/ibus/component" ]; then
    mkdir -p "$HOME/.local/share/ibus/component"
    cp data/feibai.xml "$HOME/.local/share/ibus/component/"
else
    echo "  (IBus not found, skipping IBus registration)"
    echo "  For GNOME support, manually copy data/feibai.xml to /usr/share/ibus/component/"
fi

echo ""
echo "Done! Make sure $INSTALL_BIN is in your PATH."
echo ""
echo "Usage:"
echo "  Sway/COSMIC/Hyprland: feibai"
echo "  GNOME/KDE:            select 'Feibai Pinyin' in input sources"
echo ""
echo "Config: $FEIBAI_DIR/config.toml"
echo "Dicts:  $FEIBAI_DIR/*.dict.yaml"
