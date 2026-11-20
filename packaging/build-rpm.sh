#!/usr/bin/env sh
# Builds dist/pkgtrack-<version>-1.<arch>.rpm (needs: cargo, rpmbuild).
# Works on Fedora/openSUSE and on Debian/Ubuntu with the `rpm` package.
set -e
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(uname -m)
ID=io.github.pkgtrack
TOP=$(mktemp -d)
trap 'rm -rf "$TOP"' EXIT

cargo build --release
mkdir -p "$TOP/SPECS"
cat > "$TOP/SPECS/pkgtrack.spec" <<SPEC
# The binary is built (and stripped) by cargo; just package it.
%global debug_package %{nil}
%global _build_id_links none
%global __os_install_post %{nil}

Name:           pkgtrack
Version:        $VERSION
Release:        1
Summary:        See, update, install and remove packages and check them for telemetry
License:        MIT
URL:            https://github.com/pkg-tracker/pkgtrack
BuildArch:      $ARCH
AutoReqProv:    no
Requires:       glibc
Requires:       polkit
Recommends:     zenity

%description
A native desktop app that lists packages from APT, RPM, pacman, Flatpak
and Snap; shows what uses them, which updates are available, and which
software collects telemetry. Includes a live network monitor, startup
manager, repository overview and install history.

%install
cd "$PWD"
install -Dm755 target/release/pkgtrack %{buildroot}/usr/bin/pkgtrack
install -Dm644 assets/$ID.desktop %{buildroot}/usr/share/applications/$ID.desktop
install -Dm644 assets/pkgtrack.svg %{buildroot}/usr/share/icons/hicolor/scalable/apps/$ID.svg
for s in 32 48 64 128 256 512; do
  install -Dm644 assets/pkgtrack-\$s.png %{buildroot}/usr/share/icons/hicolor/\${s}x\${s}/apps/$ID.png
done
install -Dm644 assets/$ID.metainfo.xml %{buildroot}/usr/share/metainfo/$ID.metainfo.xml
install -Dm644 README.md %{buildroot}/usr/share/doc/pkgtrack/README.md
install -Dm644 LICENSE %{buildroot}/usr/share/licenses/pkgtrack/LICENSE

%files
/usr/bin/pkgtrack
/usr/share/applications/$ID.desktop
/usr/share/icons/hicolor/*/apps/$ID.*
/usr/share/metainfo/$ID.metainfo.xml
/usr/share/doc/pkgtrack
/usr/share/licenses/pkgtrack
SPEC

rpmbuild -bb --define "_topdir $TOP" "$TOP/SPECS/pkgtrack.spec"
mkdir -p dist
OUT="dist/pkgtrack-$VERSION-1.$ARCH.rpm"
cp "$TOP/RPMS/$ARCH/pkgtrack-$VERSION-1.$ARCH.rpm" "$OUT"
echo "Built $OUT"
