//! Searching repositories and installing new packages.
use super::{run, run_privileged, run_user, which};
use crate::model::{Details, Kind, Package, Source};
use anyhow::Result;

const MAX_PER_SOURCE: usize = 200;

fn result(source: Source, id: &str, name: &str, version: &str, summary: &str, extra: String) -> Package {
    Package {
        id: id.to_string(),
        name: name.to_string(),
        version: version.to_string(),
        source,
        summary: summary.to_string(),
        size_bytes: None,
        kind: Kind::App,
        protected: false,
        extra,
        new_version: None,
        telemetry: None,
        installed_at: None,
    }
}

/// Search one source's repositories for `q`.
pub fn search(src: Source, q: &str) -> Result<Vec<Package>> {
    let mut v = Vec::new();
    match src {
        Source::AppImage => return Ok(v), // no repository to search
        Source::Apt => {
            for l in run("apt-cache", &["search", q], &[])?.lines() {
                if let Some((name, summary)) = l.split_once(" - ") {
                    v.push(result(src, name, name, "", summary, String::new()));
                }
            }
        }
        Source::Rpm if which("zypper") && !which("dnf") => {
            // S | Name | Summary | Type
            for l in run("zypper", &["-q", "--no-refresh", "se", "-t", "package", q], &[104])?.lines() {
                let f: Vec<&str> = l.split('|').map(str::trim).collect();
                if f.len() >= 3 && !f[1].is_empty() && f[1] != "Name" {
                    v.push(result(src, f[1], f[1], "", f[2], String::new()));
                }
            }
        }
        Source::Rpm => {
            let tool = if which("dnf") { "dnf" } else { "yum" };
            for l in run(tool, &["search", "-q", "--cacheonly", q], &[1])?.lines() {
                // name.arch : summary
                if let Some((na, summary)) = l.split_once(" : ") {
                    let na = na.trim();
                    let name = na.rsplit_once('.').map(|x| x.0).unwrap_or(na);
                    if !name.contains(' ') && !v.iter().any(|p: &Package| p.id == name) {
                        v.push(result(src, name, name, "", summary.trim(), String::new()));
                    }
                }
            }
        }
        Source::Pacman => {
            let out = run("pacman", &["-Ss", q], &[1])?;
            let mut lines = out.lines().peekable();
            while let Some(head) = lines.next() {
                // repo/name version [installed]
                let desc = lines.next_if(|l| l.starts_with(' ')).unwrap_or("").trim();
                let mut f = head.split_whitespace();
                let (Some(rn), Some(ver)) = (f.next(), f.next()) else { continue };
                let (repo, name) = rn.split_once('/').unwrap_or(("", rn));
                v.push(result(src, name, name, ver, desc, repo.to_string()));
            }
        }
        Source::Flatpak => {
            let out = run("flatpak", &["search", "--columns=application,name,version,remotes,description", q], &[])?;
            for l in out.lines() {
                let f: Vec<&str> = l.split('\t').collect();
                if f.len() >= 5 && f[0].contains('.') {
                    let remote = f[3].split(',').next().unwrap_or("flathub").to_string();
                    v.push(result(src, f[0], f[1], f[2], f[4], remote));
                }
            }
        }
        Source::Snap => {
            let out = run("snap", &["find", "--color=never", "--unicode=never", q], &[])?;
            for l in out.lines().skip(1) {
                let f: Vec<&str> = l.split_whitespace().collect();
                if f.len() >= 5 {
                    // extra = notes ("classic" means it needs --classic confinement)
                    v.push(result(src, f[0], f[0], f[1], &f[4..].join(" "), f[3].to_string()));
                }
            }
        }
    }
    rank(&mut v, q);
    v.truncate(MAX_PER_SOURCE);
    for p in &mut v {
        p.telemetry = crate::telemetry::quick_level(p);
    }
    Ok(v)
}

/// Put exact and prefix name matches first.
pub fn rank(v: &mut [Package], q: &str) {
    let q = q.to_lowercase();
    v.sort_by_cached_key(|p| {
        let id = p.id.to_lowercase();
        let name = p.name.to_lowercase();
        let score = if id == q || name == q || id.ends_with(&format!(".{q}")) {
            0
        } else if name.starts_with(&q) || id.starts_with(&q) {
            1
        } else if name.contains(&q) || id.contains(&q) {
            2
        } else {
            3
        };
        (score, name.len(), name)
    });
}

/// Parse "Key: value" style output, keeping only selected keys.
fn kv_lines(out: &str, keys: &[&str], sep: char) -> Vec<(String, String)> {
    let mut res = Vec::new();
    for l in out.lines() {
        if l.starts_with(' ') {
            continue;
        }
        if let Some((k, v)) = l.split_once(sep) {
            let k = k.trim();
            if keys.iter().any(|x| x.eq_ignore_ascii_case(k)) && !v.trim().is_empty() {
                res.push((k.to_string(), v.trim().to_string()));
            }
        }
    }
    res
}

fn get<'a>(info: &'a [(String, String)], key: &str) -> Option<&'a str> {
    info.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
}

/// Details for a package that is not installed yet.
pub fn remote_details(pkg: &Package) -> (Package, Details) {
    let mut pkg = pkg.clone();
    let mut d = Details::default();
    match pkg.source {
        Source::AppImage => {}
        Source::Apt => {
            if let Ok(out) = run("apt-cache", &["show", "--no-all-versions", &pkg.id], &[]) {
                // Long description: "Description(-en):" followed by indented lines.
                let mut lines = out.lines().skip_while(|l| !l.starts_with("Description"));
                if let Some(first) = lines.next() {
                    let mut text = first.split_once(": ").map(|x| x.1).unwrap_or("").to_string();
                    for l in lines.take_while(|l| l.starts_with(' ')) {
                        text.push('\n');
                        text.push_str(l);
                    }
                    d.description = crate::appstream::debian_long(&text);
                }
                d.homepage = out.lines().find_map(|l| l.strip_prefix("Homepage: ")).map(String::from);
                d.info = kv_lines(&out, &["Version", "Installed-Size", "Download-Size", "Section", "Homepage", "Maintainer", "Origin"], ':');
                if let Some(v) = get(&d.info, "Version") {
                    pkg.version = v.to_string();
                }
                pkg.size_bytes = get(&d.info, "Installed-Size").and_then(|s| s.parse::<u64>().ok()).map(|k| k * 1024);
                if let Some(dep) = kv_lines(&out, &["Depends"], ':').pop() {
                    d.depends_on = dep.1.split(',').filter_map(|s| s.split_whitespace().next()).map(String::from).collect();
                }
            }
            if let Ok(out) = run("apt-get", &["-s", "install", &pkg.id], &[100]) {
                d.would_install = Some(
                    out.lines()
                        .filter_map(|l| l.strip_prefix("Inst "))
                        .filter_map(|l| l.split_whitespace().next())
                        .map(String::from)
                        .collect(),
                );
            }
        }
        Source::Rpm => {
            let tool = if which("dnf") { "dnf" } else if which("zypper") { "zypper" } else { "yum" };
            let out = if tool == "zypper" {
                run("zypper", &["-q", "--no-refresh", "info", &pkg.id], &[])
            } else {
                run(tool, &["info", "-q", "--cacheonly", &pkg.id], &[1])
            };
            if let Ok(out) = out {
                d.info = kv_lines(&out, &["Version", "Release", "Size", "Installed Size", "Repository", "URL", "License", "Vendor"], ':');
                if let Some(v) = get(&d.info, "Version") {
                    pkg.version = v.to_string();
                }
            }
        }
        Source::Pacman => {
            if let Ok(out) = run("pacman", &["-Si", &pkg.id], &[]) {
                d.info = kv_lines(&out, &["Repository", "Version", "URL", "Licenses", "Download Size", "Installed Size", "Packager"], ':');
                if let Some(dep) = kv_lines(&out, &["Depends On"], ':').pop() {
                    d.depends_on = dep.1.split_whitespace().filter(|s| *s != "None").map(String::from).collect();
                }
            }
        }
        Source::Flatpak => {
            if let Ok(out) = run("flatpak", &["remote-info", &pkg.extra, &pkg.id], &[]) {
                d.info = kv_lines(&out, &["Version", "Branch", "Origin", "License", "Download", "Installed", "Runtime", "Date"], ':');
                if let Some(rt) = get(&d.info, "Runtime") {
                    d.depends_on.push(rt.to_string());
                }
            }
            d.info.insert(0, ("Remote".into(), pkg.extra.clone()));
            // Sandbox permissions are known before install from the app metadata.
            if let Ok(meta) = run("flatpak", &["remote-info", "--show-metadata", &pkg.extra, &pkg.id], &[]) {
                let mut in_ctx = false;
                for l in meta.lines() {
                    if l.starts_with('[') {
                        in_ctx = l == "[Context]";
                    } else if in_ctx {
                        if let Some((k, v)) = l.split_once('=') {
                            d.info.push((format!("Permission: {k}"), v.trim_end_matches(';').replace(';', ", ")));
                        }
                    }
                }
            }
        }
        Source::Snap => {
            if let Ok(out) = run("snap", &["info", "--color=never", "--unicode=never", &pkg.id], &[]) {
                d.info = kv_lines(&out, &["publisher", "store-url", "contact", "license", "description"], ':');
                if let Some(s) = get(&d.info, "summary") {
                    pkg.summary = s.to_string();
                }
            }
            if pkg.extra.contains("classic") {
                d.info.push(("Confinement".into(), "classic: NOT sandboxed, has full system access".into()));
            }
        }
    }
    d.telemetry = crate::telemetry::analyze(&pkg, &d);
    if pkg.source == Source::Snap {
        // Snap interfaces are only known once installed.
        d.telemetry.retain(|f| !f.title.starts_with("Sandbox"));
    }
    (pkg, d)
}

pub fn install(pkg: &Package) -> Result<String> {
    match pkg.source {
        Source::Apt => run_privileged(&["apt-get", "install", "-y", &pkg.id]),
        Source::Rpm if which("dnf") => run_privileged(&["dnf", "install", "-y", &pkg.id]),
        Source::Rpm if which("zypper") => run_privileged(&["zypper", "-n", "install", &pkg.id]),
        Source::Rpm => run_privileged(&["yum", "install", "-y", &pkg.id]),
        Source::Pacman => run_privileged(&["pacman", "-S", "--needed", "--noconfirm", &pkg.id]),
        Source::Flatpak => run_user("flatpak", &["install", "-y", "--noninteractive", &pkg.extra, &pkg.id]),
        Source::Snap if pkg.extra.contains("classic") => run_privileged(&["snap", "install", "--classic", &pkg.id]),
        Source::Snap => run_privileged(&["snap", "install", &pkg.id]),
        Source::AppImage => anyhow::bail!("Use \"Install AppImage…\" for AppImage files."),
    }
}
