//! When was an app last opened?
//!
//! GNOME Shell records the last time each app was used in
//! ~/.local/share/gnome-shell/application_state. On other desktops we fall
//! back to the access time of the package's executables (with `relatime`
//! mounts the kernel updates it at most once a day, which is enough to tell
//! "used this week" from "unused for months").
use crate::model::{Package, Source};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// desktop file id (e.g. "firefox_firefox.desktop") -> last seen (unix time)
fn gnome_state() -> &'static HashMap<String, u64> {
    static STATE: OnceLock<HashMap<String, u64>> = OnceLock::new();
    STATE.get_or_init(|| {
        let mut m = HashMap::new();
        let Ok(home) = std::env::var("HOME") else { return m };
        let Ok(xml) = std::fs::read_to_string(format!("{home}/.local/share/gnome-shell/application_state")) else {
            return m;
        };
        for line in xml.lines() {
            let attr = |name: &str| -> Option<&str> {
                let start = line.find(&format!("{name}=\""))? + name.len() + 2;
                let end = line[start..].find('"')? + start;
                Some(&line[start..end])
            };
            if let (Some(id), Some(seen)) = (attr("id"), attr("last-seen").and_then(|s| s.parse::<u64>().ok())) {
                m.insert(id.to_string(), seen);
            }
        }
        m
    })
}

pub fn has_gnome_state() -> bool {
    !gnome_state().is_empty()
}

/// Seconds since the app was last opened, if known.
pub fn last_used(pkg: &Package, desktop_files: &[String]) -> Option<u64> {
    let state = gnome_state();
    let mut best: Option<u64> = None;
    let mut consider = |t: u64| best = Some(best.map_or(t, |b: u64| b.max(t)));
    match pkg.source {
        Source::Snap => {
            let prefix = format!("{}_", pkg.id);
            state.iter().filter(|(k, _)| k.starts_with(&prefix)).for_each(|(_, &t)| consider(t));
        }
        Source::Flatpak => {
            if let Some(&t) = state.get(&format!("{}.desktop", pkg.id)) {
                consider(t);
            }
        }
        _ => {
            for f in desktop_files {
                let id = f.rsplit('/').next().unwrap_or(f);
                if let Some(&t) = state.get(id) {
                    consider(t);
                }
            }
        }
    }
    best.map(|t| now().saturating_sub(t))
}

/// Access-time based fallback for native packages.
pub fn last_access(files: &[String]) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    files
        .iter()
        .filter(|f| f.starts_with("/usr/bin/") || f.starts_with("/usr/games/") || f.starts_with("/opt/"))
        .filter_map(|f| std::fs::metadata(f).ok())
        .filter(|m| m.is_file())
        .map(|m| m.atime().max(0) as u64)
        .max()
        .map(|t| now().saturating_sub(t))
}

/// How long ago a native package was installed or last upgraded.
pub fn installed_ago(pkg: &Package) -> Option<u64> {
    let path = match pkg.source {
        Source::Apt => format!("/var/lib/dpkg/info/{}.list", pkg.id),
        Source::Snap => format!("/snap/{}/current", pkg.id),
        Source::Flatpak => format!("/var/lib/flatpak/app/{}", pkg.id),
        _ => return None,
    };
    let alt = format!("/var/lib/dpkg/info/{}.list", pkg.id.split(':').next().unwrap_or(&pkg.id));
    let m = std::fs::metadata(&path).or_else(|_| std::fs::metadata(alt)).ok()?;
    let t = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(now().saturating_sub(t))
}

fn newest_mtime(dir: &std::path::Path, depth: u32) -> Option<u64> {
    let meta = std::fs::symlink_metadata(dir).ok()?;
    let mut best = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    if depth > 0 && meta.is_dir() {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten().take(200) {
                if let Some(t) = newest_mtime(&e.path(), depth - 1) {
                    best = best.max(t);
                }
            }
        }
    }
    Some(best)
}

/// For sandboxed apps: the per-user data folder appears the first time the app
/// runs and changes while it's used. Some(None) = never started.
pub fn data_dir_age(pkg: &Package) -> Option<Option<u64>> {
    let home = std::env::var("HOME").ok()?;
    let dir = match pkg.source {
        Source::Snap => format!("{home}/snap/{}", pkg.id),
        Source::Flatpak => format!("{home}/.var/app/{}", pkg.id),
        _ => return None,
    };
    let path = std::path::Path::new(&dir);
    if !path.exists() {
        return Some(None);
    }
    Some(newest_mtime(path, 3).map(|t| now().saturating_sub(t)))
}

/// Does the package have a launcher that shows in the app menu?
pub fn has_visible_launcher(pkg: &Package, desktop_files: &[String]) -> bool {
    let visible = |path: &str| {
        std::fs::read_to_string(path).is_ok_and(|c| !c.lines().any(|l| l.trim() == "NoDisplay=true" || l.trim() == "Hidden=true"))
    };
    match pkg.source {
        Source::Snap => std::fs::read_dir("/var/lib/snapd/desktop/applications").is_ok_and(|rd| {
            rd.flatten().any(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with(&format!("{}_", pkg.id)) && visible(&e.path().to_string_lossy())
            })
        }),
        Source::Flatpak => true,
        _ => desktop_files.iter().any(|f| visible(f)),
    }
}

pub fn human_ago(secs: u64) -> String {
    let days = secs / 86_400;
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..=13 => format!("{days} days ago"),
        14..=59 => format!("{} weeks ago", days / 7),
        60..=729 => format!("{} months ago", days / 30),
        _ => format!("{} years ago", days / 365),
    }
}
