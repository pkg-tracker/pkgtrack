//! Install / remove / update history from the package managers' own logs,
//! plus export and import of your app list.
use crate::advice::{Action, ActionKind};
use crate::backends::{run, which};
use crate::model::{Package, Source};
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Install,
    Upgrade,
    Downgrade,
    Remove,
    Purge,
    Reinstall,
    Other,
}

impl Op {
    pub fn label(self) -> &'static str {
        match self {
            Op::Install => "installed",
            Op::Upgrade => "upgraded",
            Op::Downgrade => "downgraded",
            Op::Remove => "removed",
            Op::Purge => "purged",
            Op::Reinstall => "reinstalled",
            Op::Other => "changed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Change {
    pub op: Op,
    pub package: String,
    pub version: String,
    pub automatic: bool,
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// "YYYY-MM-DD HH:MM"
    pub time: String,
    pub source: Source,
    pub command: String,
    pub by: String,
    pub changes: Vec<Change>,
    /// Free-text summary for sources without per-package detail.
    pub summary: String,
}

fn split_pkg_list(s: &str) -> Vec<(String, String, bool)> {
    // "a:amd64 (1.0, automatic), b:amd64 (1.0, 2.0)"
    let mut out = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out.into_iter()
        .filter_map(|item| {
            let item = item.trim();
            let (name, rest) = item.split_once(' ').unwrap_or((item, ""));
            let inner = rest.trim().trim_start_matches('(').trim_end_matches(')');
            let automatic = inner.contains("automatic");
            let version = inner.split(", ").filter(|v| *v != "automatic").last().unwrap_or("").to_string();
            (!name.is_empty()).then(|| (name.split(':').next().unwrap_or(name).to_string(), version, automatic))
        })
        .collect()
}

fn apt_history() -> Vec<Entry> {
    let mut texts = Vec::new();
    for i in (1..=12).rev() {
        let gz = format!("/var/log/apt/history.log.{i}.gz");
        if std::path::Path::new(&gz).exists() {
            if let Ok(t) = run("zcat", &[&gz], &[]) {
                texts.push(t);
            }
        }
    }
    if let Ok(t) = std::fs::read_to_string("/var/log/apt/history.log") {
        texts.push(t);
    }
    let mut v = Vec::new();
    for text in texts {
        for block in text.split("\n\n") {
            let mut e = Entry {
                time: String::new(),
                source: Source::Apt,
                command: String::new(),
                by: String::new(),
                changes: Vec::new(),
                summary: String::new(),
            };
            for l in block.lines() {
                let Some((k, val)) = l.split_once(": ") else { continue };
                let op = match k {
                    "Start-Date" => {
                        e.time = val.split_whitespace().collect::<Vec<_>>().join(" ");
                        e.time.truncate(16);
                        continue;
                    }
                    "Commandline" => {
                        e.command = val.into();
                        continue;
                    }
                    "Requested-By" => {
                        e.by = val.split(' ').next().unwrap_or(val).into();
                        continue;
                    }
                    "Install" => Op::Install,
                    "Upgrade" => Op::Upgrade,
                    "Downgrade" => Op::Downgrade,
                    "Remove" => Op::Remove,
                    "Purge" => Op::Purge,
                    "Reinstall" => Op::Reinstall,
                    _ => continue,
                };
                for (package, version, automatic) in split_pkg_list(val) {
                    e.changes.push(Change { op, package, version, automatic });
                }
            }
            if !e.time.is_empty() && !e.changes.is_empty() {
                if e.command.contains("unattended-upgrade") {
                    e.command = "Automatic security updates".into();
                } else if e.command.starts_with("aptdaemon") || e.command.starts_with("packagekit") {
                    e.command = "Software Updater / app store".into();
                }
                v.push(e);
            }
        }
    }
    v
}

fn pacman_history() -> Vec<Entry> {
    let Ok(log) = std::fs::read_to_string("/var/log/pacman.log") else { return Vec::new() };
    let mut v: Vec<Entry> = Vec::new();
    for l in log.lines() {
        // [2024-01-01T10:00:00+0000] [PACMAN] Running 'pacman -S foo'
        let Some(rest) = l.strip_prefix('[') else { continue };
        let Some((ts, rest)) = rest.split_once("] ") else { continue };
        let time = ts.replace('T', " ").chars().take(16).collect::<String>();
        if let Some(cmd) = rest.strip_prefix("[PACMAN] Running '") {
            v.push(Entry {
                time,
                source: Source::Pacman,
                command: cmd.trim_end_matches('\'').into(),
                by: String::new(),
                changes: Vec::new(),
                summary: String::new(),
            });
        } else if let Some(act) = rest.strip_prefix("[ALPM] ") {
            let mut f = act.splitn(3, ' ');
            let (Some(verb), Some(name), ver) = (f.next(), f.next(), f.next()) else { continue };
            let op = match verb {
                "installed" => Op::Install,
                "upgraded" => Op::Upgrade,
                "downgraded" => Op::Downgrade,
                "removed" => Op::Remove,
                "reinstalled" => Op::Reinstall,
                _ => continue,
            };
            let change = Change {
                op,
                package: name.into(),
                version: ver.unwrap_or("").trim_matches(['(', ')']).to_string(),
                automatic: false,
            };
            match v.last_mut() {
                Some(e) => e.changes.push(change),
                None => continue,
            }
        }
    }
    v.retain(|e| !e.changes.is_empty());
    v
}

fn snap_history() -> Vec<Entry> {
    let Ok(out) = run("snap", &["changes", "--abs-time"], &[]) else { return Vec::new() };
    out.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 5 || f[1] != "Done" {
                return None;
            }
            let summary = f[4..].join(" ");
            let names: Vec<String> = summary.split('"').skip(1).step_by(2).map(String::from).collect();
            let op = if summary.starts_with("Install") {
                Op::Install
            } else if summary.starts_with("Remove") {
                Op::Remove
            } else if summary.contains("efresh") {
                Op::Upgrade
            } else {
                Op::Other
            };
            Some(Entry {
                time: f[2].replace('T', " ").chars().take(16).collect(),
                source: Source::Snap,
                command: summary.clone(),
                by: String::new(),
                changes: names
                    .into_iter()
                    .flat_map(|n| n.split(", ").map(String::from).collect::<Vec<_>>())
                    .map(|n| Change { op, package: n, version: String::new(), automatic: false })
                    .collect(),
                summary,
            })
        })
        .collect()
}

fn dnf_history() -> Vec<Entry> {
    let Ok(out) = run("dnf", &["history", "list"], &[]) else { return Vec::new() };
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('|').map(str::trim).collect();
            if f.len() < 5 || f[0].parse::<u32>().is_err() {
                return None;
            }
            Some(Entry {
                time: f[2].chars().take(16).collect(),
                source: Source::Rpm,
                command: f[1].into(),
                by: String::new(),
                changes: Vec::new(),
                summary: format!("{} ({} packages)", f[3], f[4]),
            })
        })
        .collect()
}

/// First time a package appears as installed in APT's logs: (time, command).
pub fn first_install(pkg: &str) -> Option<(String, String)> {
    static MAP: std::sync::OnceLock<HashMap<String, (String, String)>> = std::sync::OnceLock::new();
    MAP.get_or_init(|| {
        let mut m: HashMap<String, (String, String)> = HashMap::new();
        let mut entries = apt_history();
        entries.sort_by(|a, b| a.time.cmp(&b.time));
        for e in entries {
            for c in e.changes.iter().filter(|c| c.op == Op::Install) {
                let how = if c.automatic {
                    format!("as a dependency, via: {}", e.command)
                } else if e.by.is_empty() {
                    e.command.clone()
                } else {
                    format!("{} by {}", e.command, e.by)
                };
                m.entry(c.package.clone()).or_insert((e.time.clone(), how));
            }
        }
        m
    })
    .get(pkg)
    .cloned()
}

pub fn load(sources: &[Source]) -> Vec<Entry> {
    let mut v = Vec::new();
    for s in sources {
        match s {
            Source::Apt => v.extend(apt_history()),
            Source::Pacman => v.extend(pacman_history()),
            Source::Snap => v.extend(snap_history()),
            Source::Rpm if which("dnf") => v.extend(dnf_history()),
            _ => {}
        }
    }
    v.sort_by(|a, b| b.time.cmp(&a.time));
    v
}

/// Undo / reinstall actions for an entry, based on what is installed now.
pub fn entry_actions(e: &Entry, installed: &[Package]) -> Vec<Action> {
    let have: HashSet<(Source, &str)> =
        installed.iter().map(|p| (p.source, p.id.split(':').next().unwrap_or(&p.id))).collect();
    let mut actions = Vec::new();
    let removed: Vec<String> = e
        .changes
        .iter()
        .filter(|c| matches!(c.op, Op::Remove | Op::Purge) && !have.contains(&(e.source, c.package.as_str())))
        .map(|c| c.package.clone())
        .collect();
    let added: Vec<String> = e
        .changes
        .iter()
        .filter(|c| c.op == Op::Install && !c.automatic && have.contains(&(e.source, c.package.as_str())))
        .map(|c| c.package.clone())
        .collect();
    let (install, remove): (Vec<&str>, Vec<&str>) = match e.source {
        Source::Apt => (vec!["apt-get", "install", "-y"], vec!["apt-get", "remove", "-y"]),
        Source::Pacman => (vec!["pacman", "-S", "--needed", "--noconfirm"], vec!["pacman", "-R", "--noconfirm"]),
        Source::Snap => (vec!["snap", "install"], vec!["snap", "remove"]),
        _ => return actions,
    };
    let argv = |base: &[&str], pkgs: &[String]| -> Vec<String> {
        base.iter().map(|s| s.to_string()).chain(pkgs.iter().cloned()).collect()
    };
    if !removed.is_empty() {
        actions.push(Action {
            label: format!("Reinstall {}", if removed.len() == 1 { removed[0].clone() } else { format!("{} packages", removed.len()) }),
            kind: ActionKind::Root(argv(&install, &removed)),
            confirm: Some(format!("Reinstall {}?", removed.join(", "))),
        });
    }
    if !added.is_empty() {
        actions.push(Action {
            label: "Undo install".into(),
            kind: ActionKind::Root(argv(&remove, &added)),
            confirm: Some(format!("Uninstall {} (installed in this step)?", added.join(", "))),
        });
    }
    if e.source == Source::Snap && e.summary.contains("efresh") {
        let names: Vec<String> = e.changes.iter().map(|c| c.package.clone()).filter(|n| have.contains(&(Source::Snap, n.as_str()))).collect();
        if names.len() == 1 {
            actions.push(Action {
                label: "Roll back".into(),
                kind: ActionKind::Root(vec!["snap".into(), "revert".into(), names[0].clone()]),
                confirm: Some(format!("Go back to the previous version of {}?", names[0])),
            });
        }
    }
    actions
}

// ------------------------------------------------------------ export / import

pub const HEADER: &str = "# pkgtrack app list v1";

pub fn export(installed: &[Package], sources: &[Source]) -> Result<String> {
    let mut lines = vec![HEADER.to_string(), format!("# exported {}", run("date", &["+%F"], &[]).unwrap_or_default().trim())];
    let manual: HashSet<String> = if sources.contains(&Source::Apt) {
        run("apt-mark", &["showmanual"], &[])?.lines().map(|l| l.trim().split(':').next().unwrap_or("").to_string()).collect()
    } else if sources.contains(&Source::Pacman) {
        run("pacman", &["-Qqe"], &[])?.lines().map(String::from).collect()
    } else if sources.contains(&Source::Rpm) && which("dnf") {
        run("dnf", &["repoquery", "--userinstalled", "-q", "--qf", "%{name}\n"], &[])?.lines().map(String::from).collect()
    } else {
        HashSet::new()
    };
    let mut seen = HashSet::new();
    for p in installed {
        let line = match p.source {
            Source::Apt | Source::Pacman | Source::Rpm => {
                let n = p.id.split(':').next().unwrap_or(&p.id);
                if !manual.contains(n) {
                    continue;
                }
                format!("{}\t{}", p.source.short(), n)
            }
            Source::Flatpak if p.kind == crate::model::Kind::App => {
                let remote = run("flatpak", &["info", "--show-origin", &p.id], &[]).unwrap_or_else(|_| "flathub".into());
                format!("flatpak\t{}\t{}", p.id, remote.trim())
            }
            Source::Snap if p.kind == crate::model::Kind::App => {
                format!("snap\t{}\t{}", p.id, if p.extra.contains("classic") { "classic" } else { "-" })
            }
            _ => continue,
        };
        if seen.insert(line.clone()) {
            lines.push(line);
        }
    }
    Ok(lines.join("\n") + "\n")
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub file: String,
    pub total: usize,
    pub already: usize,
    /// source -> packages to install
    pub missing: Vec<(Source, Vec<String>)>,
    pub unavailable: Vec<String>,
    pub action: Option<Action>,
}

pub fn plan_import(path: &str, installed: &[Package], sources: &[Source]) -> Result<Plan> {
    let content = std::fs::read_to_string(path).with_context(|| format!("cannot read {path}"))?;
    if !content.starts_with(HEADER) {
        bail!("{path} is not a Package Tracker app list.");
    }
    let have: HashSet<(Source, String)> = installed
        .iter()
        .map(|p| (p.source, p.id.split(':').next().unwrap_or(&p.id).to_string()))
        .collect();
    let mut plan = Plan { file: path.into(), ..Default::default() };
    let mut missing: HashMap<Source, Vec<String>> = HashMap::new();
    let mut classic = HashSet::new();
    let mut remotes: HashMap<String, String> = HashMap::new();
    for l in content.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let f: Vec<&str> = l.split('\t').collect();
        let src = match f[0] {
            "deb" => Source::Apt,
            "rpm" => Source::Rpm,
            "pacman" => Source::Pacman,
            "flatpak" => Source::Flatpak,
            "snap" => Source::Snap,
            _ => continue,
        };
        let Some(name) = f.get(1).map(|s| s.to_string()) else { continue };
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || "-+._".contains(c)) {
            continue; // never pass odd strings to a root command
        }
        plan.total += 1;
        if have.contains(&(src, name.clone())) {
            plan.already += 1;
            continue;
        }
        if !sources.contains(&src) {
            plan.unavailable.push(format!("{name} ({} not available here)", src.short()));
            continue;
        }
        if f.get(2) == Some(&"classic") {
            classic.insert(name.clone());
        }
        if src == Source::Flatpak {
            remotes.insert(name.clone(), f.get(2).unwrap_or(&"flathub").to_string());
        }
        missing.entry(src).or_default().push(name);
    }
    // Check APT availability: "Candidate: (none)" means no repo has it.
    if let Some(list) = missing.get_mut(&Source::Apt) {
        let mut args = vec!["policy"];
        args.extend(list.iter().map(String::as_str));
        if let Ok(out) = run("apt-cache", &args, &[]) {
            let mut ok = HashSet::new();
            let mut current = String::new();
            for l in out.lines() {
                if !l.starts_with(' ') && l.ends_with(':') {
                    current = l.trim_end_matches(':').to_string();
                } else if let Some(c) = l.trim().strip_prefix("Candidate: ") {
                    if c != "(none)" {
                        ok.insert(current.clone());
                    }
                }
            }
            let (avail, gone): (Vec<String>, Vec<String>) = list.drain(..).partition(|n| ok.contains(n));
            plan.unavailable.extend(gone.into_iter().map(|n| format!("{n} (not in your repositories)")));
            *list = avail;
        }
    }
    let mut steps = Vec::new();
    let mut root_script = Vec::new();
    for (src, list) in &missing {
        if list.is_empty() {
            continue;
        }
        match src {
            Source::Apt => root_script.push(format!("apt-get install -y {}", list.join(" "))),
            Source::Pacman => root_script.push(format!("pacman -S --needed --noconfirm {}", list.join(" "))),
            Source::Rpm if which("dnf") => root_script.push(format!("dnf install -y {}", list.join(" "))),
            Source::Rpm => root_script.push(format!("zypper -n install {}", list.join(" "))),
            Source::AppImage | Source::Manual => {}
            Source::Snap => {
                for n in list {
                    root_script.push(format!("snap install {n}{}", if classic.contains(n) { " --classic" } else { "" }));
                }
            }
            Source::Flatpak => {
                for n in list {
                    let remote = remotes.get(n).cloned().unwrap_or_else(|| "flathub".into());
                    steps.push(ActionKind::User(
                        ["flatpak", "install", "-y", "--noninteractive", &remote, n].map(String::from).to_vec(),
                    ));
                }
            }
        }
    }
    if !root_script.is_empty() {
        steps.insert(0, ActionKind::RootScript(format!("rc=0; {}; exit $rc", root_script.iter().map(|c| format!("{c} || rc=1")).collect::<Vec<_>>().join("; "))));
    }
    let count: usize = missing.values().map(Vec::len).sum();
    plan.missing = missing.into_iter().filter(|(_, l)| !l.is_empty()).collect();
    plan.missing.sort_by_key(|(s, _)| s.short());
    if count > 0 {
        plan.action = Some(Action {
            label: format!("Install {count} missing"),
            kind: ActionKind::Many(steps),
            confirm: Some(format!("Install {count} packages from {}?", plan.file.rsplit('/').next().unwrap_or(""))),
        });
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apt_package_lists() {
        let v = split_pkg_list("a:amd64 (1.0, automatic), b:i386 (1:2.0-1, 1:2.1-1), c (3)");
        assert_eq!(v[0], ("a".into(), "1.0".into(), true));
        assert_eq!(v[1], ("b".into(), "1:2.1-1".into(), false));
        assert_eq!(v[2], ("c".into(), "3".into(), false));
    }

    #[test]
    fn import_rejects_unsafe_names() {
        let path = std::env::temp_dir().join(format!("pkgtrack-import-{}.txt", std::process::id()));
        std::fs::write(&path, format!("{HEADER}\nsnap\tok-name\t-\nsnap\tbad;rm -rf /\t-\nflatpak\torg.example.App\tflathub\n")).unwrap();
        let plan = plan_import(path.to_str().unwrap(), &[], &[Source::Snap, Source::Flatpak]).unwrap();
        assert_eq!(plan.total, 2, "the unsafe line must be skipped");
        let desc = crate::advice::describe(&plan.action.unwrap().kind);
        assert!(desc.contains("snap install ok-name") && desc.contains("org.example.App") && !desc.contains("rm -rf"), "{desc}");
        std::fs::write(&path, "not an export").unwrap();
        assert!(plan_import(path.to_str().unwrap(), &[], &[]).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
