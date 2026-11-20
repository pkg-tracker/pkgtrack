#!/usr/bin/env sh
# Builds dist/Package_Tracker-<arch>.AppImage.
# Downloads appimagetool (from github.com/AppImage/appimagetool) on first run
# into packaging/.tools/ unless APPIMAGETOOL points to an existing copy.
set -e
cd "$(dirname "$0")/.."
ARCH=$(uname -m)
ID=io.github.pkgtrack
TOOL=${APPIMAGETOOL:-packaging/.tools/appimagetool-$ARCH.AppImage}
if [ ! -x "$TOOL" ]; then
  mkdir -p packaging/.tools
  echo "Downloading appimagetool…"
  curl -fL -o "$TOOL" "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
  chmod +x "$TOOL"
fi
APPDIR=$(mktemp -d)/PackageTracker.AppDir
trap 'rm -rf "$(dirname "$APPDIR")"' EXIT
cargo build --release
install -Dm755 target/release/pkgtrack "$APPDIR/usr/bin/pkgtrack"
install -Dm644 assets/$ID.desktop "$APPDIR/$ID.desktop"
install -Dm644 assets/$ID.desktop "$APPDIR/usr/share/applications/$ID.desktop"
install -Dm644 assets/pkgtrack-256.png "$APPDIR/$ID.png"
install -Dm644 assets/pkgtrack-256.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/$ID.png"
install -Dm644 assets/$ID.metainfo.xml "$APPDIR/usr/share/metainfo/$ID.appdata.xml"
cat > "$APPDIR/AppRun" <<'RUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/pkgtrack" "$@"
RUN
chmod +x "$APPDIR/AppRun"
mkdir -p dist
# Validate the metadata offline: appimagetool's own check also fetches every
# URL in it, which fails the build whenever GitHub is unreachable.
if command -v appstreamcli >/dev/null; then
  appstreamcli validate --no-net "assets/$ID.metainfo.xml"
fi
ARCH=$ARCH "$TOOL" --no-appstream "$APPDIR" "dist/Package_Tracker-$ARCH.AppImage"
echo "Built dist/Package_Tracker-$ARCH.AppImage"
