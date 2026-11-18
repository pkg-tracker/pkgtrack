//! Live network connections per process, read from /proc (no root needed for
//! your own processes; sockets of system services show up without an owner).
use crate::telemetry::Level;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone)]
pub struct Conn {
    pub proto: &'static str,
    pub remote: IpAddr,
    pub remote_port: u16,
    pub state: &'static str,
    pub host: Option<String>,
    pub flag: Option<(String, Level)>,
}

#[derive(Debug, Clone)]
pub struct ProcConns {
    pub name: String,
    pub exe: String,
    pub package: Option<String>,
    pub conns: Vec<Conn>,
}

#[derive(Debug, Clone)]
pub struct Listener {
    pub proto: &'static str,
    pub addr: IpAddr,
    pub port: u16,
    pub process: String,
    pub public: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub procs: Vec<ProcConns>,
    pub listeners: Vec<Listener>,
}

struct Sock {
    proto: &'static str,
    local: (IpAddr, u16),
    remote: (IpAddr, u16),
    state: &'static str,
    inode: u64,
}

fn parse_addr(s: &str, v6: bool) -> Option<(IpAddr, u16)> {
    let (a, p) = s.split_once(':')?;
    let port = u16::from_str_radix(p, 16).ok()?;
    let ip = if v6 {
        if a.len() != 32 {
            return None;
        }
        let mut bytes = [0u8; 16];
        for i in 0..4 {
            let word = u32::from_str_radix(&a[i * 8..i * 8 + 8], 16).ok()?;
            bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let v6 = Ipv6Addr::from(bytes);
        match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        }
    } else {
        IpAddr::V4(Ipv4Addr::from(u32::from_str_radix(a, 16).ok()?.to_le_bytes()))
    };
    Some((ip, port))
}

fn tcp_state(code: &str) -> &'static str {
    match code {
        "01" => "established",
        "02" => "connecting",
        "03" => "connecting",
        "04" | "05" | "07" | "09" | "0B" => "closing",
        "06" => "time-wait",
        "08" => "close-wait",
        "0A" => "listen",
        _ => "other",
    }
}

fn read_sockets() -> Vec<Sock> {
    let mut v = Vec::new();
    for (file, proto, v6) in [
        ("/proc/net/tcp", "tcp", false),
        ("/proc/net/tcp6", "tcp", true),
        ("/proc/net/udp", "udp", false),
        ("/proc/net/udp6", "udp", true),
    ] {
        let Ok(content) = std::fs::read_to_string(file) else { continue };
        for line in content.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 {
                continue;
            }
            let (Some(local), Some(remote)) = (parse_addr(f[1], v6), parse_addr(f[2], v6)) else { continue };
            let state = if proto == "udp" {
                if remote.0.is_unspecified() { "listen" } else { "established" }
            } else {
                tcp_state(f[3])
            };
            v.push(Sock { proto, local, remote, state, inode: f[9].parse().unwrap_or(0) });
        }
    }
    v
}

struct ProcInfo {
    pid: u32,
    name: String,
    exe: String,
    cgroup: String,
}

/// socket inode -> owning process (only processes we may inspect).
fn socket_owners() -> HashMap<u64, std::rc::Rc<ProcInfo>> {
    let mut m = HashMap::new();
    let Ok(rd) = std::fs::read_dir("/proc") else { return m };
    for e in rd.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
        let mut info: Option<std::rc::Rc<ProcInfo>> = None;
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else { continue };
            let t = target.to_string_lossy();
            let Some(inode) = t.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']')).and_then(|s| s.parse().ok())
            else {
                continue;
            };
            let info = info.get_or_insert_with(|| {
                let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
                    .map(|p| p.to_string_lossy().trim_end_matches(" (deleted)").to_string())
                    .unwrap_or_default();
                let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default().trim().to_string();
                let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
                std::rc::Rc::new(ProcInfo { pid, name: comm, exe, cgroup })
            });
            m.insert(inode, info.clone());
        }
    }
    m
}

fn package_of(p: &ProcInfo) -> Option<String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    if let Some(rest) = p.exe.strip_prefix("/snap/") {
        return rest.split('/').next().map(|n| format!("{n} (snap)"));
    }
    if let Some(i) = p.cgroup.find("app-flatpak-") {
        let rest = &p.cgroup[i + "app-flatpak-".len()..];
        if let Some(id) = rest.rsplit_once('-').map(|x| x.0) {
            return Some(format!("{id} (flatpak)"));
        }
    }
    if p.exe.is_empty() {
        return None;
    }
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().unwrap().get(&p.exe) {
        return v.clone();
    }
    let owner = crate::backends::owner_of(&p.exe);
    cache.lock().unwrap().insert(p.exe.clone(), owner.clone());
    owner
}

// ---- reverse DNS (background, cached)

fn dns_cache() -> &'static Mutex<HashMap<IpAddr, Option<String>>> {
    static C: OnceLock<Mutex<HashMap<IpAddr, Option<String>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn resolve_async(ips: Vec<IpAddr>) {
    static PENDING: OnceLock<Mutex<HashSet<IpAddr>>> = OnceLock::new();
    let pending = PENDING.get_or_init(Default::default);
    for ip in ips {
        if !pending.lock().unwrap().insert(ip) {
            continue;
        }
        std::thread::spawn(move || {
            let name = std::process::Command::new("getent")
                .args(["hosts", &ip.to_string()])
                .output()
                .ok()
                .and_then(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().nth(1).map(String::from));
            dns_cache().lock().unwrap().insert(ip, name);
        });
    }
}

/// Known tracking / telemetry endpoints and big providers.
fn classify(host: &str, port: u16) -> Option<(String, Level)> {
    let h = host.to_lowercase();
    const TRACKERS: &[(&str, &str, Level)] = &[
        ("incoming.telemetry.mozilla.org", "Mozilla telemetry", Level::High),
        ("telemetry", "Telemetry endpoint", Level::High),
        ("vortex.data.microsoft.com", "Microsoft telemetry", Level::High),
        ("events.data.microsoft.com", "Microsoft telemetry", Level::High),
        ("dc.services.visualstudio.com", "VS Code telemetry", Level::High),
        ("google-analytics", "Google Analytics", Level::High),
        ("googletagmanager", "Google Tag Manager", Level::High),
        ("doubleclick", "Google ads", Level::High),
        ("app-measurement", "Firebase analytics", Level::High),
        ("firebaselogging", "Firebase analytics", Level::High),
        ("mixpanel", "Mixpanel analytics", Level::High),
        ("amplitude.com", "Amplitude analytics", Level::High),
        ("segment.io", "Segment analytics", Level::High),
        ("segment.com", "Segment analytics", Level::High),
        ("datadoghq", "Datadog monitoring", Level::Medium),
        ("sentry.io", "Sentry crash reports", Level::Medium),
        ("bugsnag", "Bugsnag crash reports", Level::Medium),
        ("crashlytics", "Crashlytics", Level::Medium),
        ("daisy.ubuntu.com", "Ubuntu error reports", Level::Medium),
        ("metrics.ubuntu.com", "Ubuntu metrics", Level::Medium),
        ("popcon", "Package survey", Level::Medium),
        ("p3a", "Brave analytics (P3A)", Level::Low),
        ("analytics", "Analytics endpoint", Level::Medium),
        ("metrics", "Metrics endpoint", Level::Medium),
        ("tracking", "Tracking endpoint", Level::Medium),
        ("stats.", "Statistics endpoint", Level::Low),
        ("api.snapcraft.io", "Snap Store", Level::Info),
        ("flathub", "Flathub", Level::Info),
        ("1e100.net", "Google", Level::Info),
        ("googleusercontent", "Google Cloud", Level::Info),
        ("amazonaws.com", "Amazon AWS", Level::Info),
        ("cloudfront.net", "Amazon CloudFront", Level::Info),
        ("akamai", "Akamai CDN", Level::Info),
        ("fastly", "Fastly CDN", Level::Info),
        ("cloudflare", "Cloudflare", Level::Info),
        ("msedge.net", "Microsoft", Level::Info),
        ("azure", "Microsoft Azure", Level::Info),
        ("github", "GitHub", Level::Info),
        ("canonical.com", "Canonical", Level::Info),
        ("ubuntu.com", "Ubuntu", Level::Info),
    ];
    if let Some((_, label, level)) = TRACKERS.iter().find(|(pat, _, _)| h.contains(pat)) {
        return Some((label.to_string(), *level));
    }
    match port {
        53 => Some(("DNS".into(), Level::Info)),
        _ => None,
    }
}

fn is_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_loopback() || v.is_unspecified(),
        IpAddr::V6(v) => v.is_loopback() || v.is_unspecified(),
    }
}

pub fn snapshot() -> Snapshot {
    let socks = read_sockets();
    let owners = socket_owners();
    let dns = dns_cache().lock().unwrap().clone();
    let mut unresolved = Vec::new();

    let mut by_proc: HashMap<Option<u32>, ProcConns> = HashMap::new();
    let mut listeners = Vec::new();
    for s in &socks {
        let owner = owners.get(&s.inode);
        if s.state == "listen" {
            if s.proto == "udp" && s.local.1 == 0 {
                continue;
            }
            listeners.push(Listener {
                proto: s.proto,
                addr: s.local.0,
                port: s.local.1,
                process: owner.map(|o| o.name.clone()).unwrap_or_else(|| "system service".into()),
                public: !is_local(&s.local.0) || s.local.0.is_unspecified(),
            });
            continue;
        }
        if is_local(&s.remote.0) || s.state == "time-wait" {
            continue;
        }
        let host = dns.get(&s.remote.0).cloned().flatten();
        if !dns.contains_key(&s.remote.0) {
            unresolved.push(s.remote.0);
        }
        let flag = host.as_deref().and_then(|h| classify(h, s.remote.1)).or_else(|| classify("", s.remote.1));
        let entry = by_proc.entry(owner.map(|o| o.pid)).or_insert_with(|| ProcConns {
            name: owner.map(|o| o.name.clone()).unwrap_or_else(|| "System or other users".into()),
            exe: owner.map(|o| o.exe.clone()).unwrap_or_default(),
            package: owner.and_then(|o| package_of(o)),
            conns: Vec::new(),
        });
        entry.conns.push(Conn { proto: s.proto, remote: s.remote.0, remote_port: s.remote.1, state: s.state, host, flag });
    }
    unresolved.sort();
    unresolved.dedup();
    resolve_async(unresolved);

    // Merge processes of the same program (e.g. many browser processes).
    let mut merged: HashMap<(String, String), ProcConns> = HashMap::new();
    for (_, p) in by_proc {
        let key = (p.name.clone(), p.package.clone().unwrap_or_default());
        match merged.get_mut(&key) {
            Some(m) => m.conns.extend(p.conns),
            None => {
                merged.insert(key, p);
            }
        }
    }
    let mut procs: Vec<ProcConns> = merged.into_values().collect();
    for p in &mut procs {
        p.conns.sort_by(|a, b| b.flag.as_ref().map(|f| f.1).cmp(&a.flag.as_ref().map(|f| f.1)).then(a.remote.cmp(&b.remote)));
        p.conns.dedup_by(|a, b| a.remote == b.remote && a.remote_port == b.remote_port && a.proto == b.proto);
    }
    procs.sort_by(|a, b| b.conns.len().cmp(&a.conns.len()).then(a.name.cmp(&b.name)));
    listeners.sort_by_key(|l| (!l.public, l.port));
    listeners.dedup_by(|a, b| a.port == b.port && a.proto == b.proto && a.process == b.process);
    Snapshot { procs, listeners }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_net_addresses() {
        assert_eq!(parse_addr("0100007F:0035", false), Some(("127.0.0.1".parse().unwrap(), 53)));
        // ::ffff:192.168.1.10 port 443 as written in /proc/net/tcp6
        assert_eq!(parse_addr("0000000000000000FFFF00000A01A8C0:01BB", true), Some(("192.168.1.10".parse().unwrap(), 443)));
        assert_eq!(classify("incoming.telemetry.mozilla.org", 443).unwrap().1, Level::High);
        assert_eq!(classify("lb-1.github.com", 443).unwrap().1, Level::Info);
    }
}
