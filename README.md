# Package Tracker

A fast, native Linux desktop app (Rust + [iced](https://iced.rs)) that shows everything installed on your system across package managers:

- **Lists** all packages, desktop apps, runtimes and (optionally) libraries
- **Uninstall** with a dry-run preview of what else would be removed (APT)
- **Update** a package when a newer version is available
- **Used by**: reverse dependencies, Flatpak apps using a runtime, snaps using a base, and **running processes**
- **Install new software**: one search across every source, with privacy flags, sandbox permissions and a list of extra packages it pulls in, all shown before you install
- **Suggestions tab**: apps installed twice (and which copy to keep), safe cleanups (unused dependencies, leftover configs, old Snap revisions, data of removed apps), and for each tracker whether to uninstall it or turn it off, with one-click fixes
- **Removal verdict** on every package: Safe to remove / Remove with care / Don't remove, with the reasons
- **Network**: live outgoing connections per program, with tracking/analytics servers flagged, plus ports open to your network
- **Startup**: login apps and system/user services with on/off switches and advice on what's safe to turn off
- **Sources**: APT repositories and PPAs, Flatpak remotes and more; flags PPAs built for another release, unused repositories, and packages that no repository updates
- **History**: everything installed, updated and removed (APT, pacman, Snap, DNF) with reinstall / undo / roll back, plus export and import of your app list for a new PC
- **Update all** with one password prompt, and an optional daily update notification
- **Flatpak permission switches** (internet, files, devices, sound, Bluetooth, X11)
- **Privacy & telemetry**: known telemetry components, crash reporters among dependencies, sandbox network access, and whether reporting services are active

## Download

Get the latest version from the [Releases page](https://github.com/pkg-tracker/pkgtrack/releases/latest):

- **`.deb`** (Debian, Ubuntu, Mint, Pop!_OS): `sudo apt install ./pkgtrack_*_amd64.deb`
- **`.rpm`** (Fedora, RHEL, openSUSE): `sudo dnf install ./pkgtrack-*.x86_64.rpm` (openSUSE: `sudo zypper install ./pkgtrack-*.x86_64.rpm`)
- **AppImage** (any distro): `chmod +x Package_Tracker-x86_64.AppImage && ./Package_Tracker-x86_64.AppImage`
- **Tarball** (any distro, no root): unpack, then run `./install.sh` to add it to your app menu

When a new version is released, the app tells you at startup. That check is a single request to the GitHub releases API; nothing about your computer is sent. Turn it off with **Check for app updates** in the sidebar, or set `PKGTRACK_NO_UPDATE_CHECK=1`.

## Supported sources

| Source  | Distros                         | List | Updates | Remove / Update |
|---------|---------------------------------|------|---------|-----------------|
| APT     | Debian, Ubuntu, Mint, Pop!_OS   | ✔    | ✔       | via `pkexec`    |
| RPM     | Fedora, RHEL, openSUSE (dnf/zypper) | ✔ | ✔       | via `pkexec`    |
| Pacman  | Arch, Manjaro, EndeavourOS      | ✔    | ✔       | via `pkexec` (update = full `-Syu`) |
| Flatpak | any                             | ✔    | ✔       | flatpak's own polkit |
| Snap    | any                             | ✔    | ✔       | via `pkexec`    |

Sources are detected at runtime, so a single binary works on any distro.

## Build & run

```sh
cargo build --release
./target/release/pkgtrack            # GUI
./target/release/pkgtrack --check firefox apport   # headless diagnostics
./target/release/pkgtrack --check-update            # is a newer release out?
./target/release/pkgtrack --search vlc             # headless search test
./target/release/pkgtrack --suggest                # headless suggestions scan
./target/release/pkgtrack --debug net|startup|sources|history|export|update-all
./install.sh                         # add to your app menu (~/.local, no root)
./packaging/build-deb.sh             # dist/pkgtrack_<version>_<arch>.deb
./packaging/build-rpm.sh             # dist/pkgtrack-<version>-1.<arch>.rpm (needs rpmbuild)
./packaging/build-appimage.sh        # dist/Package_Tracker-<arch>.AppImage (downloads appimagetool)
./install.sh --uninstall
```

Root actions use **polkit (`pkexec`)**, which shows a password dialog. The app itself never runs as root.

## About the telemetry detection

The telemetry check is a **heuristic**. Without auditing or monitoring the network traffic, there is no way to prove that a program doesn't send data. Findings come from a curated database ([src/telemetry.rs](src/telemetry.rs)), dependency hints and sandbox permissions. Contributions to the database are welcome.

## Releasing a new version

GitHub Actions builds and tests every push and pull request ([.github/workflows/build.yml](.github/workflows/build.yml)). To publish a release:

1. Bump `version` in `Cargo.toml` (for example `0.1.0` → `0.2.0`) and run `cargo build` so `Cargo.lock` updates.
2. Commit and push to `main`.

If no `v<version>` tag exists yet, the workflow tags the commit and publishes a release with the `.deb`, the `.rpm`, the AppImage, a tarball and `SHA256SUMS`. Pushes that don't change the version are built and tested, but not released.

## License

[MIT](LICENSE)
