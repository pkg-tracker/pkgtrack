#!/usr/bin/env sh
# Builds dist/pkgtrack_<version>_<arch>.deb (needs: cargo, dpkg-deb).
set -e
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(dpkg --print-architecture)
ID=io.github.pkgtrack
ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT

cargo build --release
install -Dm755 target/release/pkgtrack "$ROOT/usr/bin/pkgtrack"
install -Dm644 assets/$ID.desktop "$ROOT/usr/share/applications/$ID.desktop"
install -Dm644 assets/pkgtrack.svg "$ROOT/usr/share/icons/hicolor/scalable/apps/$ID.svg"
for s in 32 48 64 128 256 512; do
  install -Dm644 assets/pkgtrack-$s.png "$ROOT/usr/share/icons/hicolor/${s}x${s}/apps/$ID.png"
done
install -Dm644 README.md "$ROOT/usr/share/doc/pkgtrack/README.md"
SIZE=$(du -sk "$ROOT/usr" | cut -f1)
mkdir -p "$ROOT/DEBIAN"
cat > "$ROOT/DEBIAN/control" <<CONTROL
Package: pkgtrack
Version: $VERSION
Architecture: $ARCH
Maintainer: Package Tracker developers
Installed-Size: $SIZE
Section: admin
Priority: optional
Depends: libc6, pkexec | policykit-1
Recommends: libxkbcommon0, libwayland-client0, libvulkan1 | libgl1, zenity, libnotify-bin
Description: See, update, install and remove packages and check them for telemetry
 A native desktop app that lists packages from APT, RPM, pacman, Flatpak
 and Snap; shows what uses them, which updates are available, and which
 software collects telemetry. Includes a live network monitor, startup
 manager, repository overview and install history.
CONTROL
mkdir -p dist
OUT="dist/pkgtrack_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group --build "$ROOT" "$OUT"
echo "Built $OUT"
