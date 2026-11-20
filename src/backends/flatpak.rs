use super::{run, run_user};
use crate::model::{parse_human_size, Details, Kind, Package, Source};
use anyhow::Result;
use std::collections::HashMap;

// Package.extra = "<installation>\t<ref>"
pub(crate) fn installation(pkg: &Package) -> &str {
    pkg.extra.split('\t').next().unwrap_or("system")
}
pub(crate) fn pkg_ref(pkg: &Package) -> &str {
    pkg.extra.split('\t').nth(1).unwrap_or(&pkg.id)
}

pub fn list() -> Result<Vec<Package>> {
    let apps: std::collections::HashSet<String> = run("flatpak", &["list", "--app", "--columns=ref"], &[])?
        .lines()
        .map(|l| l.trim().to_string())
        .collect();
    let out = run(
        "flatpak",
        &["list", "--columns=application,name,version,size,installation,ref,description"],
        &[],
    )?;
    let mut v = Vec::new();
    for line in out.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 {
            continue;
        }
        let r = f[5].to_string();
        let branch = r.rsplit('/').next().unwrap_or("");
        let kind = if apps.contains(&r) { Kind::App } else { Kind::Runtime };
        v.push(Package {
            id: f[0].into(),
            name: if kind == Kind::Runtime {
                format!("{} ({branch})", f[1])
            } else {
                f[1].into()
            },
            version: if f[2].is_empty() { branch.to_string() } else { f[2].into() },
            source: Source::Flatpak,
            summary: f[6].into(),
            size_bytes: parse_human_size(f[3]),
            kind,
            protected: false,
            extra: format!("{}\t{}", f[4], r),
            new_version: None,
            telemetry: None,
            installed_at: {
                let base = if f[4] == "user" {
                    format!("{}/.local/share/flatpak", std::env::var("HOME").unwrap_or_default())
                } else {
                    "/var/lib/flatpak".into()
                };
                let dir = if kind == Kind::App { "app" } else { "runtime" };
                crate::model::birth_time(&format!("{base}/{dir}/{}", f[0]))
            },
        });
    }
    Ok(v)
}

pub fn updates() -> Result<HashMap<String, String>> {
    let out = run("flatpak", &["remote-ls", "--updates", "--columns=application,version,branch"], &[])?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let v = f.get(1).filter(|v| !v.is_empty()).or(f.get(2))?;
            Some((f[0].to_string(), v.to_string()))
        })
        .collect())
}

pub fn details(pkg: &Package) -> Details {
    let mut d = Details::default();
    let r = pkg_ref(pkg);
    // Who uses this runtime?
    if let Ok(out) = run("flatpak", &["list", "--app", "--columns=application,runtime"], &[]) {
        let rt = r.trim_start_matches("runtime/");
        for l in out.lines() {
            if let Some((app, runtime)) = l.split_once('\t') {
                if runtime.trim() == rt || (pkg.kind == Kind::Runtime && runtime.starts_with(&format!("{}/", pkg.id))) {
                    d.used_by.push(app.to_string());
                }
            }
        }
    }
    if let Ok(out) = run("flatpak", &["info", &format!("--{}", installation(pkg)), r], &[]) {
        for l in out.lines() {
            if let Some((k, v)) = l.split_once(':') {
                let k = k.trim();
                if matches!(k, "Origin" | "Runtime" | "Sdk" | "Installation" | "License" | "Commit" | "Date" | "Installed") {
                    if k == "Runtime" {
                        d.depends_on.push(v.trim().into());
                    }
                    d.info.push((k.into(), v.trim().into()));
                }
            }
        }
    }
    if pkg.kind == Kind::App {
        if let Ok(out) = run("flatpak", &["info", "--show-permissions", r], &[]) {
            for l in out.lines() {
                if let Some((k, v)) = l.split_once('=') {
                    d.info.push((format!("Permission: {k}"), v.trim_end_matches(';').replace(';', ", ")));
                }
            }
        }
        d.switches = permission_switches(&pkg.id);
        if let Ok(out) = run("flatpak", &["ps", "--columns=pid,application"], &[]) {
            for l in out.lines() {
                if let Some((pid, app)) = l.split_once('\t') {
                    if app.trim() == pkg.id {
                        let pid: u32 = pid.trim().parse().unwrap_or(0);
                        d.running.push((pid, crate::procs::cmdline(pid)));
                    }
                }
            }
        }
    }
    // AppStream description shipped inside the app.
    if pkg.kind == Kind::App {
        let root = if installation(pkg) == "user" {
            format!("{}/.local/share/flatpak", std::env::var("HOME").unwrap_or_default())
        } else {
            "/var/lib/flatpak".into()
        };
        let share = format!("{root}/app/{}/current/active/files/share", pkg.id);
        let files: Vec<String> = ["metainfo", "appdata"]
            .iter()
            .flat_map(|d| [format!("{share}/{d}/{}.metainfo.xml", pkg.id), format!("{share}/{d}/{}.appdata.xml", pkg.id)])
            .collect();
        if let Some((desc, home)) = crate::appstream::from_files(&files) {
            d.description = desc;
            d.homepage = home;
        }
    }
    let base = if installation(pkg) == "user" {
        format!("{}/.local/share/flatpak", std::env::var("HOME").unwrap_or_default())
    } else {
        "/var/lib/flatpak".into()
    };
    let kind_dir = if pkg.kind == Kind::App { "app" } else { "runtime" };
    d.location.push(format!("{base}/{kind_dir}/{}", pkg.id));
    if pkg.kind == Kind::App {
        d.location.push(format!("~/.var/app/{} (user data)", pkg.id));
    }
    d
}

pub fn remove(pkg: &Package) -> Result<String> {
    // flatpak talks to its own polkit-enabled helper; no pkexec needed.
    let inst = format!("--{}", installation(pkg));
    run_user("flatpak", &["uninstall", "-y", "--noninteractive", &inst, pkg_ref(pkg)])
}

pub fn update(pkg: &Package) -> Result<String> {
    let inst = format!("--{}", installation(pkg));
    run_user("flatpak", &["update", "-y", "--noninteractive", &inst, pkg_ref(pkg)])
}

/// A sandbox permission the user can switch on or off.
#[derive(Debug, Clone)]
pub struct Switch {
    pub label: &'static str,
    pub help: &'static str,
    pub on: bool,
    pub enable: String,
    pub disable: String,
}

/// Effective permissions = app defaults + user/system overrides.
fn effective(app_id: &str) -> std::collections::HashMap<String, std::collections::HashSet<String>> {
    let mut m: std::collections::HashMap<String, std::collections::HashSet<String>> = Default::default();
    let mut apply = |text: &str| {
        let mut ctx = false;
        for l in text.lines() {
            if l.starts_with('[') {
                ctx = l == "[Context]";
            } else if ctx {
                if let Some((k, v)) = l.split_once('=') {
                    let set = m.entry(k.to_string()).or_default();
                    for item in v.split(';').filter(|x| !x.is_empty()) {
                        match item.strip_prefix('!') {
                            Some(neg) => {
                                set.remove(neg);
                            }
                            None => {
                                set.insert(item.to_string());
                            }
                        }
                    }
                }
            }
        }
    };
    apply(&run("flatpak", &["info", "--show-permissions", app_id], &[]).unwrap_or_default());
    // --show-permissions already includes overrides on recent flatpak; applying
    // the user overrides again is harmless and covers older versions.
    apply(&run("flatpak", &["override", "--user", "--show", app_id], &[]).unwrap_or_default());
    m
}

pub fn permission_switches(app_id: &str) -> Vec<Switch> {
    let p = effective(app_id);
    let has = |k: &str, v: &str| p.get(k).is_some_and(|s| s.contains(v));
    let id = app_id.to_string();
    let sw = |label, help, on: bool, en: &str, dis: &str| Switch {
        label,
        help,
        on,
        enable: format!("--{en}"),
        disable: format!("--{dis}"),
    };
    let _ = id;
    vec![
        sw("Internet", "Connect to the network. Most apps that sync, update or browse need it.", has("shared", "network"), "share=network", "unshare=network"),
        sw("All your files", "Read and change every file on the computer.", has("filesystems", "host"), "filesystem=host", "nofilesystem=host"),
        sw("Home folder", "Read and change files in your home folder.", has("filesystems", "home") || has("filesystems", "host"), "filesystem=home", "nofilesystem=home"),
        sw("Devices (camera, USB, GPU)", "Direct access to hardware such as webcams and USB devices.", has("devices", "all"), "device=all", "nodevice=all"),
        sw("Sound & microphone", "Play sound and record from the microphone.", has("sockets", "pulseaudio"), "socket=pulseaudio", "nosocket=pulseaudio"),
        sw("Bluetooth", "Talk to Bluetooth devices.", has("features", "bluetooth"), "allow=bluetooth", "disallow=bluetooth"),
        sw("X11 windows", "Old display system: X11 apps can see other windows' contents and keystrokes. Wayland is safer.", has("sockets", "x11"), "socket=x11", "nosocket=x11"),
    ]
}
