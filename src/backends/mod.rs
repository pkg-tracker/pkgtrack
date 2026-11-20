pub mod appimage;
mod apt;
pub mod manual;
pub mod flatpak;
mod pacman;
mod rpm;
pub mod search;
mod snap;

use crate::model::{Details, Package, Source};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::process::Command;

pub fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

/// Package sources available on this machine.
pub fn detect() -> Vec<Source> {
    let mut v = Vec::new();
    if which("dpkg-query") {
        v.push(Source::Apt);
    }
    if which("rpm") && (which("dnf") || which("zypper") || which("yum")) {
        v.push(Source::Rpm);
    }
    if which("pacman") {
        v.push(Source::Pacman);
    }
    if which("flatpak") {
        v.push(Source::Flatpak);
    }
    if which("snap") && std::path::Path::new("/snap").exists() {
        v.push(Source::Snap);
    }
    v.push(Source::AppImage);
    v.push(Source::Manual);
    v
}

/// Run a command and return stdout; non-zero exit is an error unless listed in `ok_codes`.
pub fn run(cmd: &str, args: &[&str], ok_codes: &[i32]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .with_context(|| format!("failed to start {cmd}"))?;
    let code = out.status.code().unwrap_or(-1);
    if !out.status.success() && !ok_codes.contains(&code) {
        bail!(
            "{cmd} {} exited with {code}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run a command that needs root through polkit's pkexec (graphical password prompt).
pub fn run_privileged(args: &[&str]) -> Result<String> {
    // pkexec starts the program with a clean environment, so variables set on
    // this Command never arrive. APT/dpkg must not wait for answers on a
    // terminal we don't have, so pass the setting through `env`.
    let apt = matches!(args.first(), Some(&("apt" | "apt-get" | "dpkg" | "sh")));
    let mut cmd = Command::new("pkexec");
    if apt {
        cmd.args(["env", "DEBIAN_FRONTEND=noninteractive"]);
    }
    let out = cmd
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .context("failed to start pkexec (is polkit installed?)")?;
    let mut log = String::from_utf8_lossy(&out.stdout).into_owned();
    log.push_str(&String::from_utf8_lossy(&out.stderr));
    match out.status.code() {
        Some(0) => Ok(log),
        Some(126) | Some(127) => bail!("Authorization was cancelled or denied."),
        c => bail!("{}Command failed ({c:?}):\n{}", hint(&log), tail(&log, 15)),
    }
}

/// Plain-language explanation for failures the raw log doesn't make obvious.
fn hint(log: &str) -> &'static str {
    if log.contains("dkms") && log.contains("autoinstall") {
        "A driver (DKMS module, such as NVIDIA or VirtualBox) could not be built for the new kernel, so the \
         kernel update didn't finish. The kernel you're running now keeps working. Run `sudo dkms status` to \
         see which driver failed, fix or update it, then run `sudo apt-get -f install` (or try Update again).\n\n"
    } else {
        ""
    }
}

/// Run a command as the current user, capturing output.
pub fn run_user(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd).args(args).output()?;
    let mut log = String::from_utf8_lossy(&out.stdout).into_owned();
    log.push_str(&String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(log)
    } else {
        bail!("Command failed:\n{}", tail(&log, 15))
    }
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

pub fn list(src: Source) -> Result<Vec<Package>> {
    match src {
        Source::Apt => apt::list(),
        Source::Rpm => rpm::list(),
        Source::Pacman => pacman::list(),
        Source::Flatpak => flatpak::list(),
        Source::Snap => snap::list(),
        Source::AppImage => appimage::list(),
        Source::Manual => manual::list(),
    }
}

/// Map of package id -> available newer version.
pub fn updates(src: Source) -> Result<HashMap<String, String>> {
    match src {
        Source::Apt => apt::updates(),
        Source::Rpm => rpm::updates(),
        Source::Pacman => pacman::updates(),
        Source::Flatpak => flatpak::updates(),
        Source::Snap => snap::updates(),
        Source::AppImage | Source::Manual => Ok(HashMap::new()),
    }
}

pub fn details(pkg: &Package) -> Details {
    let mut d = match pkg.source {
        Source::Apt => apt::details(pkg),
        Source::Rpm => rpm::details(pkg),
        Source::Pacman => pacman::details(pkg),
        Source::Flatpak => flatpak::details(pkg),
        Source::Snap => snap::details(pkg),
        Source::AppImage => appimage::details(pkg),
        Source::Manual => manual::details(pkg),
    };
    if matches!(pkg.source, Source::Rpm | Source::Pacman) && pkg.kind == crate::model::Kind::App {
        if let Some((desc, home)) = crate::appstream::from_files(&native_files(pkg)) {
            d.description = desc;
            d.homepage = home;
        }
    }
    d.telemetry = crate::telemetry::analyze(pkg, &d);
    install_dates(pkg, &mut d);
    if pkg.kind == crate::model::Kind::App {
        let desktop: Vec<String> = d.location.iter().filter(|f| f.ends_with(".desktop")).cloned().collect();
        let used = crate::usage::last_used(pkg, &desktop);
        let text = match used {
            Some(ago) => crate::usage::human_ago(ago),
            None if crate::usage::has_gnome_state() => "no record of opening it from the app menu".into(),
            None => "unknown".into(),
        };
        d.info.insert(0, ("Last opened".into(), text));
    }
    d
}

pub fn remove(pkg: &Package) -> Result<String> {
    match pkg.source {
        Source::Apt => run_privileged(&["apt-get", "remove", "-y", &pkg.id]),
        Source::Rpm => rpm::remove(pkg),
        Source::Pacman => run_privileged(&["pacman", "-R", "--noconfirm", &pkg.id]),
        Source::Flatpak => flatpak::remove(pkg),
        Source::Snap => run_privileged(&["snap", "remove", &pkg.id]),
        Source::AppImage => appimage::remove(pkg),
        Source::Manual => manual::remove(pkg),
    }
}

pub fn update(pkg: &Package) -> Result<String> {
    match pkg.source {
        Source::Apt => {
            run_privileged(&[
                "apt-get", "install", "--only-upgrade", "-y",
                // Keep the user's edited config files instead of asking.
                "-o", "Dpkg::Options::=--force-confdef", "-o", "Dpkg::Options::=--force-confold",
                &pkg.id,
            ])
        }
        Source::Rpm => rpm::update(pkg),
        // Arch does not support partial upgrades; sync the whole system.
        Source::Pacman => run_privileged(&["pacman", "-Syu", "--noconfirm"]),
        Source::Flatpak => flatpak::update(pkg),
        Source::AppImage => anyhow::bail!("AppImages are updated by downloading a new version from the app's website."),
        Source::Manual => anyhow::bail!("Apps installed by hand are updated the same way they were installed."),
        Source::Snap => run_privileged(&["snap", "refresh", &pkg.id]).map_err(|e| {
            if e.to_string().contains("has running apps") {
                anyhow::anyhow!(
                    "{} is running. Close it and try again: snaps can't be updated while open. \
                     Snap also updates it automatically once it's closed.",
                    pkg.name
                )
            } else {
                e
            }
        }),
    }
}

/// Executables / install paths worth showing out of a full file list.
pub fn interesting_paths(files: impl Iterator<Item = String>) -> Vec<String> {
    let mut bins = Vec::new();
    let mut desktop = Vec::new();
    let mut services = Vec::new();
    for f in files {
        if (f.starts_with("/usr/bin/") || f.starts_with("/usr/sbin/") || f.starts_with("/bin/")
            || f.starts_with("/sbin/") || f.starts_with("/opt/"))
            && f.matches('/').count() <= 3
        {
            bins.push(f);
        } else if f.ends_with(".desktop") && f.contains("/applications/") {
            desktop.push(f);
        } else if f.ends_with(".service") || f.ends_with(".timer") {
            services.push(f);
        }
    }
    bins.truncate(12);
    bins.extend(desktop.into_iter().take(4));
    bins.extend(services.into_iter().take(6));
    bins
}

/// Files of a native package, used for process matching and location display.
pub fn native_files(pkg: &Package) -> Vec<String> {
    let out = match pkg.source {
        Source::Apt => run("dpkg", &["-L", &pkg.id], &[]),
        Source::Rpm => run("rpm", &["-ql", &pkg.id], &[]),
        Source::Pacman => run("pacman", &["-Qlq", &pkg.id], &[]),
        _ => return Vec::new(),
    };
    out.map(|s| {
        s.lines()
            .map(str::trim)
            .filter(|l| l.starts_with('/'))
            .map(String::from)
            .collect()
    })
    .unwrap_or_default()
}

/// "Installed on" / "Last updated" lines for the details panel.
fn install_dates(pkg: &Package, d: &mut Details) {
    use crate::model::format_date;
    let first = if pkg.source == Source::Apt {
        crate::history::first_install(pkg.id.split(':').next().unwrap_or(&pkg.id))
    } else {
        None
    };
    let installed = match (&first, pkg.installed_at) {
        (Some((time, cmd)), _) => Some(format!("{time}  ({})", cmd)),
        (None, Some(t)) => Some(format!("{} ({})", format_date(t), crate::usage::human_ago(now().saturating_sub(t)))),
        (None, None) => None,
    };
    let mut at = usize::from(d.info.first().is_some_and(|(k, _)| k == "Last opened"));
    if let Some(i) = installed {
        d.info.insert(at, ("Installed on".into(), i));
        at += 1;
    }
    let updated = match pkg.source {
        Source::Apt => {
            let base = pkg.id.split(':').next().unwrap_or(&pkg.id);
            std::fs::metadata(format!("/var/lib/dpkg/info/{}.list", pkg.id))
                .or_else(|_| std::fs::metadata(format!("/var/lib/dpkg/info/{base}.list")))
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|t| t.as_secs())
        }
        Source::Snap => crate::model::birth_time(&format!("/snap/{}/current/", pkg.id)),
        _ => None,
    };
    if let (Some(u), Some(i)) = (updated, pkg.installed_at.or(Some(0))) {
        if u > i + 86_400 {
            d.info.insert(at, ("Last updated".into(), format!("{} ({})", format_date(u), crate::usage::human_ago(now().saturating_sub(u)))));
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Which native package owns a file (used to label network connections).
pub fn owner_of(path: &str) -> Option<String> {
    let out = if which("dpkg") {
        run("dpkg", &["-S", path], &[1]).ok()?.split(':').next()?.trim().to_string()
    } else if which("rpm") {
        run("rpm", &["-qf", "--qf", "%{NAME}", path], &[1]).ok()?.trim().to_string()
    } else if which("pacman") {
        run("pacman", &["-Qoq", path], &[1]).ok()?.trim().to_string()
    } else {
        return None;
    };
    (!out.is_empty() && !out.contains(' ')).then_some(out)
}
