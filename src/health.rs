//! System health: problems that break updates or the system, with fixes.
//!
//! Every check here is read-only and runs as the current user. Fixes are
//! offered as `Action`s, which ask before running and use polkit for root.
use crate::advice::{Action, ActionKind};
use crate::backends::{run, which};
use crate::model::Source;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Breaks updates or the system until fixed.
    Problem,
    /// Not urgent, but worth fixing.
    Warning,
    /// Good to know; nothing is broken.
    Info,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Problem => "Needs fixing",
            Severity::Warning => "Worth a look",
            Severity::Info => "Good to know",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Issue {
    pub title: String,
    pub detail: String,
    pub items: Vec<String>,
    pub severity: Severity,
    pub actions: Vec<Action>,
}

/// Run every check that applies to this system; most severe first.
pub fn check(sources: &[Source]) -> Vec<Issue> {
    let apt = sources.contains(&Source::Apt);
    let rpm = sources.contains(&Source::Rpm);
    let pacman = sources.contains(&Source::Pacman);
    let mut issues: Vec<Issue> = std::thread::scope(|s| {
        let jobs: Vec<std::thread::ScopedJoinHandle<Vec<Issue>>> = vec![
            s.spawn(dkms),
            s.spawn(move || if apt { interrupted_apt() } else { Vec::new() }),
            s.spawn(move || if apt { held_apt() } else { Vec::new() }),
            s.spawn(move || if rpm { dnf_check() } else { Vec::new() }),
            s.spawn(move || if pacman { pacman_checks() } else { Vec::new() }),
            s.spawn(failed_services),
            s.spawn(move || disk_space(apt)),
            s.spawn(reboot_needed),
        ];
        jobs.into_iter().flat_map(|j| j.join().unwrap_or_default()).collect()
    });
    issues.sort_by_key(|i| i.severity);
    issues
}

fn root_script(label: &str, script: String, confirm: String) -> Action {
    Action { label: label.into(), kind: ActionKind::RootScript(script), confirm: Some(confirm) }
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ------------------------------------------------------------ packages

/// One dpkg status line, `${db:Status-Abbrev}`: want, state, error flag.
fn dpkg_broken(abbrev: &str) -> bool {
    let mut c = abbrev.chars();
    let (_want, state, err) = (c.next(), c.next(), c.next());
    matches!(state, Some('H' | 'U' | 'F' | 'W' | 't')) || err == Some('R')
}

fn interrupted_apt() -> Vec<Issue> {
    let Ok(out) = run("dpkg-query", &["-W", "-f=${db:Status-Abbrev}\t${Package}\n"], &[]) else { return Vec::new() };
    let broken: Vec<String> = out
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter(|(st, _)| dpkg_broken(st))
        .map(|(_, p)| p.to_string())
        .collect();
    if broken.is_empty() {
        return Vec::new();
    }
    vec![Issue {
        title: "Unfinished package installs".into(),
        detail: "These packages were unpacked but not finished, usually because an install was interrupted \
                 or a step failed. Until they're finished, other installs and updates can fail too. \
                 If a driver problem is listed above, fix that first."
            .into(),
        items: broken,
        severity: Severity::Problem,
        actions: vec![root_script(
            "Finish installing",
            "dpkg --configure -a && apt-get -f install -y".into(),
            "Finish the interrupted installs (dpkg --configure -a, then apt-get -f install)?".into(),
        )],
    }]
}

fn held_apt() -> Vec<Issue> {
    let Ok(out) = run("apt-mark", &["showhold"], &[]) else { return Vec::new() };
    let held: Vec<String> = out.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    if held.is_empty() {
        return Vec::new();
    }
    let mut argv: Vec<String> = vec!["apt-mark".into(), "unhold".into()];
    argv.extend(held.iter().cloned());
    vec![Issue {
        title: "Packages kept back from updates".into(),
        detail: "These packages are on hold, so they never get updates, including security fixes. \
                 That's fine if you held them on purpose (for example to keep a working driver)."
            .into(),
        items: held,
        severity: Severity::Info,
        actions: vec![Action {
            label: "Allow updates".into(),
            kind: ActionKind::Root(argv),
            confirm: Some("Remove the hold so these packages update again?".into()),
        }],
    }]
}

fn dnf_check() -> Vec<Issue> {
    if !which("dnf") {
        return Vec::new();
    }
    let out = match std::process::Command::new("dnf").args(["check", "-q"]).env("LC_ALL", "C").output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let mut log = String::from_utf8_lossy(&out.stdout).into_owned();
    log.push_str(&String::from_utf8_lossy(&out.stderr));
    let lines: Vec<String> = log.lines().map(str::trim).filter(|l| !l.is_empty()).take(30).map(String::from).collect();
    if out.status.success() && lines.is_empty() {
        return Vec::new();
    }
    vec![Issue {
        title: "Package database problems".into(),
        detail: "dnf found broken dependencies or duplicate packages. Updates may fail until they're fixed. \
                 Duplicates usually come from an interrupted update; `sudo dnf remove --duplicates` cleans them up."
            .into(),
        items: lines,
        severity: Severity::Problem,
        actions: Vec::new(),
    }]
}

fn pacman_checks() -> Vec<Issue> {
    let mut v = Vec::new();
    if let Err(e) = run("pacman", &["-Dk"], &[]) {
        v.push(Issue {
            title: "Package database problems".into(),
            detail: "pacman found missing dependencies or conflicts in the installed packages. \
                     Updates may fail until they're fixed."
                .into(),
            items: e.to_string().lines().take(30).map(String::from).collect(),
            severity: Severity::Problem,
            actions: Vec::new(),
        });
    }
    let pacnew: Vec<String> = run("find", &["/etc", "-name", "*.pacnew"], &[1])
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect();
    if !pacnew.is_empty() {
        v.push(Issue {
            title: "New config files to merge".into(),
            detail: "An update shipped new versions of config files you had changed, and saved them as .pacnew. \
                     Your old files are still in use. Compare and merge them with `sudo pacdiff` (pacman-contrib)."
                .into(),
            items: pacnew,
            severity: Severity::Info,
            actions: Vec::new(),
        });
    }
    v
}

// ------------------------------------------------------------ drivers

/// `dkms status` line → (module "name/version", kernel, status).
fn parse_dkms_line(line: &str) -> Option<(String, String, String)> {
    let (left, status) = line.rsplit_once(':')?;
    let parts: Vec<&str> = left.split(',').map(str::trim).collect();
    let (module, kernel) = match parts.as_slice() {
        // New format: "name/version, kernel, arch"
        [m, k, _arch] if m.contains('/') => (m.to_string(), k.to_string()),
        // Old format: "name, version, kernel, arch"
        [n, ver, k, _arch] => (format!("{n}/{ver}"), k.to_string()),
        // Added but not built for any kernel: "name/version"
        _ => return None,
    };
    let status = status.split_whitespace().next().unwrap_or("").to_string();
    Some((module, kernel, status))
}

/// Package that installed a DKMS module's sources, if any.
fn dkms_owner(module: &str) -> Option<String> {
    let (name, ver) = module.split_once('/')?;
    let src = format!("/usr/src/{name}-{ver}");
    let out = if which("dpkg") {
        run("dpkg", &["-S", &src], &[]).ok()?.lines().next()?.split(':').next()?.trim().to_string()
    } else if which("rpm") {
        run("rpm", &["-qf", "--qf", "%{NAME}", &src], &[]).ok()?.trim().to_string()
    } else if which("pacman") {
        run("pacman", &["-Qoq", &src], &[]).ok()?.lines().next()?.trim().to_string()
    } else {
        return None;
    };
    (!out.is_empty()).then_some(out)
}

/// Why a DKMS build failed, judged from its make.log.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BuildFailure {
    /// The kernel changed an internal function or structure the driver uses;
    /// only a newer driver fixes it.
    KernelApi,
    /// No C compiler (or make) installed.
    NoCompiler,
    /// The compiler is older than the one the kernel was built with and
    /// doesn't know its options. Holds that compiler's name, e.g. "gcc-13".
    OldCompiler(Option<String>),
    Other,
}

/// First compiler error in a build log, shortened, and what kind of failure it is.
fn classify_build_log(log: &str) -> (Option<String>, BuildFailure) {
    let first = log.lines().find(|l| l.contains("error:")).map(|line| {
        // Drop the long build path, keep "file.c:123: error: …".
        let short = line.rsplit_once('/').map(|(_, f)| f).unwrap_or(line);
        short.chars().take(200).collect::<String>()
    });
    const API: [&str; 9] = [
        "too few arguments to function",
        "too many arguments to function",
        "implicit declaration of function",
        "has no member named",
        "incompatible pointer type",
        "incompatible type for argument",
        "conflicting types for",
        "undeclared (first use",
        "unknown type name",
    ];
    let kind = if log.contains("gcc: not found")
        || log.contains("gcc: No such file or directory")
        || log.contains("gcc: command not found")
        || log.contains("make: not found")
        || log.contains("make: command not found")
    {
        BuildFailure::NoCompiler
    } else if log.contains("unrecognized command-line option") || log.contains("unrecognized command line option") {
        // "  The kernel was built by: x86_64-linux-gnu-gcc-13 (Ubuntu …) 13.3.0"
        let gcc = log
            .lines()
            .find_map(|l| l.trim().strip_prefix("The kernel was built by:"))
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.rsplit_once("gcc-").map(|(_, n)| n))
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            .map(|n| format!("gcc-{n}"));
        BuildFailure::OldCompiler(gcc)
    } else if API.iter().any(|p| log.contains(p)) {
        BuildFailure::KernelApi
    } else {
        BuildFailure::Other
    };
    (first, kind)
}

fn dkms_build_failure(module: &str) -> (Option<String>, BuildFailure) {
    match std::fs::read_to_string(format!("/var/lib/dkms/{module}/build/make.log")) {
        Ok(log) => classify_build_log(&log),
        Err(_) => (None, BuildFailure::Other),
    }
}

/// Newer version of an installed package that the package manager can install
/// now (from its local cache; no network). (installed, candidate)
fn pending_upgrade(pkg: &str) -> Option<(String, String)> {
    if which("apt-cache") {
        let out = run("apt-cache", &["policy", pkg], &[]).ok()?;
        let field = |name: &str| {
            out.lines().find_map(|l| l.trim().strip_prefix(name)).map(|v| v.trim().to_string())
        };
        let (inst, cand) = (field("Installed:")?, field("Candidate:")?);
        let none = |v: &str| v.is_empty() || v == "(none)";
        (!none(&inst) && !none(&cand) && inst != cand).then_some((inst, cand))
    } else if which("pacman") {
        // "name old -> new"; exit 1 when up to date.
        let out = run("pacman", &["-Qu", pkg], &[1]).ok()?;
        let f: Vec<&str> = out.lines().next()?.split_whitespace().collect();
        match f.as_slice() {
            [_, old, "->", new, ..] => Some((old.to_string(), new.to_string())),
            _ => None,
        }
    } else if which("dnf") {
        let out = run("dnf", &["-q", "--cacheonly", "list", "--upgrades", pkg], &[1]).ok()?;
        let new = out.lines().find(|l| l.starts_with(&format!("{pkg}.")))?.split_whitespace().nth(1)?.to_string();
        Some((String::new(), new))
    } else {
        None
    }
}

/// Command that upgrades just this package.
fn upgrade_cmd(pkg: &str) -> Option<String> {
    let q = sh_quote(pkg);
    if which("apt-get") {
        Some(format!("apt-get install -y --only-upgrade {q}"))
    } else if which("pacman") {
        Some(format!("pacman -S --noconfirm --needed {q}"))
    } else if which("dnf") {
        Some(format!("dnf upgrade -y {q}"))
    } else if which("zypper") {
        Some(format!("zypper -n update {q}"))
    } else {
        None
    }
}

/// Installed kernels that can have modules built (image + headers present).
fn buildable_kernels() -> Vec<String> {
    let Ok(rd) = std::fs::read_dir("/lib/modules") else { return Vec::new() };
    let mut v: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|k| {
            Path::new(&format!("/lib/modules/{k}/build")).exists()
                && (Path::new(&format!("/boot/vmlinuz-{k}")).exists() || Path::new(&format!("/lib/modules/{k}/vmlinuz")).exists())
        })
        .collect();
    v.sort_by_key(|k| kernel_key(k));
    v
}

/// Folders in /lib/modules left behind by kernels that are no longer
/// installed. Deliberately strict: a folder is only listed when it is older
/// than the running kernel, has no kernel image (in /boot or inside it), has
/// a plain version name, and no installed package owns it. Returns full paths.
pub(crate) fn leftover_module_dirs() -> Vec<String> {
    let running = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim().to_string();
    if running.is_empty() {
        return Vec::new();
    }
    let Ok(rd) = std::fs::read_dir("/lib/modules") else { return Vec::new() };
    let plain = |k: &str| {
        k.starts_with(|c: char| c.is_ascii_digit())
            && k.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
    };
    let candidates: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) // never follow symlinks
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|k| plain(k) && *k != running && kernel_key(k) < kernel_key(&running))
        .filter(|k| {
            !Path::new(&format!("/boot/vmlinuz-{k}")).exists()
                && !Path::new(&format!("/lib/modules/{k}/vmlinuz")).exists()
                && !Path::new(&format!("/boot/{k}")).exists()
        })
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    // Ask the package manager; check both /lib and /usr/lib spellings.
    // Without a package manager to ask, list nothing.
    let paths: Vec<String> = candidates
        .iter()
        .flat_map(|k| [format!("/lib/modules/{k}"), format!("/usr/lib/modules/{k}")])
        .collect();
    let args: Vec<&str> = paths.iter().map(String::as_str).collect();
    // Paths some package owns. If the package manager can't be asked, list nothing.
    let owned: BTreeSet<String> = if which("dpkg") {
        // "pkg1, pkg2: /lib/modules/x" for owned paths; unowned ones only go to stderr.
        let Ok(out) = run("dpkg", &[&["-S"][..], &args].concat(), &[1]) else { return Vec::new() };
        out.lines().filter_map(|l| l.rsplit_once(": ")).map(|(_, p)| p.trim().to_string()).collect()
    } else if which("pacman") {
        // "/lib/modules/x is owned by pkg 1.0-1"
        let Ok(out) = run("pacman", &[&["-Qo"][..], &args].concat(), &[1]) else { return Vec::new() };
        out.lines().filter(|l| l.contains(" is owned by ")).filter_map(|l| l.split_whitespace().next()).map(String::from).collect()
    } else if which("rpm") {
        paths
            .iter()
            .filter(|p| std::process::Command::new("rpm").args(["-qf", p.as_str()]).output().is_ok_and(|o| o.status.success()))
            .cloned()
            .collect()
    } else {
        return Vec::new();
    };
    let mut dirs: Vec<&String> = candidates
        .iter()
        .filter(|k| !owned.contains(&format!("/lib/modules/{k}")) && !owned.contains(&format!("/usr/lib/modules/{k}")))
        .collect();
    dirs.sort_by_key(|k| kernel_key(k)); // oldest first
    dirs.into_iter().map(|k| format!("/lib/modules/{k}")).collect()
}

pub(crate) fn kernel_key(k: &str) -> Vec<u64> {
    k.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).map(|s| s.parse().unwrap_or(0)).collect()
}

fn dkms() -> Vec<Issue> {
    if !which("dkms") {
        return Vec::new();
    }
    let Ok(out) = run("dkms", &["status"], &[]) else { return Vec::new() };
    let mut modules: BTreeSet<String> = BTreeSet::new();
    let mut ok: BTreeSet<(String, String)> = BTreeSet::new();
    for (m, k, st) in out.lines().filter_map(parse_dkms_line) {
        modules.insert(m.clone());
        if st == "installed" || st == "built" {
            ok.insert((m, k));
        }
    }
    let kernels = buildable_kernels();
    let running = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim().to_string();
    let newest = kernels.last().cloned().unwrap_or_default();
    let mut missing: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for m in &modules {
        // Only kernels at least as new as the oldest one the module works with;
        // drivers often drop support for very old kernels on purpose.
        let first_ok = kernels.iter().position(|k| ok.contains(&(m.clone(), k.clone())));
        let Some(first_ok) = first_ok else { continue };
        for k in &kernels[first_ok..] {
            if !ok.contains(&(m.clone(), k.clone())) {
                missing.entry(m.clone()).or_default().push(k.clone());
            }
        }
    }
    let mut issues: Vec<Issue> = missing
        .into_iter()
        .map(|(m, ks)| {
            let name = m.split('/').next().unwrap_or(&m).to_string();
            let owner = dkms_owner(&m);
            let (err, failure) = dkms_build_failure(&m);
            let upgrade = owner.as_deref().and_then(|p| pending_upgrade(p).map(|v| (p.to_string(), v)));
            let urgent = ks.iter().any(|k| *k == newest || *k == running);
            let running_broken = ks.contains(&running);
            let mut detail = if running_broken {
                format!(
                    "The driver {name} couldn't be built for kernel {}. That kernel's install can't finish, and the \
                     hardware this driver is for doesn't work while you run it, as you do now.",
                    ks.join(", ")
                )
            } else {
                format!(
                    "The driver {name} couldn't be built for kernel {}. That kernel's install can't finish, and if \
                     you start it, the hardware this driver is for won't work. The kernel you run now is not affected.",
                    ks.join(", ")
                )
            };
            if let Some(e) = &err {
                detail.push_str(&format!("\n\nBuild error: {e}"));
            }
            let configure = if which("dpkg") { " && dpkg --configure -a" } else { "" };
            let mut actions = Vec::new();
            let mut extra_tools: Option<(String, String)> = None; // (label, install command)
            let why = match &failure {
                BuildFailure::KernelApi => format!(
                    "The new kernel changed something inside it that this driver uses, so this version of the driver \
                     can't work with it. Building it again won't help; only a newer {name} fixes it. Driver makers \
                     usually release one within days or weeks of a kernel update."
                ),
                BuildFailure::NoCompiler => {
                    extra_tools = if which("apt-get") {
                        Some(("Install build tools".into(), "apt-get install -y build-essential".into()))
                    } else if which("dnf") {
                        Some(("Install build tools".into(), "dnf install -y gcc make".into()))
                    } else if which("pacman") {
                        Some(("Install build tools".into(), "pacman -S --noconfirm --needed base-devel".into()))
                    } else if which("zypper") {
                        Some(("Install build tools".into(), "zypper -n install gcc make".into()))
                    } else {
                        None
                    };
                    "Drivers are built on your computer, and the C compiler needed for that isn't installed. \
                     Install the build tools, and the driver is built again."
                        .into()
                }
                BuildFailure::OldCompiler(gcc) => {
                    if let (Some(g), true) = (gcc, which("apt-get")) {
                        extra_tools = Some((format!("Install {g}"), format!("apt-get install -y {}", sh_quote(g))));
                    }
                    match gcc {
                        Some(g) => format!(
                            "The kernel was built with a newer compiler ({g}) than the one this driver was built with, \
                             which doesn't understand its settings. Install {g}, and the driver is built again."
                        ),
                        None => "The kernel was built with a newer compiler than the one installed. Update the \
                                 compiler (gcc) and press Try again."
                            .into(),
                    }
                }
                BuildFailure::Other => "This usually means the driver is older than the kernel. Update the driver \
                                        from wherever you got it, then press Try again."
                    .into(),
            };
            detail.push_str(&format!("\n\n{why}"));
            match &upgrade {
                Some((p, (_, new))) => detail.push_str(&format!(
                    "\n\nA newer {p} ({new}) is available. Updating to it is the most likely fix."
                )),
                None if failure == BuildFailure::KernelApi => detail.push_str(
                    "\n\nNo newer version is available from your software sources yet. Check the driver's website, \
                     or wait and look again after the next update check.",
                ),
                None => {}
            }
            // A kernel the driver works with, to use in the meantime.
            if let Some(good) = kernels.iter().rev().find(|k| ok.contains(&(m.clone(), (*k).clone()))) {
                let menu = if Path::new("/boot/grub/grub.cfg").exists() || Path::new("/boot/grub2/grub.cfg").exists() {
                    " (under \u{201c}Advanced options\u{201d} in the boot menu; hold Shift or press Esc while the \
                     computer starts if the menu doesn't show)"
                } else {
                    " in the boot menu"
                };
                detail.push_str(&format!(
                    "\n\nUntil then, start kernel {good}{menu}; the driver works there. If you don't need the driver, \
                     remove it and the kernel install finishes."
                ));
            } else {
                detail.push_str("\n\nIf you don't need the driver, remove it and the kernel install finishes.");
            }
            let retry = ks
                .iter()
                .map(|k| format!("dkms autoinstall -k {}", sh_quote(k)))
                .collect::<Vec<_>>()
                .join(" && ");
            if let (Some((p, (_, new))), Some(cmd)) = (&upgrade, upgrade.as_ref().and_then(|(p, _)| upgrade_cmd(p))) {
                actions.push(root_script(
                    "Update driver",
                    // The package's own install script builds it for every kernel.
                    format!("{cmd}{configure}"),
                    format!("Update {p} to {new}? It is then built again for kernel {}.", ks.join(", ")),
                ));
            }
            if let Some((label, cmd)) = extra_tools {
                actions.push(root_script(
                    &label,
                    format!("{cmd} && {retry}{configure}"),
                    format!("Run \u{201c}{cmd}\u{201d}, then build the {name} driver again for kernel {}?", ks.join(", ")),
                ));
            }
            actions.push(root_script(
                "Try again",
                format!("{retry}{configure}"),
                format!("Build the {name} driver again for kernel {}?", ks.join(", ")),
            ));
            if let Some(pkg) = owner.as_ref() {
                // What else goes with it (apt removes dependents without asking).
                let also: Vec<String> = if which("apt-get") {
                    run("apt-get", &["-s", "remove", pkg], &[])
                        .unwrap_or_default()
                        .lines()
                        .filter_map(|l| l.strip_prefix("Remv "))
                        .filter_map(|l| l.split_whitespace().next())
                        .filter(|p| p != pkg)
                        .map(String::from)
                        .collect()
                } else {
                    Vec::new()
                };
                let also = if also.is_empty() { String::new() } else { format!(" This also removes: {}.", also.join(", ")) };
                let remove = if which("apt-get") {
                    format!("apt-get remove -y {}{configure}", sh_quote(pkg))
                } else if which("dnf") {
                    format!("dnf remove -y {}", sh_quote(pkg))
                } else if which("zypper") {
                    format!("zypper -n remove {}", sh_quote(pkg))
                } else {
                    format!("pacman -R --noconfirm {}", sh_quote(pkg))
                };
                actions.push(root_script(
                    &format!("Remove {pkg}"),
                    remove,
                    format!(
                        "Remove {pkg}?{also} The hardware or feature it supports ({name}) stops working until you install it again; \
                         the kernel update can then finish."
                    ),
                ));
            }
            let mut items: Vec<String> = ks.iter().map(|k| format!("kernel {k}")).collect();
            items.push(format!("log: /var/lib/dkms/{m}/build/make.log"));
            if let Some(p) = owner {
                items.push(format!("package: {p}"));
            }
            Issue {
                title: format!("Driver \u{201c}{name}\u{201d} doesn't build for a new kernel"),
                detail,
                items,
                severity: if urgent { Severity::Problem } else { Severity::Warning },
                actions,
            }
        })
        .collect();
    let working: Vec<String> =
        modules.iter().filter(|m| kernels.iter().any(|k| ok.contains(&((*m).clone(), k.clone())))).cloned().collect();
    issues.extend(missing_headers(&working, &running));
    issues
}

/// Headers package for a kernel, if the package manager has a naming rule for it.
fn headers_package(kernel: &str) -> Option<String> {
    if which("apt-get") {
        Some(format!("linux-headers-{kernel}"))
    } else if which("dnf") || which("zypper") {
        Some(format!("kernel-devel-{kernel}"))
    } else if which("pacman") {
        // Arch records which kernel package a modules folder belongs to.
        let base = std::fs::read_to_string(format!("/lib/modules/{kernel}/pkgbase")).ok()?;
        let base = base.trim();
        (!base.is_empty()).then(|| format!("{base}-headers"))
    } else {
        None
    }
}

/// Kernels you can start (at least as new as the running one) that have no
/// headers, so DKMS drivers that work elsewhere aren't built for them at all.
fn missing_headers(working_modules: &[String], running: &str) -> Vec<Issue> {
    if working_modules.is_empty() {
        return Vec::new();
    }
    let Ok(rd) = std::fs::read_dir("/lib/modules") else { return Vec::new() };
    let mut ks: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|k| {
            (Path::new(&format!("/boot/vmlinuz-{k}")).exists() || Path::new(&format!("/lib/modules/{k}/vmlinuz")).exists())
                && !Path::new(&format!("/lib/modules/{k}/build")).exists()
                && kernel_key(k) >= kernel_key(running)
        })
        .collect();
    if ks.is_empty() {
        return Vec::new();
    }
    ks.sort_by_key(|k| kernel_key(k));
    let names: Vec<&str> = working_modules.iter().map(|m| m.split('/').next().unwrap_or(m)).collect();
    let pkgs: Vec<String> = ks.iter().filter_map(|k| headers_package(k)).collect::<BTreeSet<_>>().into_iter().collect();
    let mut actions = Vec::new();
    if !pkgs.is_empty() {
        let q: Vec<String> = pkgs.iter().map(|p| sh_quote(p)).collect();
        let install = if which("apt-get") {
            format!("apt-get install -y {}", q.join(" "))
        } else if which("dnf") {
            format!("dnf install -y {}", q.join(" "))
        } else if which("zypper") {
            format!("zypper -n install {}", q.join(" "))
        } else {
            format!("pacman -S --noconfirm --needed {}", q.join(" "))
        };
        let build = ks.iter().map(|k| format!("dkms autoinstall -k {}", sh_quote(k))).collect::<Vec<_>>().join(" && ");
        actions.push(root_script(
            "Install headers",
            format!("{install} && {build}"),
            format!("Install {} and build {} for kernel {}?", pkgs.join(", "), names.join(", "), ks.join(", ")),
        ));
    }
    let mut items: Vec<String> = ks.iter().map(|k| format!("kernel {k}")).collect();
    items.extend(pkgs.iter().map(|p| format!("missing: {p}")));
    vec![Issue {
        title: format!("Kernel {} has no headers, so drivers aren't built for it", ks.join(", ")),
        detail: format!(
            "Drivers like {} are built on your computer for each kernel, which needs that kernel's headers. Kernel {} \
             has none installed, so if you start it, the hardware these drivers are for won't work. Install the \
             headers, and the drivers are built for it.",
            names.join(", "),
            ks.join(", ")
        ),
        items,
        severity: Severity::Warning,
        actions,
    }]
}

// ------------------------------------------------------------ services

fn failed_services() -> Vec<Issue> {
    let mut v = Vec::new();
    for user in [false, true] {
        let mut args = vec!["--failed", "--no-legend", "--plain", "--no-pager"];
        if user {
            args.insert(0, "--user");
        }
        let Ok(out) = run("systemctl", &args, &[]) else { continue };
        for line in out.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            let [unit, load, ..] = f.as_slice() else { continue };
            let unit = unit.trim_start_matches('●').to_string();
            if unit.is_empty() {
                continue;
            }
            let ctl = |verb: &str| -> Vec<String> {
                let mut a = vec!["systemctl".to_string()];
                if user {
                    a.push("--user".into());
                }
                a.push(verb.into());
                a.push(unit.clone());
                a
            };
            let action = |label: &str, verbs: &[&str], confirm: String| -> Action {
                let mut argv = ctl(verbs[0]);
                argv.splice(argv.len() - 1..argv.len() - 1, verbs[1..].iter().map(|s| s.to_string()));
                Action {
                    label: label.into(),
                    kind: if user { ActionKind::User(argv) } else { ActionKind::Root(argv) },
                    confirm: Some(confirm),
                }
            };
            if *load == "not-found" {
                // The program was uninstalled but systemd still remembers it failed.
                v.push(Issue {
                    title: format!("Leftover failed service: {unit}"),
                    detail: "This service belongs to a program that's no longer installed, but systemd still lists it \
                             as failed. Nothing is broken; clearing it just removes the entry."
                        .into(),
                    items: vec![unit.clone()],
                    severity: Severity::Info,
                    actions: vec![action("Clear", &["reset-failed"], format!("Forget the failed state of {unit}?"))],
                });
                continue;
            }
            let mut jargs = vec!["-u", unit.as_str(), "-b", "-n", "4", "--no-pager", "-o", "cat"];
            if user {
                jargs.insert(0, "--user");
            }
            let log: Vec<String> = run("journalctl", &jargs, &[1])
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(String::from)
                .collect();
            let mut items = vec![unit.clone()];
            items.extend(log);
            v.push(Issue {
                title: format!("Service failed to start: {unit}"),
                detail: format!(
                    "{unit} {} failed this session, so whatever it provides isn't running. The last log lines are below; \
                     `journalctl -u {unit} -b` shows more. If you don't use it any more, turn it off.",
                    if user { "(a service of your user account)" } else { "(a system service)" }
                ),
                items,
                severity: Severity::Warning,
                actions: vec![
                    action("Restart", &["restart"], format!("Try to start {unit} again?")),
                    action(
                        "Turn off",
                        &["disable", "--now"],
                        format!("Stop {unit} and don't start it at boot any more? You can turn it back on in the Startup tab."),
                    ),
                ],
            });
        }
    }
    v
}

// ------------------------------------------------------------ disk

fn disk_space(apt: bool) -> Vec<Issue> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let mut paths = vec!["/", "/boot", home.as_str()];
    if Path::new("/boot/efi").exists() {
        paths.push("/boot/efi");
    }
    let Ok(out) = run("df", &[&["-Pk"][..], &paths[..]].concat(), &[1]) else { return Vec::new() };
    let mut seen = BTreeSet::new();
    let mut v = Vec::new();
    for line in out.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let [dev, size, _used, avail, _pct, mount] = f.as_slice() else { continue };
        if !seen.insert(dev.to_string()) {
            continue;
        }
        let (Ok(size), Ok(avail)) = (size.parse::<u64>(), avail.parse::<u64>()) else { continue };
        let (size, avail) = (size * 1024, avail * 1024);
        let pct = (avail * 100).checked_div(size).unwrap_or(100);
        let free = crate::model::human_size(avail);
        let boot = mount.starts_with("/boot");
        // The EFI partition only holds kernels with systemd-boot / UKIs;
        // otherwise it's small and mostly empty by design.
        let kernels_here = *mount == "/boot"
            || ["loader/entries", "EFI/Linux"].iter().any(|d| Path::new(mount).join(d).exists());
        let severity = if boot && !kernels_here {
            (avail < 10_000_000).then_some(Severity::Warning)
        } else if boot {
            if avail < 100_000_000 { Some(Severity::Problem) } else if avail < 200_000_000 { Some(Severity::Warning) } else { None }
        } else if avail < 2_000_000_000 || pct < 3 {
            Some(Severity::Problem)
        } else if pct < 10 {
            Some(Severity::Warning)
        } else {
            None
        };
        let Some(severity) = severity else { continue };
        let (detail, actions) = if boot {
            (
                format!(
                    "Only {free} left on {mount}. Each kernel update needs room there; when it's full, updates fail half-way. \
                     Removing old kernels you no longer boot frees it."
                ),
                if apt && kernels_here {
                    vec![root_script(
                        "Remove old kernels",
                        "apt-get autoremove --purge -y".into(),
                        "Remove old kernels and other packages nothing needs any more (apt-get autoremove --purge)?".into(),
                    )]
                } else {
                    Vec::new()
                },
            )
        } else {
            (
                format!(
                    "Only {free} ({pct}%) left on {mount}. When a disk fills up, updates and apps start failing. \
                     See the Cleanup section for things you can safely delete, and Apps you don't use."
                ),
                Vec::new(),
            )
        };
        v.push(Issue { title: format!("Low disk space on {mount}"), detail, items: vec![format!("{dev} on {mount}")], severity, actions });
    }
    v
}

// ------------------------------------------------------------ reboot

fn reboot_needed() -> Vec<Issue> {
    let mut why: Vec<String> = Vec::new();
    if Path::new("/var/run/reboot-required").exists() {
        why = std::fs::read_to_string("/var/run/reboot-required.pkgs")
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if why.is_empty() {
            why.push("an update asked for a restart".into());
        }
    } else if which("needs-restarting") {
        if let Ok(o) = std::process::Command::new("needs-restarting").arg("-r").output() {
            if o.status.code() == Some(1) {
                why.push("core packages were updated (needs-restarting -r)".into());
            }
        }
    } else {
        // Arch removes the running kernel's modules when it updates the kernel.
        let running = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        let running = running.trim();
        if !running.is_empty() && !Path::new(&format!("/lib/modules/{running}")).exists() {
            why.push(format!("the running kernel ({running}) was replaced by an update"));
        }
    }
    if why.is_empty() {
        return Vec::new();
    }
    vec![Issue {
        title: "Restart to finish updates".into(),
        detail: "Some updates only take effect after a restart (for example a new kernel or system libraries). \
                 Until then you're still running the old versions, including any security holes they fix."
            .into(),
        items: why,
        severity: Severity::Info,
        actions: Vec::new(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpkg_states() {
        assert!(!dpkg_broken("ii "));
        assert!(!dpkg_broken("rc "));
        assert!(dpkg_broken("iF "));
        assert!(dpkg_broken("iU "));
        assert!(dpkg_broken("iiR"));
    }

    #[test]
    fn dkms_lines() {
        assert_eq!(
            parse_dkms_line("openrazer-driver/3.12.4, 6.8.0-149-generic, x86_64: installed"),
            Some(("openrazer-driver/3.12.4".into(), "6.8.0-149-generic".into(), "installed".into()))
        );
        assert_eq!(
            parse_dkms_line("nvidia, 470.1, 5.4.0-1-generic, x86_64: built"),
            Some(("nvidia/470.1".into(), "5.4.0-1-generic".into(), "built".into()))
        );
        assert_eq!(
            parse_dkms_line("virtualbox/7.0.16, 6.8.0-151-generic, x86_64: installed (WARNING! Diff between built and installed module!)")
                .map(|t| t.2),
            Some("installed".into())
        );
        assert_eq!(parse_dkms_line("openrazer-driver/3.12.4: added"), None);
        assert!(kernel_key("6.8.0-151-generic") > kernel_key("6.8.0-60-generic"));
    }

    #[test]
    fn build_failures() {
        let api = "  CC [M]  /var/lib/dkms/openrazer-driver/3.12.4/build/driver/razerkbd_driver.o\n\
/var/lib/dkms/openrazer-driver/3.12.4/build/driver/razerkbd_driver.c:4856:29: error: too few arguments to function 'hid_report_raw_event'\n";
        let (e, k) = classify_build_log(api);
        assert_eq!(k, BuildFailure::KernelApi);
        assert_eq!(e.as_deref(), Some("razerkbd_driver.c:4856:29: error: too few arguments to function 'hid_report_raw_event'"));

        let gcc = "warning: the compiler differs from the one used to build the kernel\n\
  The kernel was built by: x86_64-linux-gnu-gcc-13 (Ubuntu 13.3.0-6ubuntu2~24.04.1) 13.3.0\n\
  You are using:           gcc-12 (Ubuntu 12.3.0) 12.3.0\n\
gcc-12: error: unrecognized command-line option '-ftrivial-auto-var-init=zero'\n";
        assert_eq!(classify_build_log(gcc).1, BuildFailure::OldCompiler(Some("gcc-13".into())));

        assert_eq!(classify_build_log("/bin/sh: 1: gcc: not found\n").1, BuildFailure::NoCompiler);
        assert_eq!(classify_build_log("foo.c:1: error: something odd\n").1, BuildFailure::Other);
        assert_eq!(classify_build_log(""), (None, BuildFailure::Other));
    }
}
