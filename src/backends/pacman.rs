//! Arch Linux and derivatives (Manjaro, EndeavourOS, ...).
use super::{interesting_paths, native_files, run, which};
use crate::model::{parse_human_size, Details, Kind, Package, Source};
use anyhow::Result;
use std::collections::HashMap;

fn blocks(out: &str) -> Vec<HashMap<String, String>> {
    let mut res = Vec::new();
    let mut cur: HashMap<String, String> = HashMap::new();
    let mut last = String::new();
    for line in out.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                res.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(" : ") {
            if !k.starts_with(' ') {
                last = k.trim().to_string();
                cur.insert(last.clone(), v.trim().to_string());
                continue;
            }
        }
        // continuation line
        if let Some(v) = cur.get_mut(&last) {
            v.push(' ');
            v.push_str(line.trim());
        }
    }
    if !cur.is_empty() {
        res.push(cur);
    }
    res
}

pub fn list() -> Result<Vec<Package>> {
    let out = run("pacman", &["-Qi"], &[])?;
    let apps = desktop_owners();
    let mut v = Vec::new();
    for b in blocks(&out) {
        let Some(name) = b.get("Name").cloned() else { continue };
        let kind = if apps.contains(&name) {
            Kind::App
        } else if name.starts_with("lib") || name.starts_with("lib32-") {
            Kind::Library
        } else {
            Kind::Runtime
        };
        v.push(Package {
            id: name.clone(),
            protected: matches!(name.as_str(), "glibc" | "pacman" | "systemd" | "linux" | "bash" | "filesystem" | "base" | "coreutils"),
            version: b.get("Version").cloned().unwrap_or_default(),
            source: Source::Pacman,
            summary: b.get("Description").cloned().unwrap_or_default(),
            size_bytes: b.get("Installed Size").and_then(|s| parse_human_size(s)),
            kind,
            extra: b.get("Install Reason").cloned().unwrap_or_default(),
            new_version: None,
            telemetry: None,
            installed_at: None,
            name,
        });
    }
    Ok(v)
}

fn desktop_owners() -> std::collections::HashSet<String> {
    let files: Vec<String> = std::fs::read_dir("/usr/share/applications")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.ends_with(".desktop"))
        .collect();
    let mut set = std::collections::HashSet::new();
    if files.is_empty() {
        return set;
    }
    let mut args = vec!["-Qoq"];
    args.extend(files.iter().map(String::as_str));
    if let Ok(out) = run("pacman", &args, &[1]) {
        set.extend(out.lines().map(|l| l.trim().to_string()));
    }
    set
}

pub fn updates() -> Result<HashMap<String, String>> {
    // checkupdates (pacman-contrib) uses a temp db and is safe; fall back to -Qu.
    let out = if which("checkupdates") {
        run("checkupdates", &[], &[2])?
    } else {
        run("pacman", &["-Qu"], &[1])?
    };
    let mut m = HashMap::new();
    for l in out.lines() {
        // name old -> new
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() >= 4 {
            m.insert(f[0].to_string(), f[3].to_string());
        }
    }
    Ok(m)
}

pub fn details(pkg: &Package) -> Details {
    let mut d = Details::default();
    if let Ok(out) = run("pacman", &["-Qi", &pkg.id], &[]) {
        if let Some(b) = blocks(&out).into_iter().next() {
            let list = |k: &str| -> Vec<String> {
                b.get(k)
                    .filter(|v| v.as_str() != "None")
                    .map(|v| v.split_whitespace().map(String::from).collect())
                    .unwrap_or_default()
            };
            d.used_by = list("Required By");
            d.used_by.extend(list("Optional For").into_iter().map(|p| format!("{p} (optional)")));
            d.depends_on = list("Depends On");
            for k in ["URL", "Packager", "Licenses", "Install Reason", "Install Date"] {
                if let Some(v) = b.get(k) {
                    d.info.push((k.into(), v.clone()));
                }
            }
        }
    }
    d.info.push(("Note".into(), "Updating on Arch runs a full system upgrade (pacman -Syu).".into()));
    let files = native_files(pkg);
    d.running = crate::procs::running_from_files(&files);
    d.location = interesting_paths(files.into_iter());
    d
}
