//! Heuristic privacy / telemetry analysis.
//!
//! There is no way to *prove* a binary doesn't phone home without auditing or
//! monitoring it. This module combines:
//!   1. a curated database of software with documented telemetry,
//!   2. dependency hints (crash reporters, feedback libraries),
//!   3. sandbox permissions (Flatpak / Snap network access),
//!   4. live status of known reporting services.
use crate::model::{Details, Package, Source};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Low,
    Medium,
    High,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Info => "Info",
            Level::Low => "Low",
            Level::Medium => "Medium",
            Level::High => "High",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub level: Level,
    pub title: String,
    pub detail: String,
    pub disable: Option<String>,
    /// Automatic way to turn it off, if we know one.
    pub fix: Option<Fix>,
    /// Some(true) if it is already turned off on this machine.
    pub off: Option<bool>,
}

/// Automatic ways to turn telemetry off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    /// systemd units to `disable --now` (as root).
    Units(&'static [&'static str]),
    /// Shell script run as root.
    Script(&'static str),
    /// Command run as the current user.
    User(&'static [&'static str]),
    /// Set telemetry.telemetryLevel = "off" in VS Code's settings.json.
    VsCode,
    /// Opt-out environment variable (set to 1 for the user session).
    Env(&'static str),
}

/// System components (as opposed to apps you use directly).
pub fn is_system_component(title: &str) -> bool {
    let known = DB.iter().find(|k| k.title == title);
    known.is_some_and(|k| k.service.is_some())
        || matches!(fix_for(title), Some(Fix::Units(_) | Fix::Script(_)))
        || matches!(title, "Location service" | "KDE User Feedback" | "Ubuntu system report" | "Package usage survey")
}

pub fn fix_for(title: &str) -> Option<Fix> {
    Some(match title {
        "Package usage survey" => Fix::Script(
            "sed -i 's/^PARTICIPATE=.*/PARTICIPATE=\"no\"/' /etc/popularity-contest.conf",
        ),
        "Ubuntu system report" => Fix::User(&["ubuntu-report", "-f", "send", "no"]),
        "Crash reporting (Apport)" => Fix::Script(
            "systemctl disable --now apport.service apport-autoreport.timer apport-autoreport.path 2>/dev/null; \
             sed -i 's/^enabled=1/enabled=0/' /etc/default/apport 2>/dev/null; true",
        ),
        "Error reports daemon (whoopsie)" => Fix::Units(&["whoopsie.path", "whoopsie.service"]),
        "Kernel oops reporter" => Fix::Units(&["kerneloops.service"]),
        "MOTD news fetcher" => Fix::Script(
            "sed -i 's/^ENABLED=1/ENABLED=0/' /etc/default/motd-news 2>/dev/null; \
             systemctl disable --now motd-news.timer 2>/dev/null; true",
        ),
        "Ubuntu Pro client" => Fix::Script("pro config set apt_news=false"),
        "Landscape management agent" => Fix::Units(&["landscape-client.service"]),
        "Automatic Bug Reporting Tool" => Fix::Units(&["abrtd.service", "abrt-journal-core.service", "abrt-oops.service"]),
        "DNF 'countme'" => Fix::Script(
            "if grep -q '^countme' /etc/dnf/dnf.conf; then sed -i 's/^countme=.*/countme=0/' /etc/dnf/dnf.conf; \
             else echo countme=0 >> /etc/dnf/dnf.conf; fi",
        ),
        "Arch package statistics" => Fix::Units(&["pkgstats.timer"]),
        "VS Code telemetry" => Fix::VsCode,
        ".NET CLI telemetry" => Fix::Env("DOTNET_CLI_TELEMETRY_OPTOUT"),
        "PowerShell telemetry" => Fix::Env("POWERSHELL_TELEMETRY_OPTOUT"),
        "HashiCorp Checkpoint" => Fix::Env("CHECKPOINT_DISABLE"),
        "Azure CLI telemetry" => Fix::User(&["az", "config", "set", "core.collect_telemetry=false"]),
        "gcloud usage reporting" => Fix::User(&["gcloud", "config", "set", "disable_usage_reporting", "true"]),
        "Netdata anonymous statistics" => Fix::Script("touch /etc/netdata/.opt-out-from-anonymous-statistics"),
        _ => return None,
    })
}

fn systemctl(args: &[&str]) -> String {
    std::process::Command::new("systemctl")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub const ENV_FILE: &str = ".config/environment.d/90-telemetry-optout.conf";

/// Is this telemetry already turned off? None = can't tell.
pub fn is_off(title: &str, fix: Option<Fix>) -> Option<bool> {
    match (title, fix?) {
        (_, Fix::Units(units)) => Some(units.iter().all(|u| {
            let enabled = systemctl(&["is-enabled", u]);
            let active = systemctl(&["is-active", u]);
            !matches!(enabled.as_str(), "enabled" | "static" | "enabled-runtime") && active != "active"
        })),
        ("Package usage survey", _) => std::fs::read_to_string("/etc/popularity-contest.conf")
            .ok()
            .map(|c| !c.lines().any(|l| l.trim() == "PARTICIPATE=\"yes\"")),
        ("Crash reporting (Apport)", _) => Some(
            systemctl(&["is-active", "apport.service"]) != "active"
                && std::fs::read_to_string("/etc/default/apport").map_or(true, |c| !c.contains("enabled=1")),
        ),
        ("MOTD news fetcher", _) => std::fs::read_to_string("/etc/default/motd-news")
            .ok()
            .map(|c| c.lines().any(|l| l.trim() == "ENABLED=0")),
        ("Netdata anonymous statistics", _) => {
            Some(std::path::Path::new("/etc/netdata/.opt-out-from-anonymous-statistics").exists())
        }
        (_, Fix::VsCode) => vscode_setting().map(|(off, _)| off),
        (_, Fix::Env(var)) => {
            let home = std::env::var("HOME").unwrap_or_default();
            let file = std::fs::read_to_string(format!("{home}/{ENV_FILE}")).unwrap_or_default();
            Some(std::env::var_os(var).is_some() || file.lines().any(|l| l.starts_with(&format!("{var}="))))
        }
        _ => None,
    }
}

struct Known {
    /// Matched against package id/name (exact, or prefix if ending in '*').
    names: &'static [&'static str],
    level: Level,
    title: &'static str,
    detail: &'static str,
    disable: &'static str,
    /// systemd unit whose activity we check.
    service: Option<&'static str>,
}

const DB: &[Known] = &[
    Known { names: &["popularity-contest"], level: Level::High, title: "Package usage survey",
        detail: "Weekly uploads the list of installed packages and how recently they were used to Debian/Ubuntu (popcon), if participation is enabled.",
        disable: "sudo dpkg-reconfigure popularity-contest  → answer No, or remove the package.", service: None },
    Known { names: &["ubuntu-report"], level: Level::Medium, title: "Ubuntu system report",
        detail: "Collects hardware and install info (CPU, GPU, screen, locale, partitions) and sends it to Canonical after the first-login prompt.",
        disable: "ubuntu-report -f send no", service: None },
    Known { names: &["apport", "apport-gtk", "apport-kde", "python3-apport"], level: Level::Medium, title: "Crash reporting (Apport)",
        detail: "Collects crash dumps, which can include memory contents, and offers to upload them to errors.ubuntu.com / Launchpad.",
        disable: "sudo systemctl disable --now apport; set enabled=0 in /etc/default/apport", service: Some("apport.service") },
    Known { names: &["whoopsie", "whoopsie-preferences", "libwhoopsie0", "libwhoopsie-preferences0"], level: Level::Medium, title: "Error reports daemon (whoopsie)",
        detail: "Uploads crash reports collected by Apport to Canonical's error tracker (daisy.ubuntu.com).",
        disable: "sudo systemctl disable --now whoopsie.path whoopsie.service", service: Some("whoopsie.path") },
    Known { names: &["kerneloops"], level: Level::Medium, title: "Kernel oops reporter",
        detail: "Collects kernel crash signatures and submits them to a public statistics site.",
        disable: "sudo systemctl disable --now kerneloops", service: Some("kerneloops.service") },
    Known { names: &["motd-news-config"], level: Level::Low, title: "MOTD news fetcher",
        detail: "Periodically fetches news from motd.ubuntu.com. The request's user agent includes the OS version, architecture and uptime.",
        disable: "Set ENABLED=0 in /etc/default/motd-news", service: Some("motd-news.timer") },
    Known { names: &["ubuntu-advantage-tools", "ubuntu-pro-client", "ubuntu-pro-client-l10n"], level: Level::Low, title: "Ubuntu Pro client",
        detail: "Contacts Canonical's contract servers and shows ESM/Pro messages in apt output.",
        disable: "sudo pro config set apt_news=false", service: Some("ua-timer.timer") },
    Known { names: &["landscape-client"], level: Level::High, title: "Landscape management agent",
        detail: "Reports detailed system information (packages, hardware, processes) to a Landscape server.",
        disable: "sudo systemctl disable --now landscape-client", service: Some("landscape-client.service") },
    Known { names: &["snapd"], level: Level::Low, title: "Snap Store communication",
        detail: "snapd contacts the Snap Store several times a day to check for refreshes. Requests include the device model and installed snaps.",
        disable: "Can be postponed with: sudo snap refresh --hold", service: Some("snapd.service") },
    Known { names: &["abrt", "abrt-*", "libreport*"], level: Level::Medium, title: "Automatic Bug Reporting Tool",
        detail: "Collects crash data and can upload it to Fedora/Red Hat (FAF, Bugzilla).",
        disable: "sudo systemctl disable --now abrtd", service: Some("abrtd.service") },
    Known { names: &["dnf", "dnf5", "libdnf*"], level: Level::Low, title: "DNF 'countme'",
        detail: "On Fedora, DNF sends one anonymous 'countme' flag per week with repository metadata requests, used to estimate the number of users.",
        disable: "Add countme=0 to /etc/dnf/dnf.conf (or each .repo file)", service: None },
    Known { names: &["pkgstats"], level: Level::High, title: "Arch package statistics",
        detail: "Weekly sends the list of installed packages, CPU architecture and mirror to pkgstats.archlinux.de.",
        disable: "sudo systemctl disable --now pkgstats.timer", service: Some("pkgstats.timer") },
    Known { names: &["libkuserfeedback*", "kuserfeedback*", "libkf5userfeedback*", "libkf6userfeedback*"], level: Level::Low, title: "KDE User Feedback",
        detail: "Opt-in framework that KDE apps use to send usage statistics. It is disabled until you enable it.",
        disable: "System Settings → Privacy → User Feedback → Disabled", service: None },
    Known { names: &["geoclue-2.0", "geoclue", "geoclue2"], level: Level::Low, title: "Location service",
        detail: "Resolves your location by sending nearby Wi-Fi access points to an online location service when an app asks for your location.",
        disable: "Settings → Privacy → Location Services → Off", service: None },
    Known { names: &["code", "code-insiders", "com.visualstudio.code", "visual-studio-code-bin"], level: Level::High, title: "VS Code telemetry",
        detail: "Microsoft builds of VS Code send usage, error and crash telemetry by default. Extensions may send their own.",
        disable: "Settings → \"telemetry.telemetryLevel\": \"off\" (or use VSCodium)", service: None },
    Known { names: &["google-chrome-stable", "google-chrome-beta", "google-chrome-unstable", "com.google.Chrome"], level: Level::High, title: "Google Chrome",
        detail: "Optional usage statistics and crash reports, plus Google services (Safe Browsing, sync, field trials, search suggestions) enabled by default.",
        disable: "chrome://settings/privacy — disable usage stats and \"Make searches and browsing better\"", service: None },
    Known { names: &["microsoft-edge-stable", "microsoft-edge-beta", "microsoft-edge-dev", "com.microsoft.Edge"], level: Level::High, title: "Microsoft Edge diagnostics",
        detail: "Sends required diagnostic data to Microsoft. Optional diagnostic data and personalization are enabled by default.",
        disable: "edge://settings/privacy — turn off optional diagnostic data", service: None },
    Known { names: &["firefox", "firefox-esr", "org.mozilla.firefox", "thunderbird", "org.mozilla.Thunderbird"], level: Level::Medium, title: "Mozilla telemetry",
        detail: "Technical and interaction data, studies and crash reports are on by default. It is documented and can be turned off.",
        disable: "Settings → Privacy & Security → Firefox Data Collection and Use → uncheck all", service: None },
    Known { names: &["brave-browser", "com.brave.Browser"], level: Level::Low, title: "Brave P3A analytics",
        detail: "Privacy-preserving product analytics and daily usage pings are on by default.",
        disable: "brave://settings/privacy — disable P3A and daily usage ping", service: None },
    Known { names: &["opera-stable", "com.opera.Opera"], level: Level::High, title: "Opera usage reports",
        detail: "Sends usage statistics and feature data to Opera by default.",
        disable: "Settings → Privacy → disable 'Help improve Opera'", service: None },
    Known { names: &["steam", "steam-installer", "steam-launcher", "com.valvesoftware.Steam"], level: Level::Medium, title: "Steam client",
        detail: "Optional hardware survey. Gameplay, purchase and usage data is tied to your account.",
        disable: "Decline the hardware survey; review Steam → Settings → Privacy", service: None },
    Known { names: &["spotify-client", "spotify", "com.spotify.Client"], level: Level::High, title: "Spotify analytics",
        detail: "Extensive listening, device and interaction analytics tied to your account.",
        disable: "Not fully possible; limit via Account → Privacy settings", service: None },
    Known { names: &["discord", "com.discordapp.Discord"], level: Level::High, title: "Discord analytics",
        detail: "Collects usage analytics and activity data, including which games are running.",
        disable: "User Settings → Data & Privacy → disable 'Use data to improve Discord'", service: None },
    Known { names: &["slack", "slack-desktop", "com.slack.Slack"], level: Level::Medium, title: "Slack analytics",
        detail: "Usage analytics and crash reports are sent to Slack/Salesforce.",
        disable: "Limited — controlled by workspace admins", service: None },
    Known { names: &["zoom", "us.zoom.Zoom"], level: Level::High, title: "Zoom telemetry",
        detail: "Collects device, network and usage data for diagnostics and analytics.",
        disable: "Limited — Settings → Statistics", service: None },
    Known { names: &["teams", "teams-for-linux", "skypeforlinux", "com.skype.Client"], level: Level::High, title: "Microsoft communications app",
        detail: "Sends diagnostic and usage telemetry to Microsoft.",
        disable: "Limited in-app privacy settings", service: None },
    Known { names: &["dotnet-*", "dotnet", "dotnet-sdk*"], level: Level::Medium, title: ".NET CLI telemetry",
        detail: "The dotnet CLI sends usage data (commands, OS, hashed MAC) to Microsoft by default.",
        disable: "export DOTNET_CLI_TELEMETRY_OPTOUT=1", service: None },
    Known { names: &["powershell", "powershell-preview"], level: Level::Medium, title: "PowerShell telemetry",
        detail: "Sends basic usage telemetry to Microsoft at startup.",
        disable: "export POWERSHELL_TELEMETRY_OPTOUT=1", service: None },
    Known { names: &["azure-cli"], level: Level::Medium, title: "Azure CLI telemetry",
        detail: "Collects command usage data by default.",
        disable: "az config set core.collect_telemetry=false", service: None },
    Known { names: &["google-cloud-cli", "google-cloud-sdk"], level: Level::Low, title: "gcloud usage reporting",
        detail: "Opt-in anonymous usage statistics (asked at first run).",
        disable: "gcloud config set disable_usage_reporting true", service: None },
    Known { names: &["terraform", "packer", "vagrant", "consul", "vault"], level: Level::Low, title: "HashiCorp Checkpoint",
        detail: "Checks a HashiCorp service for new versions and security bulletins, sending the product version and OS.",
        disable: "export CHECKPOINT_DISABLE=1", service: None },
    Known { names: &["netdata"], level: Level::Medium, title: "Netdata anonymous statistics",
        detail: "Sends anonymous usage statistics and crash events by default.",
        disable: "touch /etc/netdata/.opt-out-from-anonymous-statistics", service: None },
    Known { names: &["docker-desktop"], level: Level::Medium, title: "Docker Desktop analytics",
        detail: "Sends usage statistics to Docker by default.",
        disable: "Settings → General → uncheck 'Send usage statistics'", service: None },
    Known { names: &["obsidian", "md.obsidian.Obsidian"], level: Level::Low, title: "Obsidian",
        detail: "Checks for updates. No analytics according to its privacy policy.",
        disable: "Settings → General → Automatic updates off", service: None },
    Known { names: &["jetbrains-toolbox", "intellij-idea-*", "pycharm-*", "webstorm", "clion", "goland", "rider", "com.jetbrains.*"], level: Level::Medium, title: "JetBrains usage statistics",
        detail: "Opt-in anonymous usage statistics (asked at first run). Also checks for updates and licenses.",
        disable: "Settings → Appearance & Behavior → System Settings → Data Sharing", service: None },
    Known { names: &["unityhub"], level: Level::Medium, title: "Unity Hub analytics",
        detail: "Sends editor and hub usage analytics to Unity.",
        disable: "Preferences → Privacy", service: None },
    Known { names: &["nvidia-settings"], level: Level::Info, title: "NVIDIA tools",
        detail: "The Linux driver has no telemetry. GeForce Experience-style telemetry exists only on Windows.",
        disable: "-", service: None },
];

/// Libraries whose presence in dependencies suggests crash/usage reporting.
const DEP_HINTS: &[(&str, Level, &str)] = &[
    ("libsentry", Level::Medium, "Sentry SDK — sends crash and error reports to a Sentry server"),
    ("sentry-native", Level::Medium, "Sentry SDK — sends crash and error reports"),
    ("crashpad", Level::Medium, "Crashpad — Chromium/Google crash uploader"),
    ("breakpad", Level::Medium, "Breakpad — crash dump uploader"),
    ("userfeedback", Level::Low, "KDE UserFeedback — opt-in usage statistics"),
    ("libwhoopsie", Level::Medium, "Ubuntu error reporting library"),
    ("python3-apport", Level::Low, "Apport crash-hook integration"),
    ("libreport", Level::Medium, "ABRT reporting library"),
    ("google-analytics", Level::High, "Google Analytics"),
];

fn matches(pattern: &str, id: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => id.starts_with(prefix),
        None => pattern == id,
    }
}

fn lookup(pkg: &Package) -> Option<&'static Known> {
    let base = pkg.id.split(':').next().unwrap_or(&pkg.id);
    DB.iter()
        .find(|k| k.names.iter().any(|n| matches(n, base) || matches(n, &pkg.name)))
}

/// Cheap check used while listing (no subprocesses).
pub fn quick_level(pkg: &Package) -> Option<Level> {
    let k = lookup(pkg)?;
    let level = match is_off(k.title, fix_for(k.title)) {
        Some(true) => Level::Low, // already turned off
        _ => k.level,
    };
    (level > Level::Info).then_some(level)
}

/// Read the user's own config for apps whose telemetry is a setting.
/// Returns (disabled, description).
pub fn vscode_settings_path() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    [
        format!("{home}/.config/Code/User/settings.json"),
        format!("{home}/.var/app/com.visualstudio.code/config/Code/User/settings.json"),
        format!("{home}/.config/Code - Insiders/User/settings.json"),
    ]
    .into_iter()
    .find(|p| std::path::Path::new(p).exists())
}

fn vscode_setting() -> Option<(bool, String)> {
    let home = std::env::var("HOME").ok()?;
    let paths = [
        format!("{home}/.config/Code/User/settings.json"),
        format!("{home}/.var/app/com.visualstudio.code/config/Code/User/settings.json"),
        format!("{home}/.config/Code - Insiders/User/settings.json"),
    ];
    let content = paths.iter().find_map(|p| std::fs::read_to_string(p).ok())?;
    // settings.json is JSONC; a targeted text lookup is robust enough here.
    let value_of = |key: &str| -> Option<String> {
        let line = content.lines().find(|l| l.contains(key) && !l.trim_start().starts_with("//"))?;
        let v = line.split_once(':')?.1.trim().trim_end_matches(',').trim().trim_matches('"');
        Some(v.to_string())
    };
    Some(match value_of("\"telemetry.telemetryLevel\"") {
        Some(v) if v == "off" => (true, "Your setting: telemetry.telemetryLevel = \"off\". Telemetry is disabled.".into()),
        Some(v) => (false, format!("Your setting: telemetry.telemetryLevel = \"{v}\". Telemetry is still sent.")),
        None if value_of("\"telemetry.enableTelemetry\"").as_deref() == Some("false") => {
            (true, "Your setting: telemetry.enableTelemetry = false (legacy). Telemetry is disabled.".into())
        }
        None => (false, "Your setting: not set, so the default \"all\" applies and usage, errors and crashes are sent.".into()),
    })
}

fn service_state(unit: &str) -> Option<String> {
    let out = std::process::Command::new("systemctl")
        .args(["is-active", unit])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

pub fn analyze(pkg: &Package, d: &Details) -> Vec<Finding> {
    let mut f = Vec::new();
    if let Some(k) = lookup(pkg) {
        let mut detail = k.detail.to_string();
        if let Some(unit) = k.service {
            if let Some(state) = service_state(unit) {
                detail.push_str(&format!("\nService {unit}: {state}"));
            }
        }
        let mut level = k.level;
        if let Some((disabled, msg)) = (k.title == "VS Code telemetry").then(vscode_setting).flatten() {
            detail.push('\n');
            detail.push_str(&msg);
            if disabled {
                level = Level::Low;
            }
        }
        if pkg.id == "popularity-contest" {
            if let Ok(conf) = std::fs::read_to_string("/etc/popularity-contest.conf") {
                let on = conf.lines().any(|l| l.trim() == "PARTICIPATE=\"yes\"");
                detail.push_str(if on { "\nParticipation: ENABLED" } else { "\nParticipation: disabled" });
            }
        }
        f.push(Finding {
            level,
            title: k.title.into(),
            detail,
            disable: (k.disable != "-").then(|| k.disable.to_string()),
            fix: fix_for(k.title),
            off: is_off(k.title, fix_for(k.title)),
        });
        if f.last().is_some_and(|x| x.off == Some(true)) {
            f.last_mut().unwrap().level = f.last().unwrap().level.min(Level::Low);
        }
    }
    for dep in &d.depends_on {
        for (hint, level, what) in DEP_HINTS {
            if dep.contains(hint) {
                f.push(Finding {
                    level: *level,
                    title: format!("Depends on {dep}"),
                    detail: what.to_string(),
                    disable: None,
                    fix: None,
                    off: None,
                });
            }
        }
    }
    match pkg.source {
        Source::Flatpak => {
            let net = d.info.iter().any(|(k, v)| k == "Permission: shared" && v.contains("network"));
            if pkg.kind == crate::model::Kind::App {
                f.push(Finding {
                    level: Level::Info,
                    title: if net { "Sandbox: has internet access" } else { "Sandbox: no internet access" }.into(),
                    detail: if net {
                        "The app is allowed to make network connections, so it could send data. That doesn't mean it does."
                    } else {
                        "The Flatpak sandbox blocks network access, so the app cannot phone home."
                    }
                    .into(),
                    disable: net.then(|| format!("flatpak override --user --unshare=network {}", pkg.id)),
                    fix: None,
                    off: None,
                });
            }
        }
        Source::Snap if pkg.extra.contains("classic") => {
            f.push(Finding {
                level: Level::Info,
                title: "Classic snap: not sandboxed".into(),
                detail: "Classic confinement gives the app full access to your files and the network, like a regular program.".into(),
                disable: None,
                fix: None,
                off: None,
            });
        }
        Source::Snap => {
            let net = d.info.iter().any(|(k, _)| k == "Interface: network");
            if pkg.kind == crate::model::Kind::App {
                f.push(Finding {
                    level: Level::Info,
                    title: if net { "Sandbox: network interface connected" } else { "Sandbox: no network interface" }.into(),
                    detail: "Snap confinement controls network access through the 'network' interface.".into(),
                    disable: net.then(|| format!("sudo snap disconnect {}:network", pkg.id)),
                    fix: None,
                    off: None,
                });
            }
        }
        _ => {}
    }
    f.sort_by(|a, b| b.level.cmp(&a.level));
    f
}
