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

# Verify binary actually runs — catches glibc-too-old on older distros
if ldd "$INSTALL_BIN/feibai" 2>&1 | grep -q "not found"; then
    echo "[feibai] Error: binary has unresolved library dependencies (likely glibc too old)."
    ldd "$INSTALL_BIN/feibai" 2>&1 | grep "not found"
    echo "  Your glibc: $(ldd --version | head -1)"
    echo "  Reinstall from source: curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | bash -s -- --from-source"
    exit 1
fi

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
COMPONENT_INSTALLED=0
if [ -d "$IBUS_COMPONENT_DIR" ]; then
    echo "[feibai] Installing IBus component (needs sudo)..."
    if sed "s|<exec>.*</exec>|<exec>$FEIBAI_BIN --ibus</exec>|" "$IBUS_XML_SOURCE" | sudo tee "$IBUS_COMPONENT_DIR/feibai.xml" > /dev/null 2>&1; then
        COMPONENT_INSTALLED=1
    else
        echo "[feibai] Warning: sudo failed or denied. Install the component manually:"
        echo "  sed \"s|<exec>.*</exec>|<exec>$FEIBAI_BIN --ibus</exec>|\" \"$IBUS_XML_SOURCE\" | sudo tee \"$IBUS_COMPONENT_DIR/feibai.xml\""
    fi
else
    mkdir -p "$HOME/.local/share/ibus/component"
    sed "s|<exec>.*</exec>|<exec>$FEIBAI_BIN --ibus</exec>|" "$IBUS_XML_SOURCE" > "$HOME/.local/share/ibus/component/feibai.xml"
    COMPONENT_INSTALLED=1
    echo "[feibai] Note: installed IBus component to ~/.local/share/ibus/component/"
    echo "  If GNOME doesn't list Feibai, install to /usr/share with sudo instead."
fi

if [ "$COMPONENT_INSTALLED" = 1 ] && command -v ibus &>/dev/null; then
    ibus write-cache 2>/dev/null || true
    ibus restart 2>/dev/null || true
fi

# --- Auto-add input source on GNOME ---

GNOME_ADDED=0
if echo "${XDG_CURRENT_DESKTOP:-}" | grep -qi gnome && command -v gsettings >/dev/null 2>&1; then
    cur=$(gsettings get org.gnome.desktop.input-sources sources 2>/dev/null || true)
    if [ -n "$cur" ] && ! echo "$cur" | grep -q "'feibai'"; then
        if echo "$cur" | grep -qE '\[\s*\]'; then
            new=$(echo "$cur" | sed -E "s/\[\s*\]/[('ibus','feibai')]/")
        else
            new=$(echo "$cur" | sed -E "s/\]$/, ('ibus','feibai')]/")
        fi
        if [ -n "$new" ] && gsettings set org.gnome.desktop.input-sources sources "$new" 2>/dev/null; then
            GNOME_ADDED=1
            echo "[feibai] Added 'Feibai Pinyin' to GNOME input sources automatically."
        fi
    elif echo "$cur" | grep -q "'feibai'"; then
        GNOME_ADDED=1
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
if [ "$GNOME_ADDED" = 1 ]; then
    echo "  GNOME: Feibai Pinyin already in input sources — switch to it (Super+Space) and type."
else
    echo "  GNOME/KDE: Settings > Keyboard > Input Sources > + > Chinese > Feibai Pinyin"
fi
echo ""
echo "Config: $FEIBAI_DIR/config.toml"
echo "Dicts:  $FEIBAI_DIR/*.dict.yaml"
