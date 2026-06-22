#!/bin/bash
set -e

INSTALL_BIN="${INSTALL_BIN:-$HOME/.local/bin}"
FEIBAI_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/feibai"
REPO="zhiyongjzy/feibai"
FORCE=0

usage() {
    echo "Usage: install.sh [OPTIONS]"
    echo ""
    echo "Options:"
    echo "  --from-source    Build from source (requires cargo)"
    echo "  --force-dicts    Overwrite existing dict files"
    echo "  -h, --help       Show this help"
}

FROM_SOURCE=0
for arg in "$@"; do
    case "$arg" in
        --from-source) FROM_SOURCE=1 ;;
        --force-dicts) FORCE=1 ;;
        -h|--help) usage; exit 0 ;;
    esac
done

# --- Install binary ---

if [ "$FROM_SOURCE" = 1 ]; then
    if ! command -v cargo &>/dev/null; then
        echo "[feibai] Error: cargo not found. Install Rust: https://rustup.rs"
        exit 1
    fi
    echo "[feibai] Building from source..."
    cargo build --release
    BINARY="target/release/feibai"
else
    echo "[feibai] Downloading latest release..."
    ARCH="$(uname -m)"
    case "$ARCH" in
        x86_64) ASSET="feibai-linux-x86_64" ;;
        aarch64) ASSET="feibai-linux-aarch64" ;;
        *) echo "[feibai] Error: unsupported architecture $ARCH"; exit 1 ;;
    esac

    DOWNLOAD_URL="https://github.com/$REPO/releases/latest/download/$ASSET"
    BINARY="/tmp/feibai-download"
    if command -v curl &>/dev/null; then
        curl -fSL "$DOWNLOAD_URL" -o "$BINARY"
    elif command -v wget &>/dev/null; then
        wget -q "$DOWNLOAD_URL" -O "$BINARY"
    else
        echo "[feibai] Error: curl or wget required"
        exit 1
    fi
    chmod +x "$BINARY"
fi

echo "[feibai] Installing binary to $INSTALL_BIN/"
mkdir -p "$INSTALL_BIN"
cp "$BINARY" "$INSTALL_BIN/feibai"

# --- Install dicts ---

echo "[feibai] Installing dicts to $FEIBAI_DIR/"
mkdir -p "$FEIBAI_DIR"

DICT_SOURCE="data/dicts"
if [ ! -d "$DICT_SOURCE" ]; then
    DICT_SOURCE="/tmp/feibai-dicts"
    mkdir -p "$DICT_SOURCE"
    for name in feibai.base.dict.yaml feibai.tech.dict.yaml feibai.extra.dict.yaml feibai.en.dict.yaml; do
        DICT_URL="https://github.com/$REPO/releases/latest/download/$name"
        if command -v curl &>/dev/null; then
            curl -fSL "$DICT_URL" -o "$DICT_SOURCE/$name"
        else
            wget -q "$DICT_URL" -O "$DICT_SOURCE/$name"
        fi
    done
fi

for dict in "$DICT_SOURCE"/*.dict.yaml; do
    name="$(basename "$dict")"
    if [ "$FORCE" = 1 ] || [ ! -f "$FEIBAI_DIR/$name" ]; then
        cp "$dict" "$FEIBAI_DIR/"
        echo "  installed $name"
    else
        echo "  $name exists (use --force-dicts to overwrite)"
    fi
done

# --- Runtime dependency check ---

if ! ldconfig -p 2>/dev/null | grep -q libxkbcommon; then
    echo ""
    echo "[feibai] Warning: libxkbcommon not found."
    echo "  Ubuntu/Debian: sudo apt install libxkbcommon0"
    echo "  Fedora:        sudo dnf install libxkbcommon"
fi

# --- IBus component (GNOME/KDE/X11) ---

FEIBAI_BIN="$(command -v feibai 2>/dev/null || echo "$INSTALL_BIN/feibai")"
IBUS_XML_SOURCE="data/feibai.xml"
if [ ! -f "$IBUS_XML_SOURCE" ]; then
    IBUS_XML_SOURCE="/tmp/feibai.xml"
    IBUS_XML_URL="https://github.com/$REPO/releases/latest/download/feibai.xml"
    if command -v curl &>/dev/null; then
        curl -fSL "$IBUS_XML_URL" -o "$IBUS_XML_SOURCE"
    else
        wget -q "$IBUS_XML_URL" -O "$IBUS_XML_SOURCE"
    fi
fi

IBUS_COMPONENT_DIR="/usr/share/ibus/component"
if [ -d "$IBUS_COMPONENT_DIR" ]; then
    echo "[feibai] Installing IBus component..."
    sed "s|<exec>.*</exec>|<exec>$FEIBAI_BIN --ibus</exec>|" "$IBUS_XML_SOURCE" | sudo tee "$IBUS_COMPONENT_DIR/feibai.xml" > /dev/null
    if command -v ibus &>/dev/null; then
        ibus write-cache 2>/dev/null || true
        ibus restart 2>/dev/null || true
    fi
else
    mkdir -p "$HOME/.local/share/ibus/component"
    sed "s|<exec>.*</exec>|<exec>$FEIBAI_BIN --ibus</exec>|" "$IBUS_XML_SOURCE" > "$HOME/.local/share/ibus/component/feibai.xml"
    if command -v ibus &>/dev/null; then
        ibus write-cache 2>/dev/null || true
        ibus restart 2>/dev/null || true
    fi
fi

# --- Done ---

echo ""
echo "Done! Make sure $INSTALL_BIN is in your PATH."
echo ""
echo "Next steps:"
if [ "$XDG_SESSION_TYPE" = "wayland" ] || [ -n "$WAYLAND_DISPLAY" ]; then
    echo "  Sway/Hyprland/COSMIC: exec feibai (in your compositor config)"
fi
echo "  GNOME/KDE: Settings > Keyboard > Input Sources > Add > Chinese > Feibai Pinyin"
echo ""
echo "Config: $FEIBAI_DIR/config.toml"
echo "Dicts:  $FEIBAI_DIR/*.dict.yaml"
