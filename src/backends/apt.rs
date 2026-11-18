use super::{interesting_paths, native_files, run};
use crate::model::{Details, Kind, Package, Source};
use anyhow::Result;
use std::collections::{HashMap, HashSet};

pub fn list() -> Result<Vec<Package>> {
    let out = run(
        "dpkg-query",
        &[
            "-W",
            "-f=${binary:Package}\t${Version}\t${Installed-Size}\t${db:Status-Abbrev}\t${Essential}\t${Priority}\t${Section}\t${binary:Summary}\n",
        ],
        &[],
    )?;
    let apps = desktop_owners();
    let mut v = Vec::new();
    for line in out.lines() {
        let f: Vec<&str> = line.splitn(8, '\t').collect();
        if f.len() < 8 || !f[3].starts_with("ii") {
            continue;
        }
        let id = f[0].to_string();
        let base = id.split(':').next().unwrap_or(&id).to_string();
        let section = f[6];
        let kind = if apps.contains(&base) {
            Kind::App
        } else if section.contains("libs") || base.starts_with("lib") {
            Kind::Library
        } else {
            Kind::Runtime
        };
        // The doc folder is created on first install and kept across upgrades.
        let installed_at = crate::model::birth_time(&format!("/usr/share/doc/{base}"))
            .or_else(|| crate::model::birth_time(&format!("/var/lib/dpkg/info/{}.list", f[0])));
        v.push(Package {
            name: base,
            id,
            version: f[1].into(),
            source: Source::Apt,
            summary: f[7].into(),
            // Installed-Size is in KiB; a few vendors wrongly put bytes there.
            size_bytes: f[2].parse::<u64>().ok().map(|k| if k > 50_000_000 { k } else { k * 1024 }),
            kind,
            protected: f[4] == "yes" || f[5] == "required",
            extra: String::new(),
            new_version: None,
            telemetry: None,
            installed_at,
        });
    }
    Ok(v)
}

/// Packages that ship a .desktop launcher are user-facing apps.
fn desktop_owners() -> HashSet<String> {
    let mut files = Vec::new();
    for dir in ["/usr/share/applications"] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "desktop") {
                    files.push(p.to_string_lossy().into_owned());
                }
            }
        }
    }
    let mut set = HashSet::new();
    if files.is_empty() {
        return set;
    }
    let mut args = vec!["-S"];
    args.extend(files.iter().map(String::as_str));
    // dpkg -S exits 1 if some files are unowned; that's fine.
    if let Ok(out) = run("dpkg", &args, &[1]) {
        for line in out.lines() {
            if let Some((pkgs, _)) = line.split_once(": ") {
                for p in pkgs.split(", ") {
                    set.insert(p.split(':').next().unwrap_or(p).to_string());
                }
            }
        }
    }
    set
}

pub fn updates() -> Result<HashMap<String, String>> {
    let out = run("apt", &["list", "--upgradable"], &[])?;
    let mut m = HashMap::new();
    for line in out.lines().skip_while(|l| !l.contains('/')) {
        // name/suite version arch [upgradable from: x]
        let mut parts = line.split_whitespace();
        let (Some(name), Some(ver), arch) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let name = name.split('/').next().unwrap_or(name);
        m.insert(name.to_string(), ver.to_string());
        if let Some(a) = arch {
            m.insert(format!("{name}:{a}"), ver.to_string());
        }
    }
    Ok(m)
}

pub fn details(pkg: &Package) -> Details {
    let mut d = Details::default();
    if let Ok(out) = run(
        "apt-cache",
        &["rdepends", "--installed", "--no-recommends", "--no-suggests", "--no-enhances", "--no-replaces", "--no-conflicts", "--no-breaks", &pkg.id],
        &[],
    ) {
        let mut seen = HashSet::new();
        for l in out.lines().skip_while(|l| !l.starts_with("Reverse Depends")).skip(1) {
            let n = l.trim().trim_start_matches('|').to_string();
            if !n.is_empty() && n != pkg.name && seen.insert(n.clone()) {
                d.used_by.push(n);
            }
        }
    }
    if let Ok(out) = run(
        "dpkg-query",
        &["-W", "-f=${Depends}\x1f${Homepage}\x1f${Maintainer}\x1f${Section}\x1f${Architecture}\x1f${Description}", &pkg.id],
        &[],
    ) {
        let f: Vec<&str> = out.split('\x1f').collect();
        d.depends_on = f[0]
            .split(',')
            .map(|s| s.split_whitespace().next().unwrap_or("").to_string())
            .filter(|s| !s.is_empty())
            .collect();
        d.description = f.get(5).map(|s| crate::appstream::debian_long(s)).unwrap_or_default();
        d.homepage = f.get(1).map(|s| s.trim().to_string()).filter(|s| s.starts_with("http"));
        for (i, k) in ["Homepage", "Maintainer", "Section", "Architecture"].iter().enumerate() {
            if let Some(v) = f.get(i + 1).filter(|v| !v.trim().is_empty()) {
                d.info.push((k.to_string(), v.trim().to_string()));
            }
        }
    }
    let manual = run("apt-mark", &["showmanual", &pkg.name], &[])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    d.info.push((
        "Installed as".into(),
        if manual { "manually (by you)" } else { "automatically (as a dependency)" }.into(),
    ));

    let files = native_files(pkg);
    if let Some((desc, home)) = crate::appstream::from_files(&files) {
        d.description = desc;
        d.homepage = home.or(d.homepage.take());
    }
    d.running = crate::procs::running_from_files(&files);
    d.location = interesting_paths(files.into_iter());

    // Dry run: what would actually be removed along with it.
    if let Ok(out) = run("apt-get", &["-s", "remove", &pkg.id], &[100]) {
        d.would_remove = Some(
            out.lines()
                .filter_map(|l| l.strip_prefix("Remv "))
                .map(|l| l.split_whitespace().next().unwrap_or(l).to_string())
                .collect(),
        );
    }
    d
}
