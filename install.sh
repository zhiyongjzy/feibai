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
    FEIBAI_BIN="$(which feibai 2>/dev/null || echo "$INSTALL_BIN/feibai")"
    sed "s|<exec>.*</exec>|<exec>$FEIBAI_BIN --ibus</exec>|" data/feibai.xml | sudo tee "$IBUS_COMPONENT_DIR/feibai.xml" > /dev/null
    if command -v ibus &>/dev/null; then
        ibus write-cache
        ibus restart 2>/dev/null || true
        echo "  IBus: restarted. Select 'Feibai Pinyin' in input sources"
    fi
elif [ -d "$HOME/.local/share/ibus/component" ] || [ -d "$HOME/.local/share/ibus" ]; then
    mkdir -p "$HOME/.local/share/ibus/component"
    FEIBAI_BIN="$(which feibai 2>/dev/null || echo "$INSTALL_BIN/feibai")"
    sed "s|<exec>.*</exec>|<exec>$FEIBAI_BIN --ibus</exec>|" data/feibai.xml > "$HOME/.local/share/ibus/component/feibai.xml"
    if command -v ibus &>/dev/null; then
        ibus write-cache
        ibus restart 2>/dev/null || true
    fi
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
