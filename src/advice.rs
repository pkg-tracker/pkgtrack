//! Recommendations: is it safe to remove, duplicates, cleanup, telemetry fixes.
use crate::backends::{self, run, run_privileged, run_user, which};
use crate::model::{Details, Kind, Package, Source};
use crate::telemetry::{self, Finding, Fix, Level};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

// ------------------------------------------------------------ verdict

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    Safe,
    Caution,
    Danger,
}

impl Risk {
    pub fn label(self) -> &'static str {
        match self {
            Risk::Safe => "Safe to remove",
            Risk::Caution => "Remove with care",
            Risk::Danger => "Don't remove",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Verdict {
    pub risk: Risk,
    pub headline: String,
    pub reasons: Vec<String>,
}

/// Packages whose removal takes the graphical desktop, networking or boot with it.
const CRITICAL: &[&str] = &[
    "gnome-shell", "gdm3", "gdm", "sddm", "lightdm", "plasma-desktop", "plasma-workspace", "xfce4-session",
    "cinnamon", "mate-session-manager", "network-manager", "networkmanager", "systemd", "sudo", "xorg",
    "xserver-xorg-core", "xwayland", "mutter", "pipewire", "pulseaudio", "wireplumber", "grub-pc",
    "grub-efi-amd64", "grub2", "linux-generic", "linux", "kernel-core", "polkitd", "dbus", "snapd", "flatpak",
    "apt", "dnf", "pacman", "zypper", "gnome-session", "gnome-control-center", "nautilus", "dolphin",
];

fn is_meta_desktop(n: &str) -> bool {
    n.ends_with("-desktop") || n.ends_with("-desktop-minimal") || n == "ubuntu-minimal" || n == "ubuntu-standard"
}

fn base(n: &str) -> &str {
    n.split(':').next().unwrap_or(n)
}

pub fn verdict(p: &Package, d: &Details, duplicates: usize) -> Verdict {
    let mut reasons = Vec::new();
    let mut risk = Risk::Safe;
    let me = base(&p.id);

    if p.protected {
        return Verdict {
            risk: Risk::Danger,
            headline: "Essential system package. Removing it can leave your system unbootable.".into(),
            reasons: vec!["Marked essential/required by your distribution.".into()],
        };
    }

    // Packages that would go away together with it.
    let extra: Vec<&str> = d
        .would_remove
        .iter()
        .flatten()
        .map(|s| base(s))
        .filter(|n| *n != me)
        .collect();
    let own: Vec<&str> = extra
        .iter()
        .copied()
        .filter(|n| n.starts_with(&format!("{me}-")) || me.starts_with(&format!("{n}-")) || n.starts_with("lib") && n.contains(me))
        .collect();
    let others: Vec<&str> = extra.iter().copied().filter(|n| !own.contains(n)).collect();
    let critical: Vec<&str> = others.iter().copied().filter(|n| CRITICAL.contains(n)).collect();
    let meta: Vec<&str> = others.iter().copied().filter(|n| is_meta_desktop(n)).collect();

    if CRITICAL.contains(&me) && p.kind != Kind::Library {
        risk = Risk::Danger;
        reasons.push("Core part of your system (desktop, networking, boot or package management).".into());
    }
    const DESKTOP_TOOLS: &[&str] = &[
        "network-manager-gnome", "gnome-control-center", "nautilus", "gnome-terminal", "yelp", "gnome-disk-utility",
        "software-properties-gtk", "update-manager", "gnome-system-monitor", "plasma-nm", "konsole", "dolphin", "systemsettings",
    ];
    if DESKTOP_TOOLS.contains(&me) {
        risk = risk.max(Risk::Caution);
        reasons.push("A standard tool of your desktop; settings panels or menus may expect it.".into());
    }
    if !critical.is_empty() {
        risk = Risk::Danger;
        reasons.push(format!("Would also remove {}, which your system needs.", critical.join(", ")));
    }
    if !meta.is_empty() {
        risk = risk.max(Risk::Caution);
        reasons.push(format!(
            "Would also remove the meta-package {}. Nothing visible breaks, but future upgrades may miss new desktop components.",
            meta.join(", ")
        ));
    }
    let plain: Vec<&str> = others.iter().copied().filter(|n| !critical.contains(n) && !meta.contains(n)).collect();
    if !plain.is_empty() {
        risk = risk.max(Risk::Caution);
        let preview: Vec<&str> = plain.iter().copied().take(8).collect();
        reasons.push(format!(
            "Would also remove {} other package(s): {}{}",
            plain.len(),
            preview.join(", "),
            if plain.len() > 8 { ", …" } else { "" }
        ));
    }
    if !own.is_empty() {
        reasons.push(format!("Its own add-ons go with it: {}.", own.join(", ")));
    }
    if d.would_remove.is_none() {
        let hard: Vec<&String> = d.used_by.iter().filter(|u| !u.ends_with("(optional)")).collect();
        if !hard.is_empty() {
            risk = risk.max(Risk::Caution);
            let preview: Vec<&str> = hard.iter().take(8).map(|s| s.as_str()).collect();
            reasons.push(format!(
                "{} installed package(s) need it and may stop working: {}{}",
                hard.len(),
                preview.join(", "),
                if hard.len() > 8 { ", …" } else { "" }
            ));
        }
    }
    if !d.running.is_empty() {
        risk = risk.max(Risk::Caution);
        reasons.push(format!("Running right now ({} process(es)); close it first.", d.running.len()));
    }
    if duplicates > 0 {
        reasons.push("Another copy is installed from a different source (see Suggestions → Duplicates).".into());
    }
    let auto = d.info.iter().any(|(k, v)| k == "Installed as" && v.starts_with("automatically"));
    if auto && extra.is_empty() && d.used_by.is_empty() {
        reasons.push("Installed automatically as a dependency, and nothing needs it any more.".into());
    }

    let headline = match risk {
        Risk::Danger => "Removing this would break your system or desktop.".to_string(),
        Risk::Caution => "Removing it affects other software, or an app that's open right now.".to_string(),
        Risk::Safe if p.kind == Kind::App => "Nothing else depends on it. Safe to remove if you don't use this app.".to_string(),
        Risk::Safe => "Nothing else depends on it. Safe to remove.".to_string(),
    };
    Verdict { risk, headline, reasons }
}

// ------------------------------------------------------------ actions

#[derive(Debug, Clone)]
pub enum ActionKind {
    Root(Vec<String>),
    RootScript(String),
    User(Vec<String>),
    /// Move paths to the Trash (recoverable).
    Trash(Vec<String>),
    VsCodeOff,
    Env(String),
    Remove(Vec<Package>),
    /// Permanently delete paths (only allowed inside ~/.cache).
    Delete(Vec<String>),
    /// Enable/disable a login app (desktop file name).
    Autostart(String, bool),
    /// Daily update notification on/off.
    Notify(bool),
    /// Several steps in order; stops at the first failure.
    Many(Vec<ActionKind>),
    InstallAppImage(Box<crate::backends::appimage::Info>),
}

#[derive(Debug, Clone)]
pub struct Action {
    pub label: String,
    pub kind: ActionKind,
    /// Confirmation question; None = runs immediately.
    pub confirm: Option<String>,
}

pub fn fix_action(fix: Fix) -> Action {
    let (label, kind) = match fix {
        Fix::Units(u) => (
            "Turn off".to_string(),
            ActionKind::Root(
                ["systemctl", "disable", "--now"].iter().chain(u.iter()).map(|s| s.to_string()).collect(),
            ),
        ),
        Fix::Script(s) => ("Turn off".into(), ActionKind::RootScript(s.into())),
        Fix::User(a) => ("Turn off".into(), ActionKind::User(a.iter().map(|s| s.to_string()).collect())),
        Fix::VsCode => ("Turn off in VS Code settings".into(), ActionKind::VsCodeOff),
        Fix::Env(v) => ("Opt out".into(), ActionKind::Env(v.into())),
    };
    Action { label, kind, confirm: None }
}

pub fn describe(kind: &ActionKind) -> String {
    match kind {
        ActionKind::Root(a) if a.len() > 10 => {
            format!("sudo {} … (+{} more)", a[..10].join(" "), a.len() - 10)
        }
        ActionKind::Root(a) => format!("sudo {}", a.join(" ")),
        ActionKind::RootScript(s) => format!("sudo sh -c '{}'", s.split_whitespace().collect::<Vec<_>>().join(" ")),
        ActionKind::User(a) => a.join(" "),
        ActionKind::Trash(p) => format!("Move {} folder(s) to the Trash", p.len()),
        ActionKind::VsCodeOff => format!(
            "Set \"telemetry.telemetryLevel\": \"off\" in {} (a backup is kept)",
            telemetry::vscode_settings_path().unwrap_or_else(|| "~/.config/Code/User/settings.json".into())
        ),
        ActionKind::Env(v) => format!("Add {v}=1 to ~/{} (applies after you log in again)", telemetry::ENV_FILE),
        ActionKind::Remove(p) => format!(
            "Uninstall {}",
            p.iter().map(|p| format!("{} ({})", p.name, p.source.short())).collect::<Vec<_>>().join(", ")
        ),
        ActionKind::Delete(p) => format!("Permanently delete {} folder(s) in ~/.cache", p.len()),
        ActionKind::Autostart(f, on) => {
            format!("{} {f} at login (per-user setting in ~/.config/autostart)", if *on { "Start" } else { "Don't start" })
        }
        ActionKind::Notify(on) => format!("{} a daily systemd user timer", if *on { "Create" } else { "Remove" }),
        ActionKind::Many(steps) => steps.iter().map(describe).collect::<Vec<_>>().join("; then "),
        ActionKind::InstallAppImage(i) => format!(
            "Move {} to ~/Applications and add {} to your app menu (no password needed)",
            std::path::Path::new(&i.staged).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
            i.name
        ),
    }
}

pub fn run_action(kind: &ActionKind) -> Result<String> {
    match kind {
        ActionKind::Root(a) => {
            let a: Vec<&str> = a.iter().map(String::as_str).collect();
            run_privileged(&a)
        }
        ActionKind::RootScript(s) => run_privileged(&["sh", "-c", s]),
        ActionKind::User(a) => {
            if !which(&a[0]) {
                bail!("{} is not installed.", a[0]);
            }
            let rest: Vec<&str> = a[1..].iter().map(String::as_str).collect();
            run_user(&a[0], &rest)
        }
        ActionKind::Trash(paths) => {
            for p in paths {
                run_user("gio", &["trash", p])?;
            }
            Ok(format!("Moved {} folder(s) to the Trash.", paths.len()))
        }
        ActionKind::VsCodeOff => vscode_off(),
        ActionKind::Env(var) => env_opt_out(var),
        ActionKind::Remove(pkgs) => {
            let mut log = String::new();
            for p in pkgs {
                log.push_str(&backends::remove(p)?);
            }
            Ok(log)
        }
        ActionKind::Delete(paths) => {
            let home = std::env::var("HOME")?;
            let cache = format!("{home}/.cache/");
            let mut freed = 0;
            let mut failed = Vec::new();
            for p in paths {
                let canon = std::fs::canonicalize(p)?;
                if !canon.to_string_lossy().starts_with(&cache) {
                    bail!("Refusing to delete {p}: outside ~/.cache");
                }
                let before = dir_size(&canon);
                // Some tools (e.g. Bazel) leave folders with no permissions; unlock ours first.
                make_writable(&canon);
                if let Err(e) = std::fs::remove_dir_all(&canon) {
                    failed.push(format!("{} ({e})", p.replace(&home, "~")));
                }
                freed += before.saturating_sub(dir_size(&canon));
            }
            let msg = format!("Freed {}.", crate::model::human_size(freed));
            if failed.is_empty() {
                Ok(msg)
            } else {
                bail!("{msg} Could not fully delete: {}. Files there may belong to another user or be in use; close the app that uses them and try again.", failed.join(", "))
            }
        }
        ActionKind::Autostart(file, on) => crate::startup::set_autostart(file, *on),
        ActionKind::Notify(on) => crate::startup::set_notify(*on),
        ActionKind::InstallAppImage(i) => crate::backends::appimage::install(i),
        ActionKind::Many(steps) => {
            let mut last = String::new();
            for s in steps {
                last = run_action(s)?;
            }
            Ok(last)
        }
    }
}

fn vscode_off() -> Result<String> {
    let home = std::env::var("HOME")?;
    let path = telemetry::vscode_settings_path().unwrap_or(format!("{home}/.config/Code/User/settings.json"));
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let key = "\"telemetry.telemetryLevel\"";
    let new = if let Some(line) = old.lines().find(|l| l.contains(key) && !l.trim_start().starts_with("//")) {
        let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        let comma = if line.trim_end().ends_with(',') { "," } else { "" };
        old.replacen(line, &format!("{indent}{key}: \"off\"{comma}"), 1)
    } else if let Some(i) = old.find('{') {
        let rest = old[i + 1..].trim_start();
        let comma = if rest.starts_with('}') { "" } else { "," };
        format!("{}{{\n    {key}: \"off\"{comma}\n{}", &old[..i], &old[i + 1..].trim_start_matches(['\n', '\r']))
    } else {
        format!("{{\n    {key}: \"off\"\n}}\n")
    };
    if !old.is_empty() {
        std::fs::write(format!("{path}.pkgtrack-backup"), &old).context("could not write backup")?;
    } else if let Some(dir) = Path::new(&path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, new).with_context(|| format!("could not write {path}"))?;
    Ok("VS Code telemetry turned off. VS Code picks up the change immediately.".into())
}

fn env_opt_out(var: &str) -> Result<String> {
    let home = std::env::var("HOME")?;
    let path = format!("{home}/{}", telemetry::ENV_FILE);
    let mut content = std::fs::read_to_string(&path).unwrap_or_default();
    if !content.lines().any(|l| l.starts_with(&format!("{var}="))) {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("{var}=1\n"));
        std::fs::create_dir_all(Path::new(&path).parent().unwrap())?;
        std::fs::write(&path, content)?;
    }
    Ok(format!("{var}=1 saved. It applies to programs started after your next login."))
}

/// One action that updates everything (one password prompt for root steps).
pub fn update_all_action(pkgs: &[Package]) -> Option<Action> {
    let count = |s: Source| pkgs.iter().filter(|p| p.source == s && p.new_version.is_some()).count();
    let mut root: Vec<String> = Vec::new();
    let mut steps = Vec::new();
    let mut parts = Vec::new();
    if count(Source::Apt) > 0 {
        root.push("DEBIAN_FRONTEND=noninteractive apt-get upgrade -y --with-new-pkgs -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold".into());
        parts.push(format!("{} deb", count(Source::Apt)));
    }
    if count(Source::Rpm) > 0 {
        root.push(if which("dnf") { "dnf upgrade -y" } else { "zypper -n update" }.into());
        parts.push(format!("{} rpm", count(Source::Rpm)));
    }
    if count(Source::Pacman) > 0 {
        root.push("pacman -Syu --noconfirm".into());
        parts.push(format!("{} pacman", count(Source::Pacman)));
    }
    if count(Source::Snap) > 0 {
        let snaps: Vec<&Package> = pkgs.iter().filter(|p| p.source == Source::Snap && p.new_version.is_some()).collect();
        root.push(snap_refresh_script(&snaps));
        parts.push(format!("{} snap", count(Source::Snap)));
    }
    if !root.is_empty() {
        steps.push(ActionKind::RootScript(root_script(&root)));
    }
    if count(Source::Flatpak) > 0 {
        steps.push(ActionKind::User(["flatpak", "update", "-y", "--noninteractive"].map(String::from).to_vec()));
        parts.push(format!("{} flatpak", count(Source::Flatpak)));
    }
    if steps.is_empty() {
        return None;
    }
    Some(Action {
        label: "Update all".into(),
        confirm: Some(format!(
            "Install all updates ({})? Snaps that are running are updated once you close them.",
            parts.join(", ")
        )),
        kind: ActionKind::Many(steps),
    })
}

/// Runs each command even if an earlier one fails; exits 1 if any failed.
/// Commands that handle their own failures (see `snap_refresh_script`) end in `:`.
fn root_script<S: AsRef<str>>(cmds: &[S]) -> String {
    let body = cmds
        .iter()
        .map(|c| {
            let c = c.as_ref();
            if c.ends_with(':') { c.to_string() } else { format!("{c} || rc=1") }
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("rc=0; {body}; exit $rc")
}

/// Refresh snaps one at a time: snapd refuses the whole batch when one of
/// them is running, and a running snap isn't an error (snapd updates it
/// once it's closed).
fn snap_refresh_script(snaps: &[&Package]) -> String {
    let names = snaps.iter().map(|p| sh_quote(&p.id)).collect::<Vec<_>>().join(" ");
    format!(
        "for s in {names}; do o=$(snap refresh \"$s\" 2>&1) || case \"$o\" in \
         *'has running apps'*) echo \"$s is open; it updates once you close it.\";; \
         *) echo \"$o\"; rc=1;; esac; done; :"
    )
}

/// Single-quote a word for `sh -c`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// One action that updates only the chosen packages (one password prompt for root steps).
pub fn update_selected_action(pkgs: &[&Package]) -> Option<Action> {
    let pick = |s: Source| -> Vec<&Package> {
        pkgs.iter().copied().filter(|p| p.source == s && p.new_version.is_some()).collect()
    };
    let ids = |v: &[&Package]| v.iter().map(|p| sh_quote(&p.id)).collect::<Vec<_>>().join(" ");
    let mut root = Vec::new();
    let mut steps = Vec::new();
    let mut parts = Vec::new();
    let mut notes = Vec::new();
    let apt = pick(Source::Apt);
    if !apt.is_empty() {
        root.push(format!(
            "DEBIAN_FRONTEND=noninteractive apt-get install --only-upgrade -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold {}",
            ids(&apt)
        ));
        parts.push(format!("{} deb", apt.len()));
    }
    let rpm = pick(Source::Rpm);
    if !rpm.is_empty() {
        let cmd = if which("dnf") { "dnf upgrade -y" } else { "zypper -n update" };
        root.push(format!("{cmd} {}", ids(&rpm)));
        parts.push(format!("{} rpm", rpm.len()));
    }
    let pacman = pick(Source::Pacman);
    if !pacman.is_empty() {
        // Arch does not support partial upgrades.
        root.push("pacman -Syu --noconfirm".into());
        parts.push(format!("{} pacman", pacman.len()));
        notes.push("Pacman can't update single packages, so the whole system is updated.");
    }
    let snap = pick(Source::Snap);
    if !snap.is_empty() {
        root.push(snap_refresh_script(&snap));
        parts.push(format!("{} snap", snap.len()));
        notes.push("Snaps that are running are updated once you close them.");
    }
    if !root.is_empty() {
        steps.push(ActionKind::RootScript(root_script(&root)));
    }
    let flatpak = pick(Source::Flatpak);
    if !flatpak.is_empty() {
        let mut by_inst: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for p in &flatpak {
            by_inst
                .entry(backends::flatpak::installation(p))
                .or_default()
                .push(backends::flatpak::pkg_ref(p).to_string());
        }
        for (inst, refs) in by_inst {
            let mut cmd: Vec<String> = ["flatpak", "update", "-y", "--noninteractive"].map(String::from).to_vec();
            cmd.push(format!("--{inst}"));
            cmd.extend(refs);
            steps.push(ActionKind::User(cmd));
        }
        parts.push(format!("{} flatpak", flatpak.len()));
    }
    if steps.is_empty() {
        return None;
    }
    let n: usize = [&apt, &rpm, &pacman, &snap, &flatpak].iter().map(|v| v.len()).sum();
    let mut confirm = format!("Update {n} selected package(s) ({})?", parts.join(", "));
    for note in notes {
        confirm.push(' ');
        confirm.push_str(note);
    }
    Some(Action { label: "Update selected".into(), confirm: Some(confirm), kind: ActionKind::Many(steps) })
}

// ------------------------------------------------------------ scan

#[derive(Debug, Clone)]
pub struct Member {
    pub pkg: Package,
    pub verdict: Verdict,
    pub running: bool,
    pub keep: bool,
    pub note: String,
}

#[derive(Debug, Clone)]
pub struct DupGroup {
    pub name: String,
    pub members: Vec<Member>,
    pub advice: String,
}

#[derive(Debug, Clone)]
pub struct Cleanup {
    pub title: String,
    pub detail: String,
    pub items: Vec<String>,
    pub size: Option<u64>,
    pub risk: Risk,
    pub action: Option<Action>,
}

#[derive(Debug, Clone)]
pub struct TelemetryGroup {
    pub finding: Finding,
    pub members: Vec<Member>,
    /// Remove instead of disabling?
    pub recommend_remove: bool,
    pub advice: String,
    pub fix: Option<Action>,
}

#[derive(Debug, Clone)]
pub struct Unused {
    pub member: Member,
    /// e.g. "Last opened 5 months ago"
    pub when: String,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct Suggestions {
    pub unused: Vec<Unused>,
    pub telemetry: Vec<TelemetryGroup>,
    pub duplicates: Vec<DupGroup>,
    pub cleanup: Vec<Cleanup>,
    /// System problems (broken installs, drivers, failed services, disk space…).
    pub issues: Vec<crate::health::Issue>,
}

/// Names used to spot the same app installed from different sources.
fn dup_keys(p: &Package) -> Vec<String> {
    let norm = |s: &str| s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    const GENERIC: &[&str] = &["client", "desktop", "app", "application", "platform", "stable", "studio"];
    let mut keys = Vec::new();
    match p.source {
        Source::Flatpak => {
            if p.kind != Kind::App {
                return keys;
            }
            if let Some(last) = p.id.rsplit('.').next() {
                if !GENERIC.contains(&last.to_lowercase().as_str()) {
                    keys.push(norm(last));
                }
            }
            keys.push(norm(&p.name));
        }
        Source::Snap => {
            if p.kind != Kind::App {
                return keys;
            }
            keys.push(norm(&p.id));
        }
        _ => {
            let transitional = p.version.contains("snap");
            if p.kind != Kind::App && !transitional {
                return keys;
            }
            let n = base(&p.id);
            keys.push(norm(n));
            for suf in ["-stable", "-bin", "-desktop", "-client", "-installer", "-launcher"] {
                if let Some(s) = n.strip_suffix(suf) {
                    keys.push(norm(s));
                }
            }
        }
    }
    // Well-known aliases.
    let aliases: &[(&str, &str)] = &[("visualstudiocode", "code"), ("googlechrome", "chrome"), ("chromium", "chromiumbrowser")];
    for (a, b) in aliases {
        if keys.iter().any(|k| k == a) {
            keys.push(b.to_string());
        }
    }
    keys.retain(|k| k.len() >= 2);
    keys.dedup();
    keys
}

fn version_tuple(v: &str) -> Option<Vec<u64>> {
    let v = v.split(':').next_back()?; // strip epoch
    let parts: Vec<u64> = v
        .split(|c: char| c == '.' || c == '-' || c == '+' || c == '~')
        .take(3)
        .map_while(|x| x.parse().ok())
        .collect();
    (parts.len() >= 2).then_some(parts)
}

pub fn find_duplicates(all: &[Package]) -> Vec<Vec<Package>> {
    let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, p) in all.iter().enumerate() {
        for k in dup_keys(p) {
            by_key.entry(k).or_default().push(i);
        }
    }
    let mut seen: HashSet<Vec<usize>> = HashSet::new();
    let mut groups = Vec::new();
    for (_, mut idx) in by_key {
        idx.sort();
        idx.dedup();
        let sources: HashSet<Source> = idx.iter().map(|&i| all[i].source).collect();
        if sources.len() < 2 || !idx.iter().any(|&i| all[i].kind == Kind::App) {
            continue;
        }
        if seen.insert(idx.clone()) {
            groups.push(idx.iter().map(|&i| all[i].clone()).collect());
        }
    }
    groups
}

fn details_parallel(pkgs: &[Package]) -> Vec<Details> {
    let mut out: Vec<Option<Details>> = vec![None; pkgs.len()];
    std::thread::scope(|s| {
        for (chunk_out, chunk_in) in out.chunks_mut(4).zip(pkgs.chunks(4)) {
            s.spawn(move || {
                for (o, p) in chunk_out.iter_mut().zip(chunk_in) {
                    *o = Some(backends::details(p));
                }
            });
        }
    });
    out.into_iter().map(Option::unwrap_or_default).collect()
}

pub fn scan(all: Vec<Package>, sources: Vec<Source>) -> Suggestions {
    // Health checks only run small read-only commands; do them alongside.
    let health = {
        let sources = sources.clone();
        std::thread::spawn(move || crate::health::check(&sources))
    };
    let dup_groups = find_duplicates(&all);
    let dup_ids: HashSet<(Source, String)> =
        dup_groups.iter().flatten().map(|p| (p.source, p.id.clone())).collect();

    // Telemetry-flagged packages, grouped by finding.
    let flagged: Vec<Package> = all.iter().filter(|p| telemetry::quick_level(p).is_some()).cloned().collect();
    let mut to_analyze = flagged.clone();
    for p in dup_groups.iter().flatten() {
        if !to_analyze.iter().any(|x| x.source == p.source && x.id == p.id) {
            to_analyze.push(p.clone());
        }
    }
    // Apps not opened for a long time (only judged when we have usage data).
    const DAY: u64 = 86_400;
    let mut unused_candidates: Vec<(Package, String)> = Vec::new();
    for p in all.iter().filter(|p| p.kind == Kind::App) {
        if installed_recently(p) {
            continue;
        }
        let files = if matches!(p.source, Source::Apt | Source::Rpm | Source::Pacman) { backends::native_files(p) } else { Vec::new() };
        let desktop: Vec<String> = files.iter().filter(|f| f.ends_with(".desktop") && f.contains("/applications/")).cloned().collect();
        if !crate::usage::has_visible_launcher(p, &desktop) {
            continue; // libraries, runtimes, helpers: not something you "open"
        }
        // 1. GNOME's record (exact, but only kept for ~25 most-used apps).
        // 2. Sandboxed apps: their data folder in your home.
        // 3. Native apps: when the program file was last read.
        let when = match crate::usage::last_used(p, &desktop) {
            Some(ago) if ago > 90 * DAY => Some(format!("Last opened {}", crate::usage::human_ago(ago))),
            Some(_) => None,
            None => match crate::usage::data_dir_age(p) {
                Some(None) => Some("Never started".to_string()),
                Some(Some(ago)) if ago > 90 * DAY => Some(format!("Last used {}", crate::usage::human_ago(ago))),
                Some(Some(_)) => None,
                None => match crate::usage::last_access(&files) {
                    Some(ago) if ago > 90 * DAY => Some(format!("Last used {}", crate::usage::human_ago(ago))),
                    _ => None,
                },
            },
        };
        if let Some(w) = when {
            unused_candidates.push((p.clone(), w));
        }
    }
    unused_candidates.sort_by(|a, b| b.0.size_bytes.cmp(&a.0.size_bytes));
    unused_candidates.truncate(30);
    for (p, _) in &unused_candidates {
        if !to_analyze.iter().any(|x| x.source == p.source && x.id == p.id) {
            to_analyze.push(p.clone());
        }
    }
    let details = details_parallel(&to_analyze);
    let info: HashMap<(Source, String), &Details> =
        to_analyze.iter().zip(&details).map(|(p, d)| ((p.source, p.id.clone()), d)).collect();
    let member = |p: &Package| -> Member {
        let d = info.get(&(p.source, p.id.clone())).copied().cloned().unwrap_or_default();
        let dup = dup_ids.contains(&(p.source, p.id.clone())) as usize;
        Member { verdict: verdict(p, &d, dup), running: !d.running.is_empty(), pkg: p.clone(), keep: false, note: String::new() }
    };

    let unused: Vec<Unused> = unused_candidates
        .iter()
        .map(|(p, when)| Unused { member: member(p), when: when.clone(), size: p.size_bytes })
        .filter(|u| u.member.verdict.risk != Risk::Danger && !u.member.running)
        .collect();

    // ---- duplicates
    let mut duplicates = Vec::new();
    for g in dup_groups {
        let mut members: Vec<Member> = g.iter().map(&member).collect();
        for m in &mut members {
            if m.pkg.version.contains("snap") && m.pkg.source == Source::Apt {
                m.note = "Transitional package: it only points to the Snap version.".into();
            }
        }
        let candidates: Vec<usize> = (0..members.len()).filter(|&i| members[i].note.is_empty()).collect();
        if candidates.len() < 2 {
            // e.g. Ubuntu's "firefox" deb that only points to the Snap: one real copy, nothing to do.
            continue;
        }
        let running: Vec<usize> = candidates.iter().copied().filter(|&i| members[i].running).collect();
        let keep = if running.len() == 1 {
            Some((running[0], "it's the one running right now".to_string()))
        } else {
            let mut best: Option<(usize, Vec<u64>)> = None;
            for &i in &candidates {
                if let Some(v) = version_tuple(&members[i].pkg.version) {
                    if best.as_ref().is_none_or(|(_, b)| v > *b) {
                        best = Some((i, v));
                    }
                }
            }
            let comparable = candidates.iter().filter(|&&i| version_tuple(&members[i].pkg.version).is_some()).count();
            best.filter(|_| comparable == candidates.len() && comparable > 1)
                .map(|(i, _)| (i, "it has the newest version".to_string()))
        };
        let name = members.iter().find(|m| m.pkg.kind == Kind::App).map(|m| m.pkg.name.clone()).unwrap_or_default();
        let advice = match &keep {
            Some((i, why)) => {
                members[*i].keep = true;
                let others: Vec<String> = members
                    .iter()
                    .enumerate()
                    .filter(|(j, m)| j != i && m.note.is_empty())
                    .map(|(_, m)| format!("the {} copy", m.pkg.source.label()))
                    .collect();
                if others.is_empty() {
                    format!("Keep the {} version ({why}). The other entry is harmless.", members[*i].pkg.source.label())
                } else {
                    format!(
                        "Keep the {} version ({why}) and remove {}. Settings in your home folder are usually shared, so you lose nothing.",
                        members[*i].pkg.source.label(),
                        others.join(" and ")
                    )
                }
            }
            None if candidates.len() <= 1 => "Only one real copy is installed; the other entry is a pointer to it. No action needed.".into(),
            None => "Keep the copy you start from the app menu and remove the other. Open the app and check About to see which one runs.".into(),
        };
        duplicates.push(DupGroup { name, members, advice });
    }
    duplicates.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    // ---- telemetry groups
    let mut groups: BTreeMap<String, TelemetryGroup> = BTreeMap::new();
    for p in &flagged {
        let d = info.get(&(p.source, p.id.clone())).copied().cloned().unwrap_or_default();
        let Some(f) = d.telemetry.iter().find(|f| f.level > Level::Info && !f.title.starts_with("Depends on")).cloned()
        else {
            continue;
        };
        let g = groups.entry(f.title.clone()).or_insert_with(|| TelemetryGroup {
            fix: f.fix.map(fix_action),
            finding: f.clone(),
            members: Vec::new(),
            recommend_remove: false,
            advice: String::new(),
        });
        g.members.push(member(p));
    }
    let mut telemetry: Vec<TelemetryGroup> = groups.into_values().collect();
    for g in &mut telemetry {
        let system = telemetry::is_system_component(&g.finding.title);
        let is_app = !system && g.members.iter().any(|m| m.pkg.kind == Kind::App);
        let already_off = g.finding.off == Some(true);
        let safe: Vec<&str> =
            g.members.iter().filter(|m| m.verdict.risk == Risk::Safe).map(|m| m.pkg.name.as_str()).collect();
        let unsafe_: Vec<&Member> = g.members.iter().filter(|m| m.verdict.risk != Risk::Safe).collect();
        let has_fix = g.fix.is_some();
        g.recommend_remove = !is_app && unsafe_.is_empty() && !already_off;
        let keep_note = || {
            // Group packages that share the same reason.
            let mut by_reason: Vec<(String, Vec<&str>)> = Vec::new();
            for m in &unsafe_ {
                if by_reason.iter().any(|(_, n)| n.contains(&m.pkg.name.as_str())) {
                    continue; // same name from another source
                }
                let r = m.verdict.reasons.first().map(|r| r.trim_end_matches('.').to_string()).unwrap_or_default();
                match by_reason.iter_mut().find(|(x, _)| *x == r) {
                    Some((_, names)) if !names.contains(&m.pkg.name.as_str()) => names.push(&m.pkg.name),
                    Some(_) => {}
                    None => by_reason.push((r, vec![&m.pkg.name])),
                }
            }
            by_reason
                .into_iter()
                .map(|(r, names)| {
                    let mut r = r;
                    if let Some(c) = r.get(..1) {
                        r = c.to_lowercase() + &r[1..];
                    }
                    if r.is_empty() { names.join(", ") } else { format!("{} ({r})", names.join(", ")) }
                })
                .collect::<Vec<_>>()
                .join("; ")
        };
        g.advice = if already_off {
            "Already turned off on this computer. Nothing to do.".into()
        } else if is_app {
            if has_fix {
                "You probably use this app, so keep it and turn off its telemetry with the button below.".into()
            } else {
                "You probably use this app, so keep it and limit its telemetry with the manual steps below.".into()
            }
        } else if g.recommend_remove {
            if has_fix {
                "Nothing depends on it, so you can uninstall it. If you'd rather keep it, use Turn off instead.".into()
            } else {
                "Nothing depends on it, so you can simply uninstall it.".into()
            }
        } else if !safe.is_empty() {
            format!(
                "{}You can uninstall {}, but keep {}.",
                if has_fix { "Simplest: Turn off with one click. " } else { "" },
                safe.join(", "),
                keep_note()
            )
        } else if has_fix {
            format!("Don't uninstall it: {}. Turn it off instead.", keep_note())
        } else {
            format!("Don't uninstall it: {}. Limit it with the manual steps below.", keep_note())
        };
    }
    telemetry.sort_by(|a, b| {
        (a.finding.off == Some(true), std::cmp::Reverse(a.finding.level), a.finding.title.clone())
            .cmp(&(b.finding.off == Some(true), std::cmp::Reverse(b.finding.level), b.finding.title.clone()))
    });

    // ---- cleanup
    let mut cleanup = Vec::new();
    let size_of = |names: &[String], src: Source| -> Option<u64> {
        let s: u64 = all
            .iter()
            .filter(|p| p.source == src && names.iter().any(|n| base(n) == base(&p.id)))
            .filter_map(|p| p.size_bytes)
            .sum();
        (s > 0).then_some(s)
    };
    for &src in &sources {
        match src {
            Source::Apt => {
                if let Ok(out) = run("apt-get", &["-s", "autoremove"], &[100]) {
                    let items: Vec<String> = out
                        .lines()
                        .filter_map(|l| l.strip_prefix("Remv "))
                        .filter_map(|l| l.split_whitespace().next())
                        .map(String::from)
                        .collect();
                    if !items.is_empty() {
                        cleanup.push(Cleanup {
                            title: "Unused dependencies".into(),
                            detail: "Installed automatically for software you've since removed. Nothing uses them now.".into(),
                            size: size_of(&items, src),
                            risk: Risk::Safe,
                            action: Some(Action {
                                label: "Remove all".into(),
                                kind: ActionKind::Root(vec!["apt-get".into(), "autoremove".into(), "-y".into()]),
                                confirm: Some(format!("Remove {} unused dependencies?", items.len())),
                            }),
                            items,
                        });
                    }
                }
                if let Ok(out) = run("dpkg-query", &["-W", "-f=${db:Status-Abbrev}\t${binary:Package}\n"], &[]) {
                    let items: Vec<String> = out
                        .lines()
                        .filter_map(|l| l.strip_prefix("rc"))
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .collect();
                    if !items.is_empty() {
                        let mut argv = vec!["apt-get".to_string(), "purge".into(), "-y".into()];
                        argv.extend(items.iter().cloned());
                        cleanup.push(Cleanup {
                            title: "Leftover configuration of removed packages".into(),
                            detail: "These packages were uninstalled, but their system config files in /etc are still there. Purging removes only those files.".into(),
                            size: None,
                            risk: Risk::Safe,
                            action: Some(Action {
                                label: "Purge leftovers".into(),
                                kind: ActionKind::Root(argv),
                                confirm: Some(format!("Delete leftover config files of {} removed packages?", items.len())),
                            }),
                            items,
                        });
                    }
                }
            }
            Source::Pacman => {
                if let Ok(out) = run("pacman", &["-Qdtq"], &[1]) {
                    let items: Vec<String> = out.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
                    if !items.is_empty() {
                        let mut argv = vec!["pacman".to_string(), "-Rns".into(), "--noconfirm".into()];
                        argv.extend(items.iter().cloned());
                        cleanup.push(Cleanup {
                            title: "Orphaned packages".into(),
                            detail: "Installed as dependencies, but nothing requires them any more.".into(),
                            size: size_of(&items, src),
                            risk: Risk::Safe,
                            action: Some(Action {
                                label: "Remove all".into(),
                                kind: ActionKind::Root(argv),
                                confirm: Some(format!("Remove {} orphaned packages?", items.len())),
                            }),
                            items,
                        });
                    }
                }
            }
            Source::Rpm if which("dnf") => {
                if let Ok(out) = run("dnf", &["repoquery", "--unneeded", "-q", "--qf", "%{name}\n"], &[]) {
                    let items: Vec<String> = out.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
                    if !items.is_empty() {
                        cleanup.push(Cleanup {
                            title: "Unused dependencies".into(),
                            detail: "Installed automatically for software you've since removed.".into(),
                            size: size_of(&items, src),
                            risk: Risk::Safe,
                            action: Some(Action {
                                label: "Remove all".into(),
                                kind: ActionKind::Root(vec!["dnf".into(), "autoremove".into(), "-y".into()]),
                                confirm: Some(format!("Remove {} unused dependencies?", items.len())),
                            }),
                            items,
                        });
                    }
                }
            }
            Source::Flatpak => {
                let items = flatpak_unused();
                if !items.is_empty() {
                    cleanup.push(Cleanup {
                        title: "Unused Flatpak runtimes".into(),
                        detail: "Runtimes that no installed Flatpak app uses any more.".into(),
                        size: None,
                        risk: Risk::Safe,
                        action: Some(Action {
                            label: "Remove all".into(),
                            kind: ActionKind::User(
                                ["flatpak", "uninstall", "--unused", "-y", "--noninteractive"].map(String::from).to_vec(),
                            ),
                            confirm: Some(format!("Remove {} unused runtimes?", items.len())),
                        }),
                        items,
                    });
                }
                let installed: HashSet<&str> =
                    all.iter().filter(|p| p.source == Source::Flatpak).map(|p| p.id.as_str()).collect();
                leftover_data(&mut cleanup, ".var/app", "Flatpak", |n| installed.contains(n));
            }
            Source::Snap => {
                if let Some(c) = snap_old_revisions() {
                    cleanup.push(c);
                }
                let installed: HashSet<&str> =
                    all.iter().filter(|p| p.source == Source::Snap).map(|p| p.id.as_str()).collect();
                leftover_data(&mut cleanup, "snap", "Snap", |n| installed.contains(n));
            }
            _ => {}
        }
    }

    more_cleanup(&mut cleanup, &sources);
    let issues = health.join().unwrap_or_default();
    Suggestions { unused, telemetry, duplicates, cleanup, issues }
}

fn installed_recently(p: &Package) -> bool {
    crate::usage::installed_ago(p).is_some_and(|a| a < 30 * 86_400)
}

fn du_bytes(path: &str) -> u64 {
    dir_size(Path::new(path))
}

fn more_cleanup(cleanup: &mut Vec<Cleanup>, sources: &[Source]) {
    let root = |label: &str, argv: &[&str], confirm: &str| Action {
        label: label.into(),
        kind: ActionKind::Root(argv.iter().map(|s| s.to_string()).collect()),
        confirm: Some(confirm.into()),
    };
    // Downloaded package files.
    let (cache_dir, clean): (&str, Option<Vec<&str>>) = if sources.contains(&Source::Apt) {
        ("/var/cache/apt/archives", Some(vec!["apt-get", "clean"]))
    } else if sources.contains(&Source::Pacman) {
        ("/var/cache/pacman/pkg", Some(if which("paccache") { vec!["paccache", "-rk1"] } else { vec!["pacman", "-Sc", "--noconfirm"] }))
    } else if sources.contains(&Source::Rpm) && which("dnf") {
        ("/var/cache/dnf", Some(vec!["dnf", "clean", "packages"]))
    } else {
        ("", None)
    };
    if let Some(argv) = clean {
        let size = du_bytes(cache_dir);
        if size > 50_000_000 {
            cleanup.push(Cleanup {
                title: "Downloaded package files".into(),
                detail: "Copies of packages kept after installing. They're only needed to reinstall without internet.".into(),
                items: vec![cache_dir.to_string()],
                size: Some(size),
                risk: Risk::Safe,
                action: Some(root("Clear package cache", &argv, "Delete downloaded package files?")),
            });
        }
    }
    // System logs.
    if let Ok(out) = run("journalctl", &["--disk-usage"], &[]) {
        let size = out
            .split("take up ")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| crate::model::parse_human_size(&format!("{} {}", &s[..s.len() - 1], &s[s.len() - 1..])));
        if let Some(size) = size.filter(|s| *s > 700_000_000) {
            cleanup.push(Cleanup {
                title: "Old system logs".into(),
                detail: "The system journal keeps logs for months. Trimming it to 500 MB keeps recent logs for troubleshooting.".into(),
                items: vec!["systemd journal".into()],
                size: Some(size.saturating_sub(500_000_000)),
                risk: Risk::Safe,
                action: Some(root("Trim logs to 500 MB", &["journalctl", "--vacuum-size=500M"], "Delete older system logs, keeping the newest 500 MB?")),
            });
        }
    }
    // App caches in ~/.cache (sizes computed in parallel).
    if let Ok(home) = std::env::var("HOME") {
        let base = format!("{home}/.cache");
        let dirs: Vec<std::path::PathBuf> = std::fs::read_dir(&base)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        let sizes: Vec<u64> = std::thread::scope(|s| {
            let handles: Vec<_> = dirs.iter().map(|d| s.spawn(move || dir_size(d))).collect();
            handles.into_iter().map(|h| h.join().unwrap_or(0)).collect()
        });
        let mut big: Vec<(String, u64)> = dirs
            .iter()
            .zip(sizes)
            .filter(|(_, s)| *s > 100_000_000)
            .map(|(d, s)| (d.to_string_lossy().into_owned(), s))
            .collect();
        big.sort_by(|a, b| b.1.cmp(&a.1));
        if !big.is_empty() {
            let total: u64 = big.iter().map(|b| b.1).sum();
            cleanup.push(Cleanup {
                title: "App caches".into(),
                detail: "Temporary files apps keep to start faster. Apps recreate what they need, so the first start may be slower. Close the apps first.".into(),
                items: big.iter().map(|(p, s)| format!("{} ({})", p.replace(&home, "~"), crate::model::human_size(*s))).collect(),
                size: Some(total),
                risk: Risk::Safe,
                action: Some(Action {
                    label: "Delete caches".into(),
                    confirm: Some(format!("Permanently delete {} cache folders ({})?", big.len(), crate::model::human_size(total))),
                    kind: ActionKind::Delete(big.into_iter().map(|b| b.0).collect()),
                }),
            });
        }
        // Trash.
        let trash = format!("{home}/.local/share/Trash/files");
        let size = du_bytes(&trash);
        if size > 50_000_000 && which("gio") {
            cleanup.push(Cleanup {
                title: "Trash".into(),
                detail: "Files you deleted are still taking up space in the Trash.".into(),
                items: vec!["~/.local/share/Trash".into()],
                size: Some(size),
                risk: Risk::Caution,
                action: Some(Action {
                    label: "Empty Trash".into(),
                    kind: ActionKind::User(["gio", "trash", "--empty"].map(String::from).to_vec()),
                    confirm: Some("Empty the Trash? Files in it can't be restored afterwards.".into()),
                }),
            });
        }
    }
    // Keep old snap revisions from piling up again.
    if sources.contains(&Source::Snap) {
        let on = crate::startup::snap_clean_enabled();
        cleanup.push(Cleanup {
            title: if on { "Weekly snap cleanup is on".into() } else { "Clean old snap revisions automatically".into() },
            detail: "Snap already keeps the minimum (one old copy of each app, for rollback), but those copies add up. A weekly system task can remove them for you.".into(),
            items: Vec::new(),
            size: None,
            risk: Risk::Safe,
            action: Some(crate::startup::snap_clean_action(!on)),
        });
    }
}

fn flatpak_unused() -> Vec<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    // flatpak has no dry-run; it lists the refs and then asks. We answer "no".
    let Ok(mut child) = Command::new("flatpak")
        .args(["uninstall", "--unused"])
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"n\nn\nn\n");
    }
    let Ok(out) = child.wait_with_output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let n = f.next()?;
            (n.ends_with('.') && n.trim_end_matches('.').parse::<u32>().is_ok())
                .then(|| format!("{} {}", f.next().unwrap_or(""), f.next().unwrap_or("")))
        })
        .collect()
}

fn snap_old_revisions() -> Option<Cleanup> {
    let out = run("snap", &["list", "--all", "--color=never", "--unicode=never"], &[]).ok()?;
    let mut items = Vec::new();
    let mut script = Vec::new();
    let mut total = 0u64;
    for l in out.lines().skip(1) {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 6 || !f[5].contains("disabled") {
            continue;
        }
        let (name, rev) = (f[0], f[2]);
        let valid = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && rev.chars().all(|c| c.is_ascii_digit());
        if !valid {
            continue;
        }
        let size = std::fs::metadata(format!("/var/lib/snapd/snaps/{name}_{rev}.snap")).map(|m| m.len()).unwrap_or(0);
        total += size;
        items.push(format!("{name} (old revision {rev}, {})", crate::model::human_size(size)));
        script.push(format!("snap remove {name} --revision={rev}"));
    }
    if items.is_empty() {
        return None;
    }
    Some(Cleanup {
        title: "Old Snap revisions".into(),
        detail: "Snap keeps the previous version of each app after updating, so it can roll back. You can remove the old copies safely.".into(),
        size: Some(total),
        risk: Risk::Safe,
        action: Some(Action {
            label: "Remove old revisions".into(),
            confirm: Some(format!("Remove {} old snap revisions?", items.len())),
            kind: ActionKind::RootScript(script.join("; ")),
        }),
        items,
    })
}

/// Give the owner rwx on every directory below `p` (never follows symlinks,
/// skips anything owned by someone else).
fn make_writable(p: &Path) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let uid = std::fs::metadata(std::env::var("HOME").unwrap_or_default()).map(|m| m.uid()).unwrap_or(u32::MAX);
    let Ok(meta) = std::fs::symlink_metadata(p) else { return };
    if !meta.is_dir() || meta.uid() != uid {
        return;
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode | 0o700));
    }
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                make_writable(&e.path());
            }
        }
    }
}

fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

fn leftover_data(out: &mut Vec<Cleanup>, rel: &str, label: &str, installed: impl Fn(&str) -> bool) {
    let Ok(home) = std::env::var("HOME") else { return };
    let dir = Path::new(&home).join(rel);
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    let mut items = Vec::new();
    let mut paths = Vec::new();
    let mut total = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if installed(&name) || !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let size = dir_size(&e.path());
        total += size;
        items.push(format!("~/{rel}/{name} ({})", crate::model::human_size(size)));
        paths.push(e.path().to_string_lossy().into_owned());
    }
    if items.is_empty() {
        return;
    }
    out.push(Cleanup {
        title: format!("Data left behind by removed {label} apps"),
        detail: "Settings and files of apps that are no longer installed. They're moved to the Trash, so you can restore them if you reinstall an app later.".into(),
        size: Some(total),
        risk: Risk::Caution,
        action: which("gio").then(|| Action {
            label: "Move to Trash".into(),
            confirm: Some(format!("Move {} folder(s) to the Trash?", paths.len())),
            kind: ActionKind::Trash(paths),
        }),
        items,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(id: &str, name: &str, src: Source, kind: Kind, version: &str) -> Package {
        Package {
            id: id.into(),
            name: name.into(),
            version: version.into(),
            source: src,
            summary: String::new(),
            size_bytes: None,
            kind,
            protected: false,
            extra: String::new(),
            new_version: None,
            telemetry: None,
            installed_at: None,
        }
    }

    #[test]
    fn vscode_settings_edit() {
        let _guard = crate::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("pkgtrack-test-{}", std::process::id()));
        let user = dir.join(".config/Code/User");
        std::fs::create_dir_all(&user).unwrap();
        unsafe { std::env::set_var("HOME", &dir) };
        let f = user.join("settings.json");

        // key missing -> inserted, rest preserved
        std::fs::write(&f, "{\n    \"editor.fontSize\": 14\n}\n").unwrap();
        vscode_off().unwrap();
        let c = std::fs::read_to_string(&f).unwrap();
        assert!(c.contains("\"telemetry.telemetryLevel\": \"off\","), "{c}");
        assert!(c.contains("\"editor.fontSize\": 14"));
        assert!(user.join("settings.json.pkgtrack-backup").exists());

        // key present -> replaced, comma kept
        std::fs::write(&f, "{\n  \"telemetry.telemetryLevel\": \"all\",\n  \"a\": 1\n}").unwrap();
        vscode_off().unwrap();
        let c = std::fs::read_to_string(&f).unwrap();
        assert!(c.contains("  \"telemetry.telemetryLevel\": \"off\",\n  \"a\": 1"), "{c}");

        // empty object -> no trailing comma
        std::fs::write(&f, "{}").unwrap();
        vscode_off().unwrap();
        let c = std::fs::read_to_string(&f).unwrap();
        assert_eq!(c.trim(), "{\n    \"telemetry.telemetryLevel\": \"off\"\n}");
        assert_eq!(telemetry::is_off("VS Code telemetry", Some(Fix::VsCode)), Some(true));

        // env opt-out is idempotent
        env_opt_out("TEST_OPTOUT").unwrap();
        env_opt_out("TEST_OPTOUT").unwrap();
        let e = std::fs::read_to_string(dir.join(telemetry::ENV_FILE)).unwrap();
        assert_eq!(e, "TEST_OPTOUT=1\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn duplicates_detected() {
        let all = vec![
            pkg("code", "code", Source::Snap, Kind::App, "1"),
            pkg("com.visualstudio.code", "Visual Studio Code", Source::Flatpak, Kind::App, "1.9"),
            pkg("org.videolan.VLC", "VLC", Source::Flatpak, Kind::App, "3.0.24"),
            pkg("vlc", "vlc", Source::Apt, Kind::App, "3.0.20-1"),
            pkg("libvlc5", "libvlc5", Source::Apt, Kind::Library, "3.0.20"),
            pkg("org.gnome.Platform", "GNOME", Source::Flatpak, Kind::Runtime, "45"),
            pkg("gimp", "gimp", Source::Apt, Kind::App, "2.10"),
        ];
        let mut g: Vec<Vec<String>> = find_duplicates(&all)
            .into_iter()
            .map(|g| { let mut v: Vec<String> = g.into_iter().map(|p| p.id).collect(); v.sort(); v })
            .collect();
        g.sort();
        assert_eq!(g, vec![vec!["code".to_string(), "com.visualstudio.code".into()], vec!["org.videolan.VLC".into(), "vlc".into()]]);
    }

    #[test]
    fn delete_unlocks_inaccessible_dirs() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = crate::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("pkgtrack-del-{}", std::process::id()));
        let target = home.join(".cache/bazel/out/sandbox/inaccessibleHelperDir");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(home.join(".cache/bazel/out/file"), vec![0u8; 4096]).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o000)).unwrap();
        let old = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", &home) };
        let r = run_action(&ActionKind::Delete(vec![home.join(".cache/bazel").to_string_lossy().into_owned()]));
        // outside ~/.cache is refused
        let outside = run_action(&ActionKind::Delete(vec![home.to_string_lossy().into_owned()]));
        if let Some(h) = old {
            unsafe { std::env::set_var("HOME", h) };
        }
        assert!(r.is_ok(), "{r:?}");
        assert!(!home.join(".cache/bazel").exists());
        assert!(outside.is_err());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn versions() {
        assert!(version_tuple("3.2.8").unwrap() > version_tuple("2.10.36-3ubuntu0").unwrap());
        assert_eq!(version_tuple("1:1snap1-0ubuntu5"), None);
    }

    #[test]
    fn update_selected_builds_one_root_script() {
        let mut a = pkg("vlc", "VLC", Source::Apt, Kind::App, "3.0");
        a.new_version = Some("3.1".into());
        let mut b = pkg("it's", "Odd", Source::Apt, Kind::App, "1");
        b.new_version = Some("2".into());
        let mut f = pkg("org.gimp.GIMP", "GIMP", Source::Flatpak, Kind::App, "2");
        f.new_version = Some("3".into());
        f.extra = "user\tapp/org.gimp.GIMP/x86_64/stable".into();
        let none = pkg("curl", "curl", Source::Apt, Kind::App, "8");
        let act = update_selected_action(&[&a, &b, &f, &none]).unwrap();
        assert!(act.confirm.unwrap().starts_with("Update 3 selected package(s) (2 deb, 1 flatpak)"));
        let ActionKind::Many(steps) = act.kind else { panic!() };
        let ActionKind::RootScript(sh) = &steps[0] else { panic!() };
        assert!(sh.contains("--only-upgrade") && sh.contains("'vlc' 'it'\\''s'") && !sh.contains("curl"));
        let ActionKind::User(cmd) = &steps[1] else { panic!() };
        assert_eq!(cmd[4..], ["--user".to_string(), "app/org.gimp.GIMP/x86_64/stable".into()]);
        assert!(update_selected_action(&[&none]).is_none());
    }

    #[test]
    fn snap_running_is_not_an_error() {
        let mk = |id: &str| { let mut p = pkg(id, id, Source::Snap, Kind::App, "1"); p.new_version = Some("2".into()); p };
        let (a, b) = (mk("bw"), mk("snap-store"));
        let script = root_script(&[snap_refresh_script(&[&a, &b])]);
        if let Ok(out) = std::env::var("PKGTRACK_PRINT_SCRIPT") { std::fs::write(out, &script).unwrap(); }
        assert!(script.ends_with("done; :; exit $rc") && !script.contains(": || rc=1"));
    }
}
