//! Apps installed by hand: a menu entry whose program no package manager
//! owns and that isn't an AppImage, Flatpak or Snap. Typical sources are
//! `install.sh` scripts, `make install` and unpacked tarballs.
use super::appimage::{appimage_of, canonical, parse_desktop, refresh_menus};
use super::{run, which};
use crate::model::{birth_time, Details, Kind, Package, Source};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// Menu folders where hand-installed apps put their launchers: the user's
/// own, and /usr/local (never /usr/share, which belongs to packages).
fn menu_dirs() -> Vec<PathBuf> {
    let data_home = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("{}/.local/share", home()));
    [format!("{data_home}/applications"), "/usr/local/share/applications".into()]
        .into_iter()
        .map(PathBuf::from)
        .filter(|d| d.is_dir())
        .collect()
}

fn desktop_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in menu_dirs() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "desktop") && p.is_file() {
                out.push(p);
            }
        }
    }
    out
}

/// Program a launcher starts, as an absolute path, if it's a plain program
/// (not a wrapper such as flatpak, snap or a shell command).
fn program_of(f: &BTreeMap<String, String>) -> Option<String> {
    let exec = f.get("Exec")?;
    let mut parts = exec.split_whitespace().skip_while(|p| *p == "env" || (p.contains('=') && !p.starts_with('/')));
    let prog = parts.next()?.trim_matches('"').trim_matches('\'');
    const WRAPPERS: [&str; 12] =
        ["flatpak", "snap", "sh", "bash", "dash", "zsh", "python", "python3", "java", "gtk-launch", "xdg-open", "appimage-run"];
    let base = Path::new(prog).file_name()?.to_string_lossy().into_owned();
    if WRAPPERS.contains(&base.as_str()) {
        return None;
    }
    let path = if prog.starts_with('/') {
        PathBuf::from(prog)
    } else {
        std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(prog)).find(|p| p.is_file())?
    };
    path.is_file().then(|| path.to_string_lossy().into_owned())
}

/// Places that belong to Snap, Flatpak or AppImage mounts.
fn managed_elsewhere(path: &str) -> bool {
    let h = home();
    ["/snap/", "/var/lib/snapd/", "/var/lib/flatpak/", "/tmp/", "/usr/share/"].iter().any(|p| path.starts_with(p))
        || path.starts_with(&format!("{h}/.local/share/flatpak/"))
}

/// Does a package manager own this file?
fn owned_by_package(path: &str) -> bool {
    if which("dpkg") {
        // dpkg knows /bin/x as /usr/bin/x and the other way round.
        let alt = path.strip_prefix("/usr").map(String::from).unwrap_or_else(|| format!("/usr{path}"));
        return [path, alt.as_str()].iter().any(|p| run("dpkg", &["-S", p], &[]).is_ok());
    }
    if which("rpm") {
        return run("rpm", &["-qf", path], &[]).is_ok();
    }
    if which("pacman") {
        return run("pacman", &["-Qoq", path], &[]).is_ok();
    }
    false
}

/// A program in the user's home folder (removable without a password).
fn in_home(path: &str) -> bool {
    let h = home();
    !h.is_empty() && path.starts_with(&format!("{h}/"))
}

/// Why Package Tracker won't remove this app itself, if it won't. Only
/// single-file programs in the user's own bin folders are removed; apps that
/// live in a folder of their own have files we can't know about (and often
/// their own uninstaller), so deleting just the program would break them.
pub fn cant_remove(program: &str) -> Option<String> {
    let real = canonical(program);
    if !in_home(&real) {
        return Some(format!(
            "It was installed for all users ({real}). Remove it with its own uninstaller, or the way you installed it."
        ));
    }
    let h = home();
    let parent = Path::new(&real).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    if parent == format!("{h}/.local/bin") || parent == format!("{h}/bin") {
        return None;
    }
    Some(format!(
        "It lives in a folder of its own ({parent}) with other files. Remove it with its own uninstaller if it has one, \
         or delete its folder yourself."
    ))
}

pub fn list() -> Result<Vec<Package>> {
    let mut v: Vec<Package> = Vec::new();
    for p in desktop_files() {
        let Ok(raw) = std::fs::read_to_string(&p) else { continue };
        let f = parse_desktop(&raw);
        let flag = |k: &str| f.get(k).is_some_and(|v| v == "true");
        if f.get("Type").is_some_and(|t| t != "Application") || flag("Hidden") || flag("NoDisplay") {
            continue;
        }
        if appimage_of(&f).is_some() {
            continue;
        }
        let Some(prog) = program_of(&f) else { continue };
        let real = canonical(&prog);
        if managed_elsewhere(&real) || crate::backends::appimage::is_appimage_file(&real) {
            continue;
        }
        let launcher = p.to_string_lossy().into_owned();
        if let Some(x) = v.iter_mut().find(|x| canonical(&x.id) == real) {
            x.extra.push('\t');
            x.extra.push_str(&launcher);
            continue;
        }
        // A user's edited copy of a packaged app's launcher is not a new app.
        if !in_home(&real) && owned_by_package(&real) {
            continue;
        }
        v.push(Package {
            name: f.get("Name").cloned().unwrap_or_else(|| Path::new(&prog).file_name().unwrap().to_string_lossy().into_owned()),
            version: String::new(),
            source: Source::Manual,
            summary: f.get("Comment").cloned().filter(|c| !c.is_empty()).unwrap_or_else(|| "Installed by hand".into()),
            size_bytes: std::fs::metadata(&real).ok().map(|m| m.len()),
            kind: Kind::App,
            protected: false,
            extra: launcher,
            new_version: None,
            telemetry: None,
            installed_at: birth_time(&real),
            id: prog,
        });
    }
    Ok(v)
}

fn launchers(pkg: &Package) -> Vec<String> {
    pkg.extra.split('\t').filter(|l| !l.is_empty()).map(String::from).collect()
}

pub fn details(pkg: &Package) -> Details {
    let mut d = Details::default();
    d.location.push(pkg.id.clone());
    let real = canonical(&pkg.id);
    if real != pkg.id {
        d.location.push(real.clone());
    }
    d.location.extend(launchers(pkg));
    d.running = crate::procs::running_from_files(&[pkg.id.clone(), real]);
    d.info.push(("Type".into(), "Installed by hand (not from a package manager, AppImage, Flatpak or Snap)".into()));
    d.info.push(("Updates".into(), "Not updated automatically. Update it the same way you installed it.".into()));
    d
}

/// Icon files the app installed into the user's icon folders under its own
/// name, unless another remaining launcher uses the same icon.
fn own_icons(icon: &str, keep_launchers: &[String]) -> Vec<PathBuf> {
    let user_icons = format!("{}/.local/share/icons", home());
    if icon.starts_with('/') {
        return if icon.starts_with(&format!("{user_icons}/")) { vec![PathBuf::from(icon)] } else { Vec::new() };
    }
    if icon.is_empty() || icon.contains('/') {
        return Vec::new();
    }
    let shared = desktop_files().iter().any(|p| {
        !keep_launchers.contains(&p.to_string_lossy().into_owned())
            && std::fs::read_to_string(p).map(|r| parse_desktop(&r).get("Icon").is_some_and(|i| i == icon)).unwrap_or(false)
    });
    if shared {
        return Vec::new();
    }
    let mut out = Vec::new();
    fn walk(dir: &Path, name: &str, depth: u8, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && depth > 0 {
                walk(&p, name, depth - 1, out);
            } else if p.file_stem().is_some_and(|s| s == name) {
                out.push(p);
            }
        }
    }
    walk(Path::new(&user_icons), icon, 5, &mut out);
    out
}

pub fn remove(pkg: &Package) -> Result<String> {
    let real = canonical(&pkg.id);
    if let Some(why) = cant_remove(&pkg.id) {
        bail!("Package Tracker can't remove {} for you. {why}", pkg.name);
    }
    let ls = launchers(pkg);
    let trashed = Command::new("gio").args(["trash", &real]).status().map(|s| s.success()).unwrap_or(false);
    if !trashed {
        std::fs::remove_file(&real).with_context(|| format!("could not remove {real}"))?;
    }
    // A link to it (e.g. ~/.local/bin/app → ~/opt/app/app) goes too.
    if pkg.id != real && std::fs::symlink_metadata(&pkg.id).is_ok_and(|m| m.file_type().is_symlink()) {
        let _ = std::fs::remove_file(&pkg.id);
    }
    let mut touched = BTreeSet::new();
    let mut removed = 0;
    let metainfo = format!("{}/.local/share/metainfo", home());
    for l in &ls {
        let path = Path::new(l);
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Some(icon) = parse_desktop(&raw).get("Icon") {
                for i in own_icons(icon, &ls) {
                    let _ = std::fs::remove_file(i);
                }
            }
        }
        if let Some(stem) = path.file_stem() {
            let _ = std::fs::remove_file(format!("{metainfo}/{}.metainfo.xml", stem.to_string_lossy()));
        }
        if let Some(d) = path.parent() {
            touched.insert(d.to_path_buf());
        }
        if std::fs::remove_file(path).is_ok() {
            removed += 1;
        }
    }
    refresh_menus(&touched);
    Ok(format!(
        "{} was removed{}{}. Its settings in your home folder were kept.",
        pkg.name,
        if trashed { " (the program file is in the Trash)" } else { "" },
        match removed {
            0 => String::new(),
            1 => ", with its menu entry".into(),
            n => format!(", with its {n} menu entries"),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs() {
        let f = |exec: &str| {
            let mut m = BTreeMap::new();
            m.insert("Exec".to_string(), exec.to_string());
            program_of(&m)
        };
        assert_eq!(f("/bin/ls %U").as_deref(), Some("/bin/ls"));
        assert_eq!(f("env FOO=1 \"/bin/ls\"").as_deref(), Some("/bin/ls"));
        assert_eq!(f("flatpak run org.gimp.GIMP"), None);
        assert_eq!(f("sh -c 'echo hi'"), None);
        assert_eq!(f("/nonexistent/app"), None);
        assert!(managed_elsewhere("/snap/code/12/usr/bin/code"));
        assert!(!managed_elsewhere("/opt/foo/foo"));
    }
}
