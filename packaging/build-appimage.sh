#!/usr/bin/env bash
# Construye una AppImage portable de Local Library.
#
# Uso:
#   packaging/build-appimage.sh
#
# Variables opcionales:
#   BIN=target/release/local-library
#   OUT_DIR=dist
#   TOOLS=target/appimage-tools
#   LINUXDEPLOY=/ruta/linuxdeploy-x86_64.AppImage
#   APPIMAGETOOL=/ruta/appimagetool-x86_64.AppImage
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP_ID="local-library"
APP_NAME="Local Library"
ARCH="${ARCH:-x86_64}"
VERSION="${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}"
BIN="${BIN:-target/release/local-library}"
OUT_DIR="${OUT_DIR:-dist}"
TOOLS="${TOOLS:-target/appimage-tools}"
APPDIR="$OUT_DIR/AppDir"
LINUXDEPLOY="${LINUXDEPLOY:-$TOOLS/linuxdeploy-$ARCH.AppImage}"
APPIMAGETOOL="${APPIMAGETOOL:-$TOOLS/appimagetool-$ARCH.AppImage}"

if [[ ! -x "$BIN" ]]; then
    echo "==> Compilando $BIN"
    cargo build --release --bin "$APP_ID"
fi

mkdir -p "$TOOLS" "$OUT_DIR"

download_tool() {
    local url="$1" dest="$2"
    if [[ ! -x "$dest" ]]; then
        echo "==> Descargando $(basename "$dest")"
        curl -fL --retry 3 -o "$dest" "$url"
        chmod +x "$dest"
    fi
}

download_tool \
    "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-$ARCH.AppImage" \
    "$LINUXDEPLOY"
download_tool \
    "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage" \
    "$APPIMAGETOOL"

echo "==> Preparando AppDir"
rm -rf "$APPDIR"
mkdir -p \
    "$APPDIR/usr/bin" \
    "$APPDIR/usr/share/applications" \
    "$APPDIR/usr/share/icons/hicolor/scalable/apps"

install -m 0755 "$BIN" "$APPDIR/usr/bin/$APP_ID"
install -m 0644 "assets/$APP_ID.desktop" "$APPDIR/usr/share/applications/$APP_ID.desktop"
install -m 0644 "assets/$APP_ID.svg" "$APPDIR/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
cp "assets/$APP_ID.desktop" "$APPDIR/$APP_ID.desktop"
cp "assets/$APP_ID.svg" "$APPDIR/$APP_ID.svg"
ln -sf "$APP_ID.svg" "$APPDIR/.DirIcon"

cat > "$APPDIR/AppRun" <<'APPRUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
export LD_LIBRARY_PATH="$HERE/usr/lib:$HERE/usr/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$HERE/usr/bin/local-library" "$@"
APPRUN
chmod +x "$APPDIR/AppRun"

# Estos AppImage se ejecutan sin FUSE dentro de contenedores.
export APPIMAGE_EXTRACT_AND_RUN=1

echo "==> Resolviendo bibliotecas y lanzador"
"$LINUXDEPLOY" \
    --appdir "$APPDIR" \
    --executable "$APPDIR/usr/bin/$APP_ID" \
    --desktop-file "$APPDIR/usr/share/applications/$APP_ID.desktop" \
    --icon-file "$APPDIR/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"

# linuxdeploy puede sustituir el AppRun; se restaura el nuestro.
cat > "$APPDIR/AppRun" <<'APPRUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
export LD_LIBRARY_PATH="$HERE/usr/lib:$HERE/usr/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$HERE/usr/bin/local-library" "$@"
APPRUN
chmod +x "$APPDIR/AppRun"

OUTPUT="$OUT_DIR/LocalLibrary-${VERSION}-${ARCH}.AppImage"
echo "==> Generando $OUTPUT"
ARCH="$ARCH" "$APPIMAGETOOL" "$APPDIR" "$OUTPUT"
chmod +x "$OUTPUT"

echo
echo "AppImage creada: $OUTPUT"
echo "Pruébala con:    $OUTPUT --version"
