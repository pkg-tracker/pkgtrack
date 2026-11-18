//! AppImages: single-file apps. "Installing" one means keeping it in
//! ~/Applications, extracting its icon and launcher, and adding it to the app
//! menu, so it behaves like any other installed app (and can be uninstalled).
use crate::model::{birth_time, Details, Kind, Package, Source};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const MARK: &str = "X-AppImage-Path";

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}
fn apps_dir() -> String {
    format!("{}/Applications", home())
}
fn desktop_dir() -> String {
    format!("{}/.local/share/applications", home())
}
fn icon_dir() -> String {
    format!("{}/.local/share/icons/appimages", home())
}

/// Information read from an AppImage before installing it.
#[derive(Debug, Clone)]
pub struct Info {
    /// Staged copy (executable) that will be moved into ~/Applications.
    pub staged: String,
    pub original: String,
    pub name: String,
    pub comment: String,
    pub version: String,
    pub categories: String,
    pub size: u64,
    pub icon: Option<String>,
    pub desktop: BTreeMap<String, String>,
    pub desktop_raw: String,
    pub needs_fuse2: bool,
    pub warning: Option<String>,
}

fn slug(s: &str) -> String {
    let s: String = s.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

fn parse_desktop(raw: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let mut main = false;
    for l in raw.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            main = t == "[Desktop Entry]";
        } else if main {
            if let Some((k, v)) = t.split_once('=') {
                m.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
            }
        }
    }
    m
}

/// AppImage type from the magic bytes ("AI\x02" at offset 8 = type 2).
fn appimage_type(path: &str) -> Result<u8> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).with_context(|| format!("cannot open {path}"))?;
    let mut head = [0u8; 11];
    f.read_exact(&mut head).context("file is too small")?;
    if &head[..4] != b"\x7fELF" {
        bail!("This is not an AppImage (not a Linux program).");
    }
    if &head[8..10] != b"AI" {
        bail!("This file doesn't look like an AppImage.");
    }
    Ok(head[10])
}

fn fuse2_available() -> bool {
    crate::backends::run("ldconfig", &["-p"], &[]).map(|s| s.contains("libfuse.so.2")).unwrap_or(true)
}

/// Run the AppImage's own extractor for a few files (it only unpacks; the app doesn't start).
fn extract(exe: &Path, workdir: &Path, pattern: &str) {
    let _ = Command::new("timeout")
        .args(["60", &exe.to_string_lossy(), "--appimage-extract", pattern])
        .current_dir(workdir)
        .env("APPIMAGE_EXTRACT_AND_RUN", "0")
        .output();
}

/// Root entries are often symlinks into usr/share/...; extract the targets too.
/// Returns the real file, if it could be extracted.
fn resolve(exe: &Path, work: &Path, rel: &str) -> Option<PathBuf> {
    let root = work.join("squashfs-root");
    let mut rel = rel.to_string();
    for _ in 0..4 {
        let p = root.join(&rel);
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => {
                let target = std::fs::read_link(&p).ok()?;
                let target = target.to_string_lossy().trim_start_matches("./").to_string();
                if target.starts_with('/') || target.contains("..") {
                    return None; // never follow links outside the image
                }
                let parent = Path::new(&rel).parent().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
                rel = if parent.is_empty() { target } else { format!("{parent}/{target}") };
                extract(exe, work, &rel);
            }
            Ok(m) if m.is_file() && m.len() > 0 => return Some(p),
            _ => return None,
        }
    }
    None
}

fn version_from_filename(file: &str) -> String {
    // e.g. "Obsidian-1.6.7.AppImage", "app-2.0.1-x86_64.AppImage"
    let stem = file.trim_end_matches(".AppImage").trim_end_matches(".appimage");
    stem.split(['-', '_'])
        .find(|p| p.chars().next().is_some_and(|c| c.is_ascii_digit()) && p.contains('.'))
        .unwrap_or("")
        .to_string()
}

pub fn inspect(path: &str) -> Result<Info> {
    let kind = appimage_type(path)?;
    let size = std::fs::metadata(path)?.len();
    let file = Path::new(path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let stage_dir = PathBuf::from(format!("{}/.cache/pkgtrack/staging/{}", home(), std::process::id()));
    let _ = std::fs::remove_dir_all(&stage_dir);
    std::fs::create_dir_all(&stage_dir)?;
    let staged = stage_dir.join(&file);
    std::fs::copy(path, &staged).context("could not copy the AppImage")?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    let mut info = Info {
        staged: staged.to_string_lossy().into_owned(),
        original: path.to_string(),
        name: file.trim_end_matches(".AppImage").split(['-', '_']).next().unwrap_or(&file).to_string(),
        comment: String::new(),
        version: version_from_filename(&file),
        categories: String::new(),
        size,
        icon: None,
        desktop: BTreeMap::new(),
        desktop_raw: String::new(),
        needs_fuse2: false,
        warning: None,
    };
    if kind != 2 {
        info.warning = Some("Old-style (type 1) AppImage: no icon or description could be read.".into());
        return Ok(info);
    }
    let work = stage_dir.join("x");
    std::fs::create_dir_all(&work)?;
    extract(&staged, &work, "*.desktop");
    let root = work.join("squashfs-root");
    let desktop = std::fs::read_dir(&root)
        .ok()
        .and_then(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).find(|n| n.ends_with(".desktop")))
        .and_then(|name| resolve(&staged, &work, &name));
    if let Some(d) = desktop {
        info.desktop_raw = std::fs::read_to_string(&d).unwrap_or_default();
        info.desktop = parse_desktop(&info.desktop_raw);
        if let Some(n) = info.desktop.get("Name") {
            info.name = n.clone();
        }
        info.comment = info.desktop.get("Comment").cloned().unwrap_or_default();
        info.categories = info.desktop.get("Categories").cloned().unwrap_or_default();
        if let Some(v) = info.desktop.get("X-AppImage-Version").filter(|v| !v.is_empty()) {
            info.version = v.clone();
        }
        // Icon: <Icon>.png / .svg at the root, or .DirIcon.
        let icon = info.desktop.get("Icon").cloned().unwrap_or_default();
        for cand in [format!("{icon}.png"), format!("{icon}.svg"), ".DirIcon".to_string()] {
            extract(&staged, &work, &cand);
            if let Some(p) = resolve(&staged, &work, &cand) {
                info.icon = Some(p.to_string_lossy().into_owned());
                break;
            }
        }
    } else {
        info.warning = Some("No launcher found inside; a basic one will be created.".into());
    }
    // Old runtimes need libfuse2; newer static runtimes don't.
    info.needs_fuse2 = !fuse2_available();
    Ok(info)
}

fn icon_ext(path: &str) -> &'static str {
    let head = std::fs::read(path).map(|b| b.into_iter().take(256).collect::<Vec<u8>>()).unwrap_or_default();
    if head.starts_with(b"\x89PNG") {
        "png"
    } else if String::from_utf8_lossy(&head).contains("<svg") || String::from_utf8_lossy(&head).contains("<?xml") {
        "svg"
    } else {
        "png"
    }
}

/// Install a previously inspected AppImage (no root needed).
pub fn install(info: &Info) -> Result<String> {
    std::fs::create_dir_all(apps_dir())?;
    std::fs::create_dir_all(desktop_dir())?;
    let file = Path::new(&info.staged).file_name().unwrap().to_string_lossy().into_owned();
    let target = format!("{}/{file}", apps_dir());
    if Path::new(&target).exists() {
        bail!("{target} already exists. Uninstall the old one first.");
    }
    if std::fs::rename(&info.staged, &target).is_err() {
        std::fs::copy(&info.staged, &target)?;
    }
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
    }
    let id = slug(&info.name);
    let icon = match &info.icon {
        Some(src) => {
            std::fs::create_dir_all(icon_dir())?;
            let dst = format!("{}/{id}.{}", icon_dir(), icon_ext(src));
            std::fs::copy(src, &dst)?;
            dst
        }
        None => "application-x-executable".into(),
    };
    let quoted = format!("\"{target}\"");
    let launcher = if info.needs_fuse2 { format!("env APPIMAGE_EXTRACT_AND_RUN=1 {quoted}") } else { quoted.clone() };
    // Rewrite the app's own launcher so it points to the AppImage.
    let mut out = Vec::new();
    if info.desktop_raw.is_empty() {
        out.push("[Desktop Entry]".to_string());
        out.push("Type=Application".into());
        out.push(format!("Name={}", info.name));
        out.push(format!("Exec={launcher} %U"));
    } else {
        for l in info.desktop_raw.lines() {
            let t = l.trim_start();
            if let Some(exec) = t.strip_prefix("Exec=") {
                // keep arguments such as %U, replace the program
                let args: Vec<&str> = exec.split_whitespace().skip(1).collect();
                out.push(format!("Exec={launcher}{}{}", if args.is_empty() { "" } else { " " }, args.join(" ")));
            } else if t.starts_with("TryExec=") || t.starts_with("Icon=") || t.starts_with("DBusActivatable=") || t.starts_with(MARK) {
                continue;
            } else {
                out.push(l.to_string());
            }
            if t == "[Desktop Entry]" {
                out.push(format!("Icon={icon}"));
                out.push(format!("TryExec={target}"));
                out.push(format!("{MARK}={target}"));
                if !info.version.is_empty() {
                    out.push(format!("X-AppImage-Version={}", info.version));
                }
            }
        }
    }
    if info.desktop_raw.is_empty() {
        out.push(format!("Icon={icon}"));
        out.push(format!("{MARK}={target}"));
    }
    let desktop = format!("{}/appimage-{id}.desktop", desktop_dir());
    std::fs::write(&desktop, out.join("\n") + "\n")?;
    let _ = Command::new("update-desktop-database").arg(desktop_dir()).output();
    let _ = std::fs::remove_dir_all(Path::new(&info.staged).parent().unwrap_or(Path::new("/nonexistent")));
    Ok(format!("{} was installed and added to your app menu.", info.name))
}

/// Path of the AppImage a desktop file launches, if any.
fn appimage_of(fields: &BTreeMap<String, String>) -> Option<String> {
    if let Some(p) = fields.get(MARK) {
        return Some(p.clone());
    }
    let exec = fields.get("Exec")?;
    let mut parts = exec.split_whitespace().skip_while(|p| *p == "env" || p.contains('='));
    let prog = parts.next()?.trim_matches('"').to_string();
    prog.to_lowercase().ends_with(".appimage").then_some(prog)
}

/// AppImages added to the app menu, plus loose ones in ~/Applications.
pub fn list() -> Result<Vec<Package>> {
    let mut v: Vec<Package> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(desktop_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "desktop") {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(&p) else { continue };
            let f = parse_desktop(&raw);
            let Some(img) = appimage_of(&f) else { continue };
            if !Path::new(&img).exists() || v.iter().any(|x| x.id == img) {
                continue;
            }
            let file = Path::new(&img).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            v.push(Package {
                name: f.get("Name").cloned().unwrap_or_else(|| file.clone()),
                version: f.get("X-AppImage-Version").cloned().unwrap_or_else(|| version_from_filename(&file)),
                source: Source::AppImage,
                summary: f.get("Comment").cloned().unwrap_or_default(),
                size_bytes: std::fs::metadata(&img).ok().map(|m| m.len()),
                kind: Kind::App,
                protected: false,
                extra: p.to_string_lossy().into_owned(),
                new_version: None,
                telemetry: None,
                installed_at: birth_time(&img),
                id: img,
            });
        }
    }
    if let Ok(rd) = std::fs::read_dir(apps_dir()) {
        for e in rd.flatten() {
            let img = e.path().to_string_lossy().into_owned();
            if !img.to_lowercase().ends_with(".appimage") || v.iter().any(|x| x.id == img) {
                continue;
            }
            let file = e.file_name().to_string_lossy().into_owned();
            v.push(Package {
                name: file.trim_end_matches(".AppImage").to_string(),
                version: version_from_filename(&file),
                source: Source::AppImage,
                summary: "Not in the app menu".into(),
                size_bytes: e.metadata().ok().map(|m| m.len()),
                kind: Kind::App,
                protected: false,
                extra: String::new(),
                new_version: None,
                telemetry: None,
                installed_at: birth_time(&img),
                id: img,
            });
        }
    }
    Ok(v)
}

pub fn details(pkg: &Package) -> Details {
    let mut d = Details::default();
    d.location.push(pkg.id.clone());
    if !pkg.extra.is_empty() {
        d.location.push(pkg.extra.clone());
    }
    // AppImages run from a temporary mount; the runtime exports $APPIMAGE.
    let needle = format!("APPIMAGE={}", pkg.id);
    d.running = crate::procs::running_matching(|exe, _| exe.starts_with("/tmp/.mount_") || exe == pkg.id)
        .into_iter()
        .filter(|(pid, _)| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .map(|b| b.split(|c| *c == 0).any(|kv| kv == needle.as_bytes()))
                .unwrap_or(false)
        })
        .collect();
    d.info.push(("Type".into(), "AppImage (single-file app, not sandboxed)".into()));
    d.info.push(("Updates".into(), "AppImages don't update through your package manager. Download new versions from the app's website.".into()));
    d
}

pub fn remove(pkg: &Package) -> Result<String> {
    let trashed = Command::new("gio").args(["trash", &pkg.id]).status().map(|s| s.success()).unwrap_or(false);
    if !trashed {
        std::fs::remove_file(&pkg.id).with_context(|| format!("could not remove {}", pkg.id))?;
    }
    if !pkg.extra.is_empty() {
        if let Ok(raw) = std::fs::read_to_string(&pkg.extra) {
            // Remove the icon only if we created it.
            if let Some(icon) = parse_desktop(&raw).get("Icon").filter(|i| i.starts_with(&icon_dir())) {
                let _ = std::fs::remove_file(icon);
            }
        }
        let _ = std::fs::remove_file(&pkg.extra);
        let _ = Command::new("update-desktop-database").arg(desktop_dir()).output();
    }
    Ok(format!(
        "{} was removed{}. Its settings in your home folder were kept.",
        pkg.name,
        if trashed { " (the AppImage file is in the Trash)" } else { "" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(version_from_filename("Obsidian-1.6.7.AppImage"), "1.6.7");
        assert_eq!(version_from_filename("app_2.0.1-x86_64.AppImage"), "2.0.1");
        assert_eq!(slug("My Cool App 2"), "my-cool-app-2");
        let f = parse_desktop("[Desktop Entry]\nName=X\nExec=env FOO=1 \"/home/u/Applications/X.AppImage\" %U\n");
        assert_eq!(appimage_of(&f).as_deref(), Some("/home/u/Applications/X.AppImage"));
        assert!(appimage_type("/bin/sh").is_err());
    }
}
