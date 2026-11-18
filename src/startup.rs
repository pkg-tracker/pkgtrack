//! Things that start automatically: system services/timers, user services,
//! and desktop autostart entries.
use crate::advice::{Action, ActionKind};
use crate::backends::run;
use anyhow::{Context, Result};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    System,
    User,
    Autostart,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::System => "system service",
            Kind::User => "user service",
            Kind::Autostart => "login app",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Hint {
    Disable,
    Optional,
    Unknown,
    Keep,
}

impl Hint {
    pub fn label(self) -> &'static str {
        match self {
            Hint::Disable => "Safe to turn off",
            Hint::Optional => "Optional",
            Hint::Unknown => "",
            Hint::Keep => "Keep on",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    /// unit name, or .desktop file name for autostart entries
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: Kind,
    pub enabled: bool,
    pub active: bool,
    pub hint: Hint,
    pub why: String,
}

/// (pattern, hint, explanation). Pattern matches unit/desktop id; '*' suffix = prefix.
const KNOWN: &[(&str, Hint, &str)] = &[
    // telemetry / phoning home
    ("motd-news.timer", Hint::Disable, "Downloads news for the terminal login message from Canonical."),
    ("ua-timer.timer", Hint::Disable, "Ubuntu Pro status checks and messages."),
    ("ubuntu-advantage.service", Hint::Disable, "Ubuntu Pro background service."),
    ("ubuntu-advantage-notification.desktop", Hint::Disable, "Ubuntu Pro notifications at login."),
    ("apport.service", Hint::Disable, "Crash reporting: collects crash dumps to send to Ubuntu."),
    ("apport-autoreport*", Hint::Disable, "Automatically uploads crash reports."),
    ("whoopsie*", Hint::Disable, "Uploads error reports to Canonical."),
    ("kerneloops.service", Hint::Disable, "Sends kernel crash signatures to a statistics site."),
    ("pkgstats.timer", Hint::Disable, "Sends your package list to Arch statistics."),
    ("abrtd.service", Hint::Disable, "Fedora crash reporting."),
    ("geoclue-demo-agent.desktop", Hint::Optional, "Lets apps ask for your location. Turn off if you never use location."),
    // hardware you may not have
    ("cups.service", Hint::Optional, "Printing. Turn off if you never print."),
    ("cups-browsed.service", Hint::Disable, "Finds network printers automatically. Rarely needed and has had security issues."),
    ("cups.path", Hint::Optional, "Starts printing on demand."),
    ("print-applet.desktop", Hint::Optional, "Printer status icon."),
    ("bluetooth.service", Hint::Optional, "Bluetooth. Turn off if you never use Bluetooth devices."),
    ("ModemManager.service", Hint::Disable, "Mobile broadband (3G/4G) modems. Not needed on most PCs."),
    ("org.gnome.SettingsDaemon.Wwan.desktop", Hint::Optional, "Mobile broadband settings helper."),
    ("org.gnome.SettingsDaemon.Smartcard.desktop", Hint::Optional, "Smart card readers."),
    ("org.gnome.SettingsDaemon.Wacom.desktop", Hint::Optional, "Wacom drawing tablets."),
    ("spice-vdagent*", Hint::Disable, "Only useful when Linux runs inside a SPICE virtual machine."),
    ("openvpn.service", Hint::Optional, "OpenVPN tunnels."),
    ("avahi-daemon.service", Hint::Optional, "Discovers printers and shares on your network. Announces this PC to the local network."),
    ("avahi-daemon.socket", Hint::Optional, "Starts network discovery on demand."),
    ("brltty*", Hint::Disable, "Braille displays."),
    ("orca-autostart.desktop", Hint::Optional, "Screen reader."),
    ("openrazer-daemon.service", Hint::Optional, "Razer keyboard/mouse lighting."),
    ("polychromatic-autostart.desktop", Hint::Optional, "Razer lighting tray app."),
    ("speech-dispatcher*", Hint::Optional, "Text-to-speech."),
    // slows boot
    ("NetworkManager-wait-online.service", Hint::Optional, "Delays boot until the network is up. Only servers usually need it."),
    ("systemd-networkd-wait-online.service", Hint::Optional, "Delays boot until the network is up."),
    ("plymouth*", Hint::Keep, "Boot splash screen."),
    // background apps
    ("org.gnome.Software.desktop", Hint::Optional, "GNOME Software runs in the background to check for updates (uses memory)."),
    ("gnome-software-service.desktop", Hint::Optional, "GNOME Software background update checks."),
    ("tracker-miner-fs-3.desktop", Hint::Optional, "Indexes your files for search. Turning it off makes file search in Activities slower."),
    ("org.gnome.Evolution-alarm-notify.desktop", Hint::Optional, "Calendar reminders. Turn off if you don't use the GNOME calendar."),
    ("org.gnome.DejaDup.Monitor.desktop", Hint::Optional, "Backup scheduler. Keep if you use Déjà Dup backups."),
    ("snapd.apparmor.service", Hint::Keep, "Security confinement for snaps."),
    ("snap-userd-autostart.desktop", Hint::Keep, "Lets snaps open links and files."),
    ("fwupd-refresh.timer", Hint::Keep, "Checks for firmware updates."),
    ("apt-daily*", Hint::Keep, "Downloads and installs security updates."),
    ("unattended-upgrades.service", Hint::Keep, "Installs security updates automatically."),
    ("snapd*", Hint::Keep, "Runs and updates snaps (Firefox, VS Code, …)."),
    ("man-db.timer", Hint::Optional, "Rebuilds the manual-page index."),
    ("e2scrub*", Hint::Keep, "Checks filesystems for errors."),
    ("fstrim.timer", Hint::Keep, "Keeps SSDs fast."),
    ("logrotate.timer", Hint::Keep, "Stops logs from filling your disk."),
    ("anacron*", Hint::Keep, "Runs daily system maintenance."),
    ("cron.service", Hint::Keep, "Runs scheduled tasks."),
    ("ssh.service", Hint::Optional, "Lets other computers log in remotely. Turn off if you don't use SSH into this PC."),
    ("sshd.service", Hint::Optional, "Lets other computers log in remotely. Turn off if you don't use SSH into this PC."),
    ("docker.service", Hint::Optional, "Docker. Turn off if you only use it occasionally (start it when needed)."),
    ("containerd.service", Hint::Optional, "Container runtime for Docker."),
    ("libvirtd.service", Hint::Optional, "Virtual machines."),
    ("postgresql*", Hint::Optional, "PostgreSQL database server."),
    ("mysql.service", Hint::Optional, "MySQL database server."),
    ("apache2.service", Hint::Optional, "Web server."),
    ("nginx.service", Hint::Optional, "Web server."),
    ("nordvpn*", Hint::Optional, "NordVPN background service."),
    ("pkgtrack-*", Hint::Optional, "Package Tracker scheduled task."),
    ("cloud-init*", Hint::Disable, "Sets up cloud virtual machines on first boot. Not needed on a desktop PC."),
    ("cloud-config.service", Hint::Disable, "Part of cloud-init (cloud VMs only)."),
    ("cloud-final.service", Hint::Disable, "Part of cloud-init (cloud VMs only)."),
    ("rpcbind.service", Hint::Disable, "Needed only for NFS network shares. It listens on your network (port 111)."),
    ("rpcbind.socket", Hint::Disable, "Needed only for NFS network shares."),
    ("gnome-remote-desktop.service", Hint::Optional, "Lets others connect to this PC's screen when Remote Desktop is turned on in Settings."),
    ("lttng-sessiond.service", Hint::Optional, "Developer tracing tool. Not needed for normal use."),
    ("rsync.service", Hint::Optional, "rsync file-sharing server. Not needed to use rsync as a command."),
    ("qemu-kvm.service", Hint::Optional, "Virtual machines (QEMU/KVM)."),
    ("libvirt-guests.service", Hint::Optional, "Saves and restores running virtual machines at shutdown."),
    ("smartmontools.service", Hint::Keep, "Warns you when a disk is about to fail."),
    ("lvm2-monitor.service", Hint::Keep, "Disk volume management."),
    ("blk-availability.service", Hint::Keep, "Disk setup at shutdown."),
    ("binfmt-support.service", Hint::Keep, "Lets the system run different program formats."),
    ("console-setup.service", Hint::Keep, "Console font and keyboard."),
    ("keyboard-setup.service", Hint::Keep, "Keyboard layout."),
    ("setvtrgb.service", Hint::Keep, "Console colours."),
    ("dpkg-db-backup.timer", Hint::Keep, "Backs up the package database daily."),
    ("lm-sensors.service", Hint::Keep, "Temperature sensors."),
    ("launchpadlib-cache-clean.timer", Hint::Keep, "Cleans a small cache."),
    ("session-migration.service", Hint::Keep, "Migrates desktop settings after upgrades."),
    ("org.freedesktop.IBus.session.GNOME.service", Hint::Keep, "Keyboard input methods."),
    ("networkd-dispatcher.service", Hint::Keep, "Runs network scripts."),
    ("NetworkManager-dispatcher.service", Hint::Keep, "Runs network scripts."),
    ("dmesg.service", Hint::Keep, "Saves kernel messages from boot."),
    ("vmware*", Hint::Optional, "VMware virtual machines."),
    // core
    ("gdm*", Hint::Keep, "Login screen."),
    ("NetworkManager.service", Hint::Keep, "Networking (Wi-Fi, Ethernet)."),
    ("nm-applet.desktop", Hint::Keep, "Network icon."),
    ("dbus*", Hint::Keep, "Core system messaging."),
    ("systemd-*", Hint::Keep, "Core system component."),
    ("polkit*", Hint::Keep, "Permission prompts."),
    ("udisks2.service", Hint::Keep, "Mounts USB drives and disks."),
    ("accounts-daemon.service", Hint::Keep, "User accounts."),
    ("power-profiles-daemon.service", Hint::Keep, "Power modes."),
    ("upower.service", Hint::Keep, "Battery and power."),
    ("thermald.service", Hint::Keep, "Prevents overheating."),
    ("pipewire*", Hint::Keep, "Sound and screen sharing."),
    ("wireplumber*", Hint::Keep, "Sound."),
    ("pulseaudio*", Hint::Keep, "Sound."),
    ("gnome-keyring*", Hint::Keep, "Stores your passwords and keys."),
    ("gcr-ssh-agent*", Hint::Keep, "SSH key agent."),
    ("at-spi-dbus-bus.desktop", Hint::Keep, "Accessibility bus; some apps need it."),
    ("org.gnome.SettingsDaemon.*", Hint::Keep, "Part of GNOME settings."),
    ("im-launch.desktop", Hint::Keep, "Keyboard input methods."),
    ("apparmor.service", Hint::Keep, "Security."),
    ("ufw.service", Hint::Keep, "Firewall."),
    ("rsyslog.service", Hint::Keep, "System logs."),
    ("getty@*", Hint::Keep, "Text consoles."),
    ("wpa_supplicant.service", Hint::Keep, "Wi-Fi."),
    ("switcheroo-control.service", Hint::Keep, "Switches between GPUs."),
    ("gpu-manager.service", Hint::Keep, "Graphics driver setup."),
    ("kerneloops*", Hint::Disable, "Kernel crash reporting."),
    ("grub*", Hint::Keep, "Boot loader."),
    ("secureboot*", Hint::Keep, "Secure boot."),
    ("filter-chain.service", Hint::Keep, "Audio processing."),
    ("obex.service", Hint::Optional, "Bluetooth file transfer."),
];

fn lookup(id: &str) -> (Hint, String) {
    for (pat, hint, why) in KNOWN {
        let hit = match pat.strip_suffix('*') {
            Some(p) => id.starts_with(p),
            None => *pat == id,
        };
        if hit {
            return (*hint, why.to_string());
        }
    }
    (Hint::Unknown, String::new())
}

fn known_unit(id: &str) -> bool {
    lookup(id).0 != Hint::Unknown
}

fn units(user: bool) -> Vec<Item> {
    let mut base = vec![];
    if user {
        base.push("--user");
    }
    let mut args = base.clone();
    args.extend(["list-unit-files", "--type=service", "--type=timer", "--type=socket", "--type=path", "--no-legend", "--no-pager"]);
    let Ok(out) = run("systemctl", &args, &[]) else { return Vec::new() };
    let mut names = Vec::new();
    for l in out.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 2 {
            continue;
        }
        let (name, state) = (f[0], f[1]);
        let enabled = state == "enabled";
        // Show enabled units, plus known ones the user turned off (to turn back on).
        let show = enabled || (state == "disabled" && known_unit(name) && f.get(2) == Some(&"enabled"));
        let socket_or_path = name.ends_with(".socket") || name.ends_with(".path");
        if show && !name.contains('@') && (!socket_or_path || known_unit(name)) {
            names.push((name.to_string(), enabled));
        }
    }
    if names.is_empty() {
        return Vec::new();
    }
    // Descriptions and active states in one call.
    let mut args = base.clone();
    args.extend(["show", "--property=Id,Description,ActiveState", "--no-pager"]);
    let refs: Vec<&str> = names.iter().map(|(n, _)| n.as_str()).collect();
    args.extend(refs.iter());
    let mut desc: BTreeMap<String, (String, bool)> = BTreeMap::new();
    if let Ok(out) = run("systemctl", &args, &[]) {
        for block in out.split("\n\n") {
            let mut id = String::new();
            let mut d = String::new();
            let mut active = false;
            for l in block.lines() {
                if let Some(v) = l.strip_prefix("Id=") {
                    id = v.into();
                } else if let Some(v) = l.strip_prefix("Description=") {
                    d = v.into();
                } else if let Some(v) = l.strip_prefix("ActiveState=") {
                    active = v == "active";
                }
            }
            desc.insert(id, (d, active));
        }
    }
    names
        .into_iter()
        .map(|(id, enabled)| {
            let (description, active) = desc.get(&id).cloned().unwrap_or_default();
            let (hint, why) = lookup(&id);
            Item {
                name: id.trim_end_matches(".service").to_string(),
                id,
                description,
                kind: if user { Kind::User } else { Kind::System },
                enabled,
                active,
                hint,
                why,
            }
        })
        .collect()
}

struct DesktopEntry {
    fields: BTreeMap<String, String>,
}

fn parse_desktop(content: &str) -> DesktopEntry {
    let mut fields = BTreeMap::new();
    let mut in_main = false;
    for l in content.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            in_main = l == "[Desktop Entry]";
        } else if in_main {
            if let Some((k, v)) = l.split_once('=') {
                fields.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
            }
        }
    }
    DesktopEntry { fields }
}

fn user_autostart_dir() -> String {
    format!("{}/.config/autostart", std::env::var("HOME").unwrap_or_default())
}

fn autostart() -> Vec<Item> {
    let desktops: Vec<String> = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .map(|s| s.to_lowercase())
        .collect();
    let mut files: BTreeMap<String, String> = BTreeMap::new(); // name -> path (user overrides system)
    for dir in ["/etc/xdg/autostart".to_string(), user_autostart_dir()] {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.ends_with(".desktop") {
                    files.insert(n, e.path().to_string_lossy().into_owned());
                }
            }
        }
    }
    let mut v = Vec::new();
    for (file, path) in files {
        let Ok(content) = std::fs::read_to_string(&path) else { continue };
        let e = parse_desktop(&content);
        let get = |k: &str| e.fields.get(k).map(String::as_str).unwrap_or("");
        let in_list = |k: &str| get(k).split(';').any(|d| !d.is_empty() && desktops.contains(&d.to_lowercase()));
        if (!get("OnlyShowIn").is_empty() && !in_list("OnlyShowIn")) || in_list("NotShowIn") {
            continue;
        }
        if file.starts_with("gnome-initial-setup") {
            continue;
        }
        let enabled = get("Hidden") != "true" && get("X-GNOME-Autostart-enabled") != "false";
        let (hint, why) = lookup(&file);
        v.push(Item {
            id: file.clone(),
            name: if get("Name").is_empty() { file.trim_end_matches(".desktop").to_string() } else { get("Name").to_string() },
            description: get("Comment").to_string(),
            kind: Kind::Autostart,
            enabled,
            active: enabled,
            hint,
            why,
        });
    }
    v
}

pub fn list() -> Vec<Item> {
    let mut v = autostart();
    v.extend(units(true));
    v.extend(units(false));
    v.sort_by(|a, b| {
        (a.hint, !a.enabled, a.name.to_lowercase()).cmp(&(b.hint, !b.enabled, b.name.to_lowercase()))
    });
    v
}

pub fn toggle_action(item: &Item, enable: bool) -> Action {
    let verb = if enable { "enable" } else { "disable" };
    let kind = match item.kind {
        Kind::System => ActionKind::Root(vec!["systemctl".into(), verb.into(), "--now".into(), item.id.clone()]),
        Kind::User => ActionKind::User(vec!["systemctl".into(), "--user".into(), verb.into(), "--now".into(), item.id.clone()]),
        Kind::Autostart => ActionKind::Autostart(item.id.clone(), enable),
    };
    let confirm = (!enable && item.hint == Hint::Keep).then(|| {
        format!("Turn off {}? {} Your system may not work properly without it.", item.name, item.why)
    });
    Action { label: if enable { "Turn on".into() } else { "Turn off".into() }, kind, confirm }
}

/// Enable/disable a login app by writing a per-user override (no root needed).
pub fn set_autostart(file: &str, enable: bool) -> Result<String> {
    let user_dir = user_autostart_dir();
    let user_path = format!("{user_dir}/{file}");
    let system_path = format!("/etc/xdg/autostart/{file}");
    let user_exists = std::path::Path::new(&user_path).exists();
    let system_exists = std::path::Path::new(&system_path).exists();

    if enable && user_exists && system_exists {
        // Our override only hides the system entry: drop it to restore the default.
        let content = std::fs::read_to_string(&user_path)?;
        let system = std::fs::read_to_string(&system_path).unwrap_or_default();
        let strip = |s: &str| s.lines().filter(|l| !l.starts_with("Hidden=") && !l.starts_with("X-GNOME-Autostart-enabled=")).collect::<Vec<_>>().join("\n");
        if strip(&content).trim() == strip(&system).trim() {
            std::fs::remove_file(&user_path)?;
            return Ok(format!("{file} will start at login again."));
        }
    }
    let src = if user_exists { &user_path } else { &system_path };
    let content = std::fs::read_to_string(src).with_context(|| format!("cannot read {src}"))?;
    let mut out = Vec::new();
    let mut in_main = false;
    let mut wrote = false;
    for l in content.lines() {
        if l.trim().starts_with('[') {
            if in_main && !wrote {
                out.push(format!("Hidden={}", !enable));
                wrote = true;
            }
            in_main = l.trim() == "[Desktop Entry]";
        }
        if in_main && (l.starts_with("Hidden=") || l.starts_with("X-GNOME-Autostart-enabled=")) {
            if l.starts_with("Hidden=") {
                out.push(format!("Hidden={}", !enable));
                wrote = true;
            } else {
                out.push(format!("X-GNOME-Autostart-enabled={enable}"));
            }
            continue;
        }
        out.push(l.to_string());
    }
    if !wrote {
        out.push(format!("Hidden={}", !enable));
    }
    std::fs::create_dir_all(&user_dir)?;
    std::fs::write(&user_path, out.join("\n") + "\n")?;
    Ok(format!("{file} {} start at login.", if enable { "will" } else { "won't" }))
}

// ---- daily update notification (systemd user timer)

const TIMER: &str = "pkgtrack-notify.timer";

pub fn notify_enabled() -> bool {
    run("systemctl", &["--user", "is-enabled", TIMER], &[1]).map(|s| s.trim() == "enabled").unwrap_or(false)
}

pub fn set_notify(enable: bool) -> Result<String> {
    let home = std::env::var("HOME")?;
    let dir = format!("{home}/.config/systemd/user");
    if !enable {
        let _ = crate::backends::run_user("systemctl", &["--user", "disable", "--now", TIMER]);
        let _ = std::fs::remove_file(format!("{dir}/{TIMER}"));
        let _ = std::fs::remove_file(format!("{dir}/pkgtrack-notify.service"));
        return Ok("Daily update check turned off.".into());
    }
    let installed = format!("{home}/.local/bin/pkgtrack");
    let exe = if std::path::Path::new(&installed).exists() {
        installed
    } else {
        std::env::current_exe()?.to_string_lossy().into_owned()
    };
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        format!("{dir}/pkgtrack-notify.service"),
        format!("[Unit]\nDescription=Package Tracker update check\n\n[Service]\nType=oneshot\nExecStart={exe} --notify\n"),
    )?;
    std::fs::write(
        format!("{dir}/{TIMER}"),
        "[Unit]\nDescription=Daily Package Tracker update check\n\n[Timer]\nOnBootSec=15min\nOnCalendar=daily\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n",
    )?;
    crate::backends::run_user("systemctl", &["--user", "daemon-reload"])?;
    crate::backends::run_user("systemctl", &["--user", "enable", "--now", TIMER])?;
    Ok("You'll get a notification once a day when updates are available.".into())
}

// ---- weekly cleanup of old snap revisions (system timer)

pub const SNAP_CLEAN_TIMER: &str = "pkgtrack-snap-clean.timer";

pub fn snap_clean_enabled() -> bool {
    std::path::Path::new(&format!("/etc/systemd/system/{SNAP_CLEAN_TIMER}")).exists()
}

pub fn snap_clean_action(enable: bool) -> Action {
    let script = if enable {
        r#"cat > /etc/systemd/system/pkgtrack-snap-clean.service <<'EOF'
[Unit]
Description=Remove old (disabled) snap revisions

[Service]
Type=oneshot
ExecStart=/bin/sh -c 'LC_ALL=C snap list --all | awk "/disabled/{print \$1, \$3}" | while read name rev; do snap remove "$name" --revision="$rev"; done'
EOF
cat > /etc/systemd/system/pkgtrack-snap-clean.timer <<'EOF'
[Unit]
Description=Weekly cleanup of old snap revisions

[Timer]
OnCalendar=weekly
Persistent=true

[Install]
WantedBy=timers.target
EOF
systemctl daemon-reload && systemctl enable --now pkgtrack-snap-clean.timer"#
    } else {
        "systemctl disable --now pkgtrack-snap-clean.timer; rm -f /etc/systemd/system/pkgtrack-snap-clean.timer /etc/systemd/system/pkgtrack-snap-clean.service; systemctl daemon-reload"
    };
    Action {
        label: if enable { "Clean weekly".into() } else { "Stop weekly cleanup".into() },
        kind: ActionKind::RootScript(script.into()),
        confirm: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_override_roundtrip() {
        let _guard = crate::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("pkgtrack-as-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".config/autostart")).unwrap();
        // A user-only login app.
        std::fs::write(dir.join(".config/autostart/demo.desktop"), "[Desktop Entry]\nName=Demo\nExec=demo\n[Desktop Action x]\nName=X\n").unwrap();
        let old = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", &dir) };
        set_autostart("demo.desktop", false).unwrap();
        let c = std::fs::read_to_string(dir.join(".config/autostart/demo.desktop")).unwrap();
        assert!(c.contains("Exec=demo\nHidden=true\n[Desktop Action x]"), "{c}");
        set_autostart("demo.desktop", true).unwrap();
        let c = std::fs::read_to_string(dir.join(".config/autostart/demo.desktop")).unwrap();
        assert!(c.contains("Hidden=false") && !c.contains("Hidden=true"), "{c}");
        if let Some(h) = old {
            unsafe { std::env::set_var("HOME", h) };
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hints() {
        assert_eq!(lookup("motd-news.timer").0, Hint::Disable);
        assert_eq!(lookup("gdm3.service").0, Hint::Keep);
        assert_eq!(lookup("something-else.service").0, Hint::Unknown);
    }
}
