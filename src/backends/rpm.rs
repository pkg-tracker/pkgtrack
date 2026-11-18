//! RPM-based distros: Fedora/RHEL (dnf, yum) and openSUSE (zypper).
use super::{interesting_paths, native_files, run, run_privileged, which};
use crate::model::{Details, Kind, Package, Source};
use anyhow::Result;
use std::collections::HashMap;

fn tool() -> &'static str {
    if which("dnf") {
        "dnf"
    } else if which("zypper") {
        "zypper"
    } else {
        "yum"
    }
}

pub fn list() -> Result<Vec<Package>> {
    let out = run(
        "rpm",
        &["-qa", "--qf", "%{NAME}\t%{VERSION}-%{RELEASE}\t%{SIZE}\t%{GROUP}\t%{INSTALLTIME}\t%{SUMMARY}\n"],
        &[],
    )?;
    let apps = desktop_owners();
    let mut v = Vec::new();
    for line in out.lines() {
        let f: Vec<&str> = line.splitn(6, '\t').collect();
        if f.len() < 6 || f[0] == "gpg-pubkey" {
            continue;
        }
        let name = f[0].to_string();
        let kind = if apps.contains(&name) {
            Kind::App
        } else if name.starts_with("lib") || f[3].contains("Libraries") {
            Kind::Library
        } else {
            Kind::Runtime
        };
        v.push(Package {
            id: name.clone(),
            protected: matches!(name.as_str(), "glibc" | "rpm" | "systemd" | "kernel-core" | "bash" | "filesystem" | "setup" | "dnf" | "zypper" | "coreutils"),
            name,
            version: f[1].into(),
            source: Source::Rpm,
            summary: f[5].into(),
            size_bytes: f[2].parse().ok(),
            kind,
            extra: String::new(),
            new_version: None,
            telemetry: None,
            installed_at: f[4].parse().ok(),
        });
    }
    Ok(v)
}

fn desktop_owners() -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    let files: Vec<String> = std::fs::read_dir("/usr/share/applications")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.ends_with(".desktop"))
        .collect();
    if files.is_empty() {
        return set;
    }
    let mut args = vec!["-qf", "--qf", "%{NAME}\n"];
    args.extend(files.iter().map(String::as_str));
    if let Ok(out) = run("rpm", &args, &[1]) {
        set.extend(out.lines().filter(|l| !l.contains(' ')).map(String::from));
    }
    set
}

pub fn updates() -> Result<HashMap<String, String>> {
    let mut m = HashMap::new();
    match tool() {
        "zypper" => {
            let out = run("zypper", &["-q", "--no-refresh", "lu"], &[])?;
            // S | Repository | Name | Current Version | Available Version | Arch
            for l in out.lines() {
                let f: Vec<&str> = l.split('|').map(str::trim).collect();
                if f.len() >= 5 && f[0] == "v" {
                    m.insert(f[2].to_string(), f[4].to_string());
                }
            }
        }
        t => {
            // exit code 100 = updates available
            let out = run(t, &["check-update", "-q", "--cacheonly"], &[100, 1])?;
            for l in out.lines() {
                let f: Vec<&str> = l.split_whitespace().collect();
                if f.len() >= 3 && f[0].contains('.') {
                    let name = f[0].rsplit_once('.').map(|x| x.0).unwrap_or(f[0]);
                    m.insert(name.to_string(), f[1].to_string());
                }
            }
        }
    }
    Ok(m)
}

pub fn details(pkg: &Package) -> Details {
    let mut d = Details::default();
    if let Ok(desc) = run("rpm", &["-q", "--qf", "%{DESCRIPTION}", &pkg.id], &[]) {
        d.description = desc.trim().to_string();
    }
    let rdeps = match tool() {
        "zypper" => run("rpm", &["-q", "--whatrequires", &pkg.id, "--qf", "%{NAME}\n"], &[1]),
        t => run(t, &["repoquery", "--installed", "-q", "--whatrequires", &pkg.id, "--qf", "%{name}\n"], &[]),
    };
    if let Ok(out) = rdeps {
        d.used_by = out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("no package") && *l != pkg.id)
            .map(String::from)
            .collect();
        d.used_by.sort();
        d.used_by.dedup();
    }
    if let Ok(out) = run("rpm", &["-qR", &pkg.id], &[]) {
        d.depends_on = out
            .lines()
            .map(|l| l.split_whitespace().next().unwrap_or("").to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('/') && !l.starts_with("rpmlib("))
            .collect();
        d.depends_on.dedup();
    }
    if let Ok(out) = run("rpm", &["-q", "--qf", "%{URL}\x1f%{VENDOR}\x1f%{LICENSE}\x1f%{ARCH}", &pkg.id], &[]) {
        for (k, v) in ["Homepage", "Vendor", "License", "Architecture"].iter().zip(out.split('\x1f')) {
            if !v.trim().is_empty() && v != "(none)" {
                d.info.push((k.to_string(), v.trim().into()));
            }
        }
    }
    let files = native_files(pkg);
    d.running = crate::procs::running_from_files(&files);
    d.location = interesting_paths(files.into_iter());
    d
}

pub fn remove(pkg: &Package) -> Result<String> {
    match tool() {
        "zypper" => run_privileged(&["zypper", "-n", "remove", &pkg.id]),
        t => run_privileged(&[t, "remove", "-y", &pkg.id]),
    }
}

pub fn update(pkg: &Package) -> Result<String> {
    match tool() {
        "zypper" => run_privileged(&["zypper", "-n", "update", &pkg.id]),
        t => run_privileged(&[t, "upgrade", "-y", &pkg.id]),
    }
}
