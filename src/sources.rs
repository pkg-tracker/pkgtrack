//! Where software comes from: APT repositories / PPAs, Flatpak remotes,
//! DNF/zypper repos, pacman repos, and packages that no repository provides.
use crate::advice::{Action, ActionKind};
use crate::backends::{run, which};
use crate::model::{Package, Source};
use std::collections::{BTreeSet, HashSet};

#[derive(Debug, Clone)]
pub struct Repo {
    pub source: Source,
    pub name: String,
    pub url: String,
    pub suites: String,
    pub file: String,
    pub enabled: bool,
    pub official: bool,
    pub issues: Vec<String>,
    /// Installed packages that come from this repo (third-party only).
    pub installed: Vec<String>,
    pub toggle: Option<(Action, Action)>, // (disable, enable)
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub repos: Vec<Repo>,
    /// Installed packages no enabled repository offers (no security updates).
    pub orphans: Vec<String>,
    pub codename: String,
}

const UBUNTU: &[&str] = &[
    "trusty", "xenial", "bionic", "focal", "groovy", "hirsute", "impish", "jammy", "kinetic", "lunar", "mantic",
    "noble", "oracular", "plucky", "questing", "resolute",
];
const DEBIAN: &[&str] = &["stretch", "buster", "bullseye", "bookworm", "trixie", "forky"];

fn os_codename() -> (String, String) {
    let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let get = |k: &str| {
        os.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .map(|v| v.trim_matches('"').to_string())
            .unwrap_or_default()
    };
    let code = get("UBUNTU_CODENAME");
    let code = if code.is_empty() { get("VERSION_CODENAME") } else { code };
    (code, get("PRETTY_NAME"))
}

fn is_official(url: &str) -> bool {
    let host = url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("");
    host == "archive.ubuntu.com"
        || host.ends_with(".archive.ubuntu.com")
        || host == "security.ubuntu.com"
        || host == "ports.ubuntu.com"
        || host == "esm.ubuntu.com"
        || host == "deb.debian.org"
        || host == "security.debian.org"
        || host.ends_with(".debian.org")
}

fn list_prefix(url: &str) -> String {
    let u = url.split("://").nth(1).unwrap_or(url);
    let u = u.rsplit_once('@').map(|x| x.1).unwrap_or(u);
    u.trim_end_matches('/').replace('/', "_")
}

/// Packages offered by a repo, read from apt's downloaded index files.
fn repo_packages(url: &str) -> Option<HashSet<String>> {
    let prefix = list_prefix(url);
    let rd = std::fs::read_dir("/var/lib/apt/lists").ok()?;
    let mut set = HashSet::new();
    let mut found = false;
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with(&format!("{prefix}_")) && n.ends_with("_Packages") {
            found = true;
            if let Ok(c) = std::fs::read_to_string(e.path()) {
                set.extend(c.lines().filter_map(|l| l.strip_prefix("Package: ")).map(String::from));
            }
        }
    }
    found.then_some(set)
}

struct RawRepo {
    url: String,
    suites: Vec<String>,
    file: String,
    enabled: bool,
}

fn parse_apt() -> Vec<RawRepo> {
    let mut v = Vec::new();
    let mut files = vec!["/etc/apt/sources.list".to_string()];
    if let Ok(rd) = std::fs::read_dir("/etc/apt/sources.list.d") {
        let mut more: Vec<String> = rd
            .flatten()
            .map(|e| e.path().to_string_lossy().into_owned())
            .filter(|p| p.ends_with(".list") || p.ends_with(".sources"))
            .collect();
        more.sort();
        files.extend(more);
    }
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else { continue };
        if file.ends_with(".sources") {
            for para in content.split("\n\n") {
                let mut url = String::new();
                let mut suites = Vec::new();
                let mut enabled = true;
                let mut types = String::new();
                for l in para.lines() {
                    let l = l.trim();
                    if l.starts_with('#') {
                        continue;
                    }
                    if let Some((k, val)) = l.split_once(':') {
                        let val = val.trim();
                        match k.trim().to_ascii_lowercase().as_str() {
                            "uris" => url = val.split_whitespace().next().unwrap_or("").to_string(),
                            "suites" => suites = val.split_whitespace().map(String::from).collect(),
                            "enabled" => enabled = val != "no",
                            "types" => types = val.into(),
                            _ => {}
                        }
                    }
                }
                if !url.is_empty() && types.split_whitespace().any(|t| t == "deb") {
                    v.push(RawRepo { url, suites, file: file.clone(), enabled });
                }
            }
        } else {
            for l in content.lines() {
                let t = l.trim();
                let (enabled, rest) = match t.strip_prefix('#') {
                    Some(r) => (false, r.trim()),
                    None => (true, t),
                };
                let Some(rest) = rest.strip_prefix("deb ") else { continue };
                let rest = if rest.trim_start().starts_with('[') {
                    rest.split_once(']').map(|x| x.1).unwrap_or(rest)
                } else {
                    rest
                };
                let f: Vec<&str> = rest.split_whitespace().collect();
                if f.len() >= 2 && f[0].contains("://") {
                    v.push(RawRepo { url: f[0].into(), suites: vec![f[1].into()], file: file.clone(), enabled });
                }
            }
        }
    }
    v
}

fn toggle_actions(file: &str) -> (Action, Action) {
    let (disable, enable) = if file.ends_with(".sources") {
        (
            format!(
                "if grep -qi '^Enabled:' '{file}'; then sed -i 's/^Enabled:.*/Enabled: no/I' '{file}'; \
                 else sed -i '/^Types:/I a Enabled: no' '{file}'; fi"
            ),
            format!("sed -i 's/^Enabled:.*/Enabled: yes/I' '{file}'"),
        )
    } else {
        (format!("sed -i 's/^deb /# deb /' '{file}'"), format!("sed -i 's/^# *deb /deb /' '{file}'"))
    };
    let short = file.rsplit('/').next().unwrap_or(file).to_string();
    (
        Action {
            label: "Disable".into(),
            kind: ActionKind::RootScript(disable),
            confirm: Some(format!(
                "Disable the repository in {short}? Packages installed from it stay, but they won't get updates from it."
            )),
        },
        Action { label: "Enable".into(), kind: ActionKind::RootScript(enable), confirm: None },
    )
}

pub fn report(sources: &[Source], installed: &[Package]) -> Report {
    let (codename, _) = os_codename();
    let mut r = Report { codename: codename.clone(), ..Default::default() };
    let installed_apt: HashSet<String> = installed
        .iter()
        .filter(|p| p.source == Source::Apt)
        .map(|p| p.id.split(':').next().unwrap_or(&p.id).to_string())
        .collect();

    if sources.contains(&Source::Apt) {
        // Merge entries by (file, url).
        let mut merged: Vec<RawRepo> = Vec::new();
        for raw in parse_apt() {
            match merged.iter_mut().find(|m| m.file == raw.file && m.url == raw.url) {
                Some(m) => {
                    m.enabled |= raw.enabled;
                    for s in raw.suites {
                        if !m.suites.contains(&s) {
                            m.suites.push(s);
                        }
                    }
                }
                None => merged.push(raw),
            }
        }
        for raw in merged.into_iter().filter(|r| !r.url.starts_with("cdrom:")) {
            let official = is_official(&raw.url);
            let mut issues = Vec::new();
            let mut pkgs = Vec::new();
            // PPAs are built per release, so a different codename really means a mismatch.
            // (Vendors like Signal use one codename for every release on purpose.)
            let per_release = raw.url.contains("launchpadcontent.net") || raw.url.contains("ppa.launchpad.net") || official;
            for s in raw.suites.iter().filter(|_| per_release && raw.enabled) {
                let base = s.split('-').next().unwrap_or(s);
                if (UBUNTU.contains(&base) || DEBIAN.contains(&base)) && base != codename && !codename.is_empty() {
                    issues.push(format!(
                        "Made for {base}, but you run {codename}. Its packages may not fit your system; look for a {codename} version of this repository."
                    ));
                    break;
                }
            }
            if !official {
                match repo_packages(&raw.url) {
                    Some(set) => {
                        pkgs = set.intersection(&installed_apt).cloned().collect::<BTreeSet<_>>().into_iter().collect();
                        if pkgs.is_empty() && raw.enabled {
                            issues.push("Nothing installed comes from it. You can disable it.".into());
                        }
                    }
                    None if raw.enabled => issues.push(
                        "apt has no package list for it: it may be unreachable or broken (watch for errors when updating).".into(),
                    ),
                    None => {}
                }
            }
            let host = raw.url.split("://").nth(1).unwrap_or(&raw.url).split('/').next().unwrap_or("").to_string();
            let name = if host.contains("launchpadcontent.net") || host.contains("ppa.launchpad.net") {
                let parts: Vec<&str> = raw.url.split('/').collect();
                format!("PPA: {}/{}", parts.get(3).unwrap_or(&""), parts.get(4).unwrap_or(&""))
            } else if official {
                format!("{} (official)", host)
            } else {
                host
            };
            r.repos.push(Repo {
                source: Source::Apt,
                name,
                url: raw.url,
                suites: raw.suites.join(" "),
                toggle: (!official || raw.file.ends_with(".list")).then(|| toggle_actions(&raw.file)),
                file: raw.file,
                enabled: raw.enabled,
                official,
                issues,
                installed: pkgs,
            });
        }
        if let Ok(out) = run("apt", &["list", "--installed"], &[]) {
            r.orphans = out
                .lines()
                .filter(|l| l.contains(",local]"))
                .filter_map(|l| l.split('/').next())
                .map(String::from)
                .collect();
        }
    }
    if sources.contains(&Source::Flatpak) {
        let origins = run("flatpak", &["list", "--columns=application,origin"], &[]).unwrap_or_default();
        if let Ok(out) = run("flatpak", &["remotes", "--columns=name,url,options"], &[]) {
            for l in out.lines() {
                let f: Vec<&str> = l.split('\t').collect();
                if f.is_empty() || f[0].is_empty() {
                    continue;
                }
                let installed: Vec<String> = origins
                    .lines()
                    .filter_map(|o| o.split_once('\t'))
                    .filter(|(_, origin)| origin.trim() == f[0])
                    .map(|(a, _)| a.to_string())
                    .collect();
                r.repos.push(Repo {
                    source: Source::Flatpak,
                    name: f[0].to_string(),
                    url: f.get(1).unwrap_or(&"").to_string(),
                    suites: f.get(2).unwrap_or(&"").to_string(),
                    file: String::new(),
                    enabled: !f.get(2).unwrap_or(&"").contains("disabled"),
                    official: f[0] == "flathub",
                    issues: Vec::new(),
                    installed,
                    toggle: None,
                });
            }
        }
    }
    if sources.contains(&Source::Snap) {
        r.repos.push(Repo {
            source: Source::Snap,
            name: "Snap Store".into(),
            url: "https://snapcraft.io".into(),
            suites: String::new(),
            file: String::new(),
            enabled: true,
            official: true,
            issues: Vec::new(),
            installed: installed.iter().filter(|p| p.source == Source::Snap).map(|p| p.id.clone()).collect(),
            toggle: None,
        });
    }
    if sources.contains(&Source::Rpm) {
        let out = if which("dnf") {
            run("dnf", &["repolist", "--all", "-q"], &[])
        } else {
            run("zypper", &["-q", "lr", "-u"], &[])
        };
        for l in out.unwrap_or_default().lines().skip(1) {
            let f: Vec<&str> = if l.contains('|') { l.split('|').map(str::trim).collect() } else { l.split_whitespace().collect() };
            if f.len() < 2 || f[0].starts_with('-') || f[0] == "#" {
                continue;
            }
            let enabled = l.contains("enabled") || l.contains("Yes");
            r.repos.push(Repo {
                source: Source::Rpm,
                name: f[0].to_string(),
                url: f.last().unwrap_or(&"").to_string(),
                suites: String::new(),
                file: String::new(),
                enabled,
                official: f[0].starts_with("fedora") || f[0].starts_with("updates") || f[0].starts_with("repo-"),
                issues: Vec::new(),
                installed: Vec::new(),
                toggle: None,
            });
        }
    }
    if sources.contains(&Source::Pacman) {
        if let Ok(conf) = std::fs::read_to_string("/etc/pacman.conf") {
            for l in conf.lines() {
                let t = l.trim();
                if t.starts_with('[') && t != "[options]" {
                    let n = t.trim_matches(['[', ']']).to_string();
                    r.repos.push(Repo {
                        source: Source::Pacman,
                        official: matches!(n.as_str(), "core" | "extra" | "multilib" | "community"),
                        name: n,
                        url: String::new(),
                        suites: String::new(),
                        file: "/etc/pacman.conf".into(),
                        enabled: true,
                        issues: Vec::new(),
                        installed: Vec::new(),
                        toggle: None,
                    });
                }
            }
        }
        if let Ok(out) = run("pacman", &["-Qmq"], &[1]) {
            r.orphans = out.lines().map(String::from).collect();
        }
    }
    // Third-party with issues first, then third-party, then official.
    r.repos.sort_by_key(|x| (x.official, x.issues.is_empty(), !x.enabled, x.name.to_lowercase()));
    r
}

pub fn refresh_action(sources: &[Source]) -> Option<Action> {
    let cmd = if sources.contains(&Source::Apt) {
        vec!["apt-get", "update"]
    } else if sources.contains(&Source::Rpm) && which("dnf") {
        vec!["dnf", "makecache"]
    } else if sources.contains(&Source::Pacman) {
        vec!["pacman", "-Sy"]
    } else {
        return None;
    };
    Some(Action {
        label: "Refresh package lists".into(),
        kind: ActionKind::Root(cmd.into_iter().map(String::from).collect()),
        confirm: None,
    })
}
