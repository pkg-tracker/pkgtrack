use std::fmt;

/// Where a package comes from. Native managers are mutually exclusive per
/// distro; Flatpak and Snap can coexist with any of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Apt,
    Rpm,
    Pacman,
    Flatpak,
    Snap,
    AppImage,
    /// Installed by hand (install script, tarball, make install).
    Manual,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Apt => "APT / dpkg",
            Source::Rpm => "RPM",
            Source::Pacman => "Pacman",
            Source::Flatpak => "Flatpak",
            Source::Snap => "Snap",
            Source::AppImage => "AppImage",
            Source::Manual => "Installed by hand",
        }
    }
    pub fn short(self) -> &'static str {
        match self {
            Source::Apt => "deb",
            Source::Rpm => "rpm",
            Source::Pacman => "pacman",
            Source::Flatpak => "flatpak",
            Source::Snap => "snap",
            Source::AppImage => "appimage",
            Source::Manual => "manual",
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    App,
    Runtime,
    Library,
}

#[derive(Debug, Clone)]
pub struct Package {
    /// Identifier passed to the package manager (name, app id, or snap name).
    pub id: String,
    /// Human-friendly display name.
    pub name: String,
    pub version: String,
    pub source: Source,
    pub summary: String,
    pub size_bytes: Option<u64>,
    pub kind: Kind,
    /// Essential / base system package: removing it may break the OS.
    pub protected: bool,
    /// Extra manager-specific data (flatpak installation, ref, ...).
    pub extra: String,
    pub new_version: Option<String>,
    pub telemetry: Option<crate::telemetry::Level>,
    /// When it was first installed (unix time), if known.
    pub installed_at: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct Details {
    pub used_by: Vec<String>,
    pub depends_on: Vec<String>,
    pub location: Vec<String>,
    pub running: Vec<(u32, String)>,
    pub info: Vec<(String, String)>,
    pub telemetry: Vec<crate::telemetry::Finding>,
    /// Result of a dry-run removal, if the backend supports it.
    pub would_remove: Option<Vec<String>>,
    /// Result of a dry-run install (packages that would be pulled in).
    pub would_install: Option<Vec<String>>,
    /// Flatpak sandbox permission switches.
    pub switches: Vec<crate::backends::flatpak::Switch>,
    /// Longer "what is this" text (AppStream or package description).
    pub description: String,
    pub homepage: Option<String>,
}

/// Birth (creation) time of a path, falling back to modification time.
pub fn birth_time(path: &str) -> Option<u64> {
    let m = std::fs::metadata(path).ok()?;
    let t = m.created().or_else(|_| m.modified()).ok()?;
    t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// "2026-11-01" for a unix time (UTC is fine for a date).
pub fn format_date(t: u64) -> String {
    let days = (t / 86_400) as i64;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Parse strings like "117.8 MB", "312MB", "12.2 kB".
pub fn parse_human_size(s: &str) -> Option<u64> {
    let s = s.trim().replace('\u{a0}', " ");
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))?;
    let (num, unit) = s.split_at(split);
    let num: f64 = num.replace(',', ".").parse().ok()?;
    let mult = match unit.trim().to_ascii_lowercase().as_str() {
        "b" | "bytes" => 1.0,
        "kb" | "kib" | "k" => 1e3,
        "mb" | "mib" | "m" => 1e6,
        "gb" | "gib" | "g" => 1e9,
        "tb" | "tib" | "t" => 1e12,
        _ => return None,
    };
    Some((num * mult) as u64)
}
