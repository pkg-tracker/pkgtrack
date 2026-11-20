#!/usr/bin/env sh
# Installs Package Tracker for the current user (no root needed).
# Uninstall: ./install.sh --uninstall
set -e
cd "$(dirname "$0")"
BIN="$HOME/.local/bin"; SHARE="$HOME/.local/share"; ID=io.github.pkgtrack

if [ "$1" = "--uninstall" ]; then
  rm -f "$BIN/pkgtrack" "$SHARE/applications/$ID.desktop" "$SHARE/metainfo/$ID.metainfo.xml" "$SHARE/icons/hicolor/scalable/apps/$ID.svg"
  for s in 32 48 64 128 256 512; do rm -f "$SHARE/icons/hicolor/${s}x${s}/apps/$ID.png"; done
  echo "Package Tracker removed."; exit 0
fi

[ -x target/release/pkgtrack ] || cargo build --release
install -Dm755 target/release/pkgtrack "$BIN/pkgtrack"
install -Dm644 assets/$ID.desktop "$SHARE/applications/$ID.desktop"
sed -i "s|^Exec=.*|Exec=$BIN/pkgtrack|" "$SHARE/applications/$ID.desktop"
install -Dm644 assets/pkgtrack.svg "$SHARE/icons/hicolor/scalable/apps/$ID.svg"
install -Dm644 assets/$ID.metainfo.xml "$SHARE/metainfo/$ID.metainfo.xml"
for s in 32 48 64 128 256 512; do
  install -Dm644 assets/pkgtrack-$s.png "$SHARE/icons/hicolor/${s}x${s}/apps/$ID.png"
done
gtk-update-icon-cache -q "$SHARE/icons/hicolor" 2>/dev/null || true
update-desktop-database -q "$SHARE/applications" 2>/dev/null || true
echo "Installed. Launch 'Package Tracker' from your app menu."
