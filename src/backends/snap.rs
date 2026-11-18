use super::run;
use crate::model::{Details, Kind, Package, Source};
use anyhow::Result;
use std::collections::HashMap;

pub fn list() -> Result<Vec<Package>> {
    let out = run("snap", &["list", "--color=never", "--unicode=never"], &[])?;
    let mut v = Vec::new();
    for line in out.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 6 {
            continue;
        }
        let notes = f[5];
        let kind = if notes.contains("base") || notes.contains("core") || notes.contains("snapd")
            || notes.contains("kernel") || notes.contains("gadget")
        {
            Kind::Runtime
        } else if f[0].starts_with("gnome-") || f[0].starts_with("kf5-") || f[0].starts_with("gtk-common") || f[0].starts_with("mesa-") {
            Kind::Library
        } else {
            Kind::App
        };
        let name = f[0].to_string();
        v.push(Package {
            id: name.clone(),
            summary: format!("Publisher: {} · channel {}", f[4].trim_end_matches('*'), f[3]),
            protected: matches!(name.as_str(), "snapd") || notes.contains("kernel") || notes.contains("gadget"),
            size_bytes: snap_size(&name, f[2]),
            name,
            version: f[1].into(),
            source: Source::Snap,
            kind,
            extra: notes.into(), // e.g. "classic", "base", "-"
            new_version: None,
            telemetry: None,
            // /snap/<name> is created on first install and kept across refreshes.
            installed_at: crate::model::birth_time(&format!("/snap/{}", f[0])),
        });
    }
    Ok(v)
}

fn snap_size(name: &str, rev: &str) -> Option<u64> {
    std::fs::metadata(format!("/var/lib/snapd/snaps/{name}_{rev}.snap")).ok().map(|m| m.len())
}

pub fn updates() -> Result<HashMap<String, String>> {
    let out = run("snap", &["refresh", "--list", "--color=never", "--unicode=never"], &[])?;
    Ok(out
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((f.next()?.to_string(), f.next()?.to_string()))
        })
        .collect())
}

fn base_of(snap: &str) -> Option<String> {
    let y = std::fs::read_to_string(format!("/snap/{snap}/current/meta/snap.yaml")).ok()?;
    y.lines()
        .find_map(|l| l.strip_prefix("base:"))
        .map(|b| b.trim().to_string())
}

pub fn details(pkg: &Package) -> Details {
    let mut d = Details::default();
    // Who uses this snap: snaps using it as base, or plugging its content slots.
    if let Ok(rd) = std::fs::read_dir("/snap") {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n != pkg.id && n != "bin" && base_of(&n).as_deref() == Some(pkg.id.as_str()) {
                d.used_by.push(format!("{n} (base)"));
            }
        }
    }
    if let Ok(out) = run("snap", &["connections", &pkg.id], &[]) {
        for l in out.lines().skip(1) {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 3 {
                continue;
            }
            let (iface, plug, slot) = (f[0], f[1], f[2]);
            if plug != "-" && slot.starts_with(&format!("{}:", pkg.id)) && !plug.starts_with(&format!("{}:", pkg.id)) {
                if let Some(user) = plug.split(':').next() {
                    d.used_by.push(format!("{user} (via {iface})"));
                }
            } else if plug.starts_with(&format!("{}:", pkg.id)) && slot != "-" {
                d.info.push((format!("Interface: {iface}"), "connected".into()));
                if let Some(provider) = slot.split(':').next().filter(|p| !p.is_empty()) {
                    d.depends_on.push(format!("{provider} ({iface})"));
                }
            }
        }
    }
    d.used_by.sort();
    d.used_by.dedup();
    if let Some(b) = base_of(&pkg.id) {
        d.depends_on.insert(0, format!("{b} (base)"));
    }
    if let Ok(out) = run("snap", &["info", "--color=never", "--unicode=never", &pkg.id], &[]) {
        // "description: |" followed by indented lines
        let mut in_desc = false;
        let mut desc = Vec::new();
        for l in out.lines() {
            if l.starts_with("description:") {
                in_desc = true;
                continue;
            }
            if in_desc {
                if let Some(t) = l.strip_prefix("  ") {
                    desc.push(t.trim_end());
                    continue;
                }
                in_desc = false;
            }
        }
        // snap wraps lines at ~70 columns; re-join paragraphs.
        // Keep list items on their own lines and drop Markdown emphasis.
        let mut text = String::new();
        for l in desc {
            let t = l.trim();
            if t.is_empty() {
                text.push_str("\n\n");
            } else if let Some(item) = t.strip_prefix("* ").or_else(|| t.strip_prefix("- ")) {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str("• ");
                text.push_str(item);
            } else {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push(' ');
                }
                text.push_str(t);
            }
        }
        while text.contains("\n\n\n") {
            text = text.replace("\n\n\n", "\n\n");
        }
        d.description = text.replace("**", "").trim().to_string();
        for l in out.lines().take_while(|l| !l.starts_with("channels:")) {
            if let Some((k, v)) = l.split_once(':') {
                if k == "store-url" {
                    d.homepage = Some(v.trim().to_string());
                }
                if matches!(k, "publisher" | "store-url" | "contact" | "license" | "tracking" | "refresh-date") {
                    d.info.push((k.into(), v.trim().into()));
                }
            }
        }
    }
    d.location.push(format!("/snap/{}/current", pkg.id));
    d.location.push(format!("~/snap/{} (user data)", pkg.id));
    d.running = crate::procs::running_matching(|exe, cgroup| {
        exe.starts_with(&format!("/snap/{}/", pkg.id)) || cgroup.contains(&format!("snap.{}.", pkg.id))
    });
    d
}
