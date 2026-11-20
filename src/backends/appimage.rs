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
    /// An installed copy of the same app this one replaces: (path, version).
    pub replaces: Option<(String, String)>,
}

fn slug(s: &str) -> String {
    let s: String = s.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

pub(crate) fn parse_desktop(raw: &str) -> BTreeMap<String, String> {
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
        replaces: None,
    };
    if kind != 2 {
        info.warning = Some("Old-style (type 1) AppImage: no icon or description could be read.".into());
        info.replaces = installed_copy(&info);
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
    info.replaces = installed_copy(&info);
    Ok(info)
}

/// An installed AppImage of the same app (same menu name, or the same file
/// name apart from version and architecture), other than the file itself.
fn installed_copy(info: &Info) -> Option<(String, String)> {
    let me = canonical(&info.original);
    let file = Path::new(&info.original).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = file_stem_key(&file);
    list().ok()?.into_iter().find_map(|p| {
        let other = Path::new(&p.id).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let same = slug(&p.name) == slug(&info.name) || (!stem.is_empty() && file_stem_key(&other) == stem);
        (same && canonical(&p.id) != me).then_some((p.id, p.version))
    })
}

/// "Package_Tracker-0.1.2-x86_64.AppImage" → "package-tracker".
pub(crate) fn file_stem_key(file: &str) -> String {
    const ARCH: [&str; 7] = ["x86", "x86_64", "amd64", "i386", "i686", "aarch64", "arm64"];
    let stem = file.trim_end_matches(".AppImage").trim_end_matches(".appimage");
    let words: Vec<&str> = stem
        .split(['-', '_', '.', ' '])
        .take_while(|w| !w.is_empty() && !w.starts_with(|c: char| c.is_ascii_digit()) && !ARCH.contains(&w.to_lowercase().as_str()))
        .collect();
    slug(&words.join("-"))
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
    // Take the old version out first (file to the Trash, its menu entries and icon removed).
    let mut replaced = String::new();
    if let Some((old, ver)) = &info.replaces {
        if Path::new(old).exists() {
            let old_pkg = list()?.into_iter().find(|p| p.id == *old).context("the installed copy could not be found")?;
            remove(&old_pkg).context("could not remove the old version")?;
            replaced = if ver.is_empty() { " It replaced the old version (now in the Trash).".into() } else { format!(" It replaced version {ver} (now in the Trash).") };
        }
    }
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
    Ok(format!("{} was installed and added to your app menu.{replaced}", info.name))
}

/// Is this file an AppImage (by its magic bytes, whatever its name)?
fn is_appimage(path: &str) -> bool {
    appimage_type(path).is_ok()
}

pub(crate) fn is_appimage_file(path: &str) -> bool {
    Path::new(path).is_file() && is_appimage(path)
}

/// Real path of a file (follows symlinks); the path itself if that fails.
pub(crate) fn canonical(path: &str) -> String {
    std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_string())
}

/// Path of the AppImage a desktop file launches, if any.
pub(crate) fn appimage_of(fields: &BTreeMap<String, String>) -> Option<String> {
    if let Some(p) = fields.get(MARK) {
        return Some(p.clone());
    }
    let exec = fields.get("Exec")?;
    let mut parts = exec.split_whitespace().skip_while(|p| *p == "env" || p.contains('='));
    let prog = parts.next()?.trim_matches('"').trim_matches('\'').to_string();
    if prog.to_lowercase().ends_with(".appimage") {
        return Some(prog);
    }
    // Integration tools often rename the file (no extension): check its signature.
    (prog.starts_with('/') && Path::new(&prog).is_file() && is_appimage(&prog)).then_some(prog)
}

/// Every folder launchers can live in: the app menu (user and system),
/// login apps and the desktop.
fn launcher_dirs() -> Vec<PathBuf> {
    let h = home();
    let data_home = std::env::var("XDG_DATA_HOME").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| format!("{h}/.local/share"));
    let data_dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let mut dirs: Vec<PathBuf> = vec![PathBuf::from(format!("{data_home}/applications"))];
    dirs.extend(data_dirs.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(format!("{d}/applications"))));
    let config = std::env::var("XDG_CONFIG_HOME").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| format!("{h}/.config"));
    dirs.push(PathBuf::from(format!("{config}/autostart")));
    let desktop = crate::backends::run("xdg-user-dir", &["DESKTOP"], &[])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && *s != h)
        .unwrap_or_else(|| format!("{h}/Desktop"));
    dirs.push(PathBuf::from(desktop));
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| d.is_dir() && seen.insert(std::fs::canonicalize(d).unwrap_or_else(|_| d.clone())));
    dirs
}

/// All .desktop files in the launcher folders (and their subfolders).
fn launcher_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() && depth > 0 {
                walk(&p, depth - 1, out);
            } else if p.extension().is_some_and(|x| x == "desktop") {
                out.push(p);
            }
        }
    }
    let mut v = Vec::new();
    for d in launcher_dirs() {
        walk(&d, 3, &mut v);
    }
    v
}

/// Does `text` contain `path` as a whole path (not as the start of a longer name)?
fn mentions_path(text: &str, path: &str) -> bool {
    text.match_indices(path).any(|(i, _)| {
        let next = text[i + path.len()..].chars().next();
        !next.is_some_and(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '/'))
    })
}

/// Launchers that start the AppImage at any of `targets` (paths, canonical or not).
fn launchers_for(targets: &[String]) -> Vec<(PathBuf, BTreeMap<String, String>)> {
    launcher_files()
        .into_iter()
        .filter_map(|p| {
            let raw = std::fs::read_to_string(&p).ok()?;
            let f = parse_desktop(&raw);
            let hit = appimage_of(&f).is_some_and(|img| targets.contains(&img) || targets.contains(&canonical(&img)))
                // Wrappers like `sh -c "… /path/App.AppImage"` or `appimage-run /path/…`.
                || ["Exec", "TryExec"].iter().any(|k| f.get(*k).is_some_and(|v| targets.iter().any(|t| mentions_path(v, t))));
            hit.then_some((p, f))
        })
        .collect()
}

/// A launcher whose AppImage is gone (deleted or moved by hand, or left
/// behind by an older uninstall).
#[derive(Debug, Clone)]
pub struct OrphanLauncher {
    pub launcher: String,
    pub name: String,
    pub missing: String,
    /// In the user's home folder (no password needed to remove).
    pub user: bool,
}

/// Launchers that point to an AppImage that no longer exists. To avoid
/// false alarms, the folder it was in must still exist (so it isn't just an
/// unplugged drive), and the target must clearly be an AppImage: our marker,
/// an .AppImage name, or X-AppImage-* keys written by integration tools.
pub fn orphan_launchers() -> Vec<OrphanLauncher> {
    let h = home();
    launcher_files()
        .into_iter()
        .filter_map(|p| {
            let raw = std::fs::read_to_string(&p).ok()?;
            let f = parse_desktop(&raw);
            let exec_prog = || -> Option<String> {
                let exec = f.get("Exec")?;
                let mut parts = exec.split_whitespace().skip_while(|p| *p == "env" || p.contains('='));
                Some(parts.next()?.trim_matches('"').trim_matches('\'').to_string())
            };
            let target = f.get(MARK).cloned().or_else(|| {
                let prog = exec_prog()?;
                let tagged = f.keys().any(|k| k.starts_with("X-AppImage"));
                (prog.to_lowercase().ends_with(".appimage") || tagged).then_some(prog)
            })?;
            if !target.starts_with('/') || std::fs::symlink_metadata(&target).is_ok() {
                return None;
            }
            let parent = Path::new(&target).parent()?;
            if !parent.is_dir() {
                return None; // drive not mounted, or a whole folder moved: don't guess
            }
            let launcher = p.to_string_lossy().into_owned();
            Some(OrphanLauncher {
                name: f.get("Name").cloned().unwrap_or_else(|| p.file_stem().unwrap_or_default().to_string_lossy().into_owned()),
                user: launcher.starts_with(&h),
                launcher,
                missing: target,
            })
        })
        .collect()
}

/// AppImages added to the app menu (by us or any other tool), plus loose
/// ones in ~/Applications.
pub fn list() -> Result<Vec<Package>> {
    let mut v: Vec<Package> = Vec::new();
    // The user's app menu folder comes first, so its names win.
    for p in launcher_files() {
        let Ok(raw) = std::fs::read_to_string(&p) else { continue };
        let f = parse_desktop(&raw);
        let Some(img) = appimage_of(&f) else { continue };
        if !Path::new(&img).exists() {
            continue;
        }
        let real = canonical(&img);
        let launcher = p.to_string_lossy().into_owned();
        // Same app from several launchers: one entry, remember every launcher.
        if let Some(x) = v.iter_mut().find(|x| canonical(&x.id) == real) {
            x.extra.push('\t');
            x.extra.push_str(&launcher);
            continue;
        }
        let file = Path::new(&img).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        v.push(Package {
            name: f.get("Name").cloned().unwrap_or_else(|| file.clone()),
            version: f.get("X-AppImage-Version").cloned().unwrap_or_else(|| version_from_filename(&file)),
            source: Source::AppImage,
            summary: f.get("Comment").cloned().unwrap_or_default(),
            size_bytes: std::fs::metadata(&real).ok().map(|m| m.len()),
            kind: Kind::App,
            protected: false,
            extra: launcher,
            new_version: None,
            telemetry: None,
            installed_at: birth_time(&img),
            id: img,
        });
    }
    if let Ok(rd) = std::fs::read_dir(apps_dir()) {
        for e in rd.flatten() {
            let img = e.path().to_string_lossy().into_owned();
            let named = img.to_lowercase().ends_with(".appimage");
            if !(named || (e.path().is_file() && is_appimage(&img))) {
                continue;
            }
            let real = canonical(&img);
            if v.iter().any(|x| x.id == img || canonical(&x.id) == real) {
                continue;
            }
            let file = e.file_name().to_string_lossy().into_owned();
            v.push(Package {
                name: file.trim_end_matches(".AppImage").trim_end_matches(".appimage").to_string(),
                version: version_from_filename(&file),
                source: Source::AppImage,
                summary: "Not in the app menu".into(),
                size_bytes: std::fs::metadata(&real).ok().map(|m| m.len()),
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
    let real = canonical(&pkg.id);
    if real != pkg.id {
        d.location.push(real);
    }
    d.location.extend(pkg.extra.split('\t').filter(|l| !l.is_empty()).map(String::from));
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

/// Icons an integration tool made for this launcher (never shared theme icons).
fn own_icons(icon: &str) -> Vec<PathBuf> {
    let h = home();
    let user_icons = format!("{h}/.local/share/icons");
    if icon.starts_with('/') {
        // Only files in the user's icon folders, never system or theme files.
        return if icon.starts_with(&user_icons) { vec![PathBuf::from(icon)] } else { Vec::new() };
    }
    // AppImageLauncher / appimaged name them "appimagekit_<hash>…".
    if !icon.starts_with("appimagekit_") {
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

/// Ask the desktop to reload its app menu.
pub(crate) fn refresh_menus(dirs: &std::collections::BTreeSet<PathBuf>) {
    for d in dirs {
        let _ = Command::new("update-desktop-database").arg(d).output();
    }
    for kde in ["kbuildsycoca6", "kbuildsycoca5"] {
        if crate::backends::which(kde) {
            let _ = Command::new(kde).arg("--noincremental").output();
            break;
        }
    }
    let _ = Command::new("gtk-update-icon-cache").args(["-q", "-t", &format!("{}/.local/share/icons/hicolor", home())]).output();
}

pub fn remove(pkg: &Package) -> Result<String> {
    let real = canonical(&pkg.id);
    let targets: Vec<String> = [pkg.id.clone(), real.clone()].into_iter().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    // Find every launcher first, while the file still exists.
    let mut launchers = launchers_for(&targets);
    for l in pkg.extra.split('\t').filter(|l| !l.is_empty()) {
        if !launchers.iter().any(|(p, _)| p == Path::new(l)) {
            if let Ok(raw) = std::fs::read_to_string(l) {
                launchers.push((PathBuf::from(l), parse_desktop(&raw)));
            }
        }
    }

    // The app itself, plus every symlink pointing to it (in ~/Applications
    // or used by a launcher). Links are just deleted; the AppImage goes to the Trash.
    let mut aliases: std::collections::BTreeSet<String> = launchers.iter().filter_map(|(_, f)| appimage_of(f)).collect();
    aliases.insert(pkg.id.clone());
    if let Ok(rd) = std::fs::read_dir(apps_dir()) {
        aliases.extend(rd.flatten().map(|e| e.path().to_string_lossy().into_owned()));
    }
    for a in &aliases {
        let link = std::fs::symlink_metadata(a).map(|m| m.file_type().is_symlink()).unwrap_or(false);
        if link && *a != real && canonical(a) == real {
            let _ = std::fs::remove_file(a);
        }
    }
    let trash = |path: &str| -> Result<bool> {
        if Command::new("gio").args(["trash", path]).status().map(|s| s.success()).unwrap_or(false) {
            return Ok(true);
        }
        std::fs::remove_file(path).with_context(|| format!("could not remove {path}"))?;
        Ok(false)
    };
    // Only trash the target of a link if it really is an AppImage.
    let trashed = if real == pkg.id || (Path::new(&real).is_file() && is_appimage(&real)) {
        trash(&real)?
    } else {
        false
    };

    // Launchers (and the icons made for them).
    let mut removed = 0;
    let mut touched = std::collections::BTreeSet::new();
    let mut system: Vec<String> = Vec::new();
    for (path, f) in &launchers {
        if let Some(icon) = f.get("Icon") {
            for i in own_icons(icon) {
                let _ = std::fs::remove_file(i);
            }
        }
        if let Some(d) = path.parent() {
            touched.insert(d.to_path_buf());
        }
        match std::fs::remove_file(path) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => system.push(path.to_string_lossy().into_owned()),
            Err(_) => {}
        }
    }
    let mut note = String::new();
    if !system.is_empty() {
        // Installed for all users: one password prompt removes them.
        let mut argv = vec!["rm", "-f", "--"];
        argv.extend(system.iter().map(String::as_str));
        match crate::backends::run_privileged(&argv) {
            Ok(_) => removed += system.len(),
            Err(e) => note = format!(" Some menu entries for all users were not removed ({e}): {}.", system.join(", ")),
        }
    }
    refresh_menus(&touched);

    // Anything left that still points to it?
    let left: Vec<String> = launchers_for(&targets).into_iter().map(|(p, _)| p.to_string_lossy().into_owned()).collect();
    if note.is_empty() && !left.is_empty() {
        note = format!(" These menu entries still point to it: {}.", left.join(", "));
    }
    Ok(format!(
        "{} was removed{}{}. Its settings in your home folder were kept.{note}",
        pkg.name,
        if trashed { " (the AppImage file is in the Trash)" } else { "" },
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
    fn helpers() {
        assert_eq!(version_from_filename("Obsidian-1.6.7.AppImage"), "1.6.7");
        assert_eq!(version_from_filename("app_2.0.1-x86_64.AppImage"), "2.0.1");
        assert_eq!(slug("My Cool App 2"), "my-cool-app-2");
        let f = parse_desktop("[Desktop Entry]\nName=X\nExec=env FOO=1 \"/home/u/Applications/X.AppImage\" %U\n");
        assert_eq!(appimage_of(&f).as_deref(), Some("/home/u/Applications/X.AppImage"));
        assert!(appimage_type("/bin/sh").is_err());
    }

    #[test]
    fn path_matching() {
        assert!(mentions_path("sh -c '/opt/apps/foo --x'", "/opt/apps/foo"));
        assert!(mentions_path("\"/opt/apps/foo\" %U", "/opt/apps/foo"));
        assert!(!mentions_path("/opt/apps/foo2 %U", "/opt/apps/foo"));
        assert!(!mentions_path("/opt/apps/foo.AppImage", "/opt/apps/foo"));
    }

    /// A third-party integrator: renamed AppImage (no extension) kept in its
    /// own folder, a symlink in ~/Applications, and launchers in a menu
    /// subfolder, autostart and on the desktop. Uninstall must remove all.
    #[test]
    fn uninstall_removes_every_launcher() {
        let _guard = crate::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("pkgtrack-appimage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let h = home.to_string_lossy().into_owned();
        let store = format!("{h}/.local/lib/myintegrator");
        let menu_sub = format!("{h}/.local/share/applications/myintegrator");
        let auto = format!("{h}/.config/autostart");
        let desk = format!("{h}/Desktop");
        let icons = format!("{h}/.local/share/icons/hicolor/64x64/apps");
        for d in [&store, &menu_sub, &auto, &desk, &icons, &format!("{h}/Applications")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let real = format!("{store}/coolapp");
        std::fs::write(&real, b"\x7fELF\x02\x01\x01\x00AI\x02rest-of-file").unwrap();
        let link = format!("{h}/Applications/CoolApp.AppImage");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let icon = format!("{icons}/appimagekit_abc123_coolapp.png");
        std::fs::write(&icon, b"png").unwrap();
        let entry = |exec: &str| format!("[Desktop Entry]\nType=Application\nName=Cool App\nExec={exec}\nIcon=appimagekit_abc123_coolapp\n");
        std::fs::write(format!("{menu_sub}/coolapp.desktop"), entry(&format!("{real} %U"))).unwrap();
        std::fs::write(format!("{auto}/coolapp.desktop"), entry(&format!("sh -c '{real} --tray'"))).unwrap();
        std::fs::write(format!("{desk}/Cool App.desktop"), entry(&format!("\"{link}\""))).unwrap();
        let other = format!("{h}/.local/share/applications/other.desktop");
        std::fs::write(&other, entry("/usr/bin/true")).unwrap();
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("XDG_DATA_HOME", format!("{h}/.local/share"));
            std::env::set_var("XDG_CONFIG_HOME", format!("{h}/.config"));
            std::env::set_var("XDG_DATA_DIRS", format!("{h}/no-system"));
        }

        let pkgs = list().unwrap();
        assert_eq!(pkgs.len(), 1, "one app, not one per launcher: {pkgs:?}");
        let p = &pkgs[0];
        assert_eq!(p.name, "Cool App");
        assert_eq!(p.extra.split('\t').count(), 2, "menu + desktop launchers recorded: {}", p.extra);

        let msg = remove(p).unwrap();
        assert!(msg.contains("3 menu entries"), "{msg}");
        assert!(!Path::new(&link).exists() && std::fs::symlink_metadata(&link).is_err());
        assert!(!Path::new(&real).exists(), "the real AppImage behind the symlink is removed too");
        assert!(!Path::new(&icon).exists());
        assert!(Path::new(&other).exists(), "unrelated launchers are kept");
        assert!(list().unwrap().is_empty());

        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("XDG_DATA_DIRS");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn finds_launchers_of_deleted_appimages() {
        let _guard = crate::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("pkgtrack-orphans-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let h = home.to_string_lossy().into_owned();
        let menu = format!("{h}/.local/share/applications");
        std::fs::create_dir_all(&menu).unwrap();
        std::fs::create_dir_all(format!("{h}/Applications")).unwrap();
        let alive = format!("{h}/Applications/Alive.AppImage");
        std::fs::write(&alive, b"\x7fELF\x02\x01\x01\x00AI\x02x").unwrap();
        let entry = |name: &str, exec: &str| format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\n");
        // Deleted by hand: folder exists, file doesn't.
        std::fs::write(format!("{menu}/gone.desktop"), entry("Gone", &format!("{h}/Applications/Gone.AppImage %U"))).unwrap();
        // Renamed by an integrator (no extension), tagged with X-AppImage keys.
        std::fs::write(
            format!("{menu}/tagged.desktop"),
            entry("Tagged", &format!("\"{h}/Applications/tagged-app\"")) + "X-AppImage-Version=1.0\n",
        )
        .unwrap();
        // Unplugged drive: the whole folder is missing, so don't touch it.
        std::fs::write(format!("{menu}/usb.desktop"), entry("Usb", "/media/nobody/usb/App.AppImage")).unwrap();
        // Still there, and an ordinary program.
        std::fs::write(format!("{menu}/alive.desktop"), entry("Alive", &alive)).unwrap();
        std::fs::write(format!("{menu}/normal.desktop"), entry("Normal", "/usr/bin/true")).unwrap();
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("XDG_DATA_HOME", format!("{h}/.local/share"));
            std::env::set_var("XDG_CONFIG_HOME", format!("{h}/.config"));
            std::env::set_var("XDG_DATA_DIRS", format!("{h}/no-system"));
        }
        let mut names: Vec<String> = orphan_launchers().into_iter().inspect(|o| assert!(o.user)).map(|o| o.name).collect();
        names.sort();
        assert_eq!(names, ["Gone", "Tagged"]);
        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("XDG_DATA_DIRS");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Installing a new version of an installed AppImage replaces the old
    /// copy (same file name, as with Package_Tracker-x86_64.AppImage, or a
    /// different one) instead of leaving two.
    #[test]
    fn new_version_replaces_old() {
        let _guard = crate::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("pkgtrack-replace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let h = home.to_string_lossy().into_owned();
        std::fs::create_dir_all(format!("{h}/dl")).unwrap();
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("XDG_DATA_HOME", format!("{h}/.local/share"));
            std::env::set_var("XDG_CONFIG_HOME", format!("{h}/.config"));
            std::env::set_var("XDG_DATA_DIRS", format!("{h}/no-system"));
        }
        let stage = |file: &str, version: &str, body: &[u8]| -> Info {
            let dir = format!("{h}/stage-{version}");
            std::fs::create_dir_all(&dir).unwrap();
            let staged = format!("{dir}/{file}");
            std::fs::write(&staged, body).unwrap();
            let original = format!("{h}/dl/{file}");
            std::fs::write(&original, body).unwrap();
            let raw = "[Desktop Entry]\nType=Application\nName=Cool App\nExec=coolapp %U\n".to_string();
            let mut i = Info {
                staged, original, name: "Cool App".into(), comment: String::new(), version: version.into(),
                categories: String::new(), size: 1, icon: None, desktop: parse_desktop(&raw), desktop_raw: raw,
                needs_fuse2: false, warning: None, replaces: None,
            };
            i.replaces = installed_copy(&i);
            i
        };
        let v1 = stage("CoolApp-x86_64.AppImage", "1.0", b"\x7fELF\x02\x01\x01\x00AI\x02one");
        assert!(v1.replaces.is_none());
        install(&v1).unwrap();
        // Same file name, new version.
        let v2 = stage("CoolApp-x86_64.AppImage", "2.0", b"\x7fELF\x02\x01\x01\x00AI\x02two");
        assert_eq!(v2.replaces.as_ref().map(|r| r.1.as_str()), Some("1.0"));
        let msg = install(&v2).unwrap();
        assert!(msg.contains("replaced version 1.0"), "{msg}");
        // Different file name, new version.
        let v3 = stage("CoolApp-3.0-x86_64.AppImage", "3.0", b"\x7fELF\x02\x01\x01\x00AI\x02three");
        assert_eq!(v3.replaces.as_ref().map(|r| r.1.as_str()), Some("2.0"));
        install(&v3).unwrap();
        let l = list().unwrap();
        assert_eq!(l.len(), 1, "{l:?}");
        assert_eq!(l[0].version, "3.0");
        assert!(l[0].id.ends_with("CoolApp-3.0-x86_64.AppImage"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
