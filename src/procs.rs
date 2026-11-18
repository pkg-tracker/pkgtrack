//! Find running processes that belong to a package by scanning /proc.
use std::collections::HashSet;
use std::fs;

pub fn cmdline(pid: u32) -> String {
    fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| {
            String::from_utf8_lossy(&b)
                .split('\0')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect()
}

/// Calls `pred(exe_path, cgroup)` for every visible process.
pub fn running_matching(pred: impl Fn(&str, &str) -> bool) -> Vec<(u32, String)> {
    let mut v = Vec::new();
    let Ok(rd) = fs::read_dir("/proc") else { return v };
    for e in rd.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        let exe = fs::read_link(format!("/proc/{pid}/exe"))
            .map(|p| p.to_string_lossy().trim_end_matches(" (deleted)").to_string())
            .unwrap_or_default();
        let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
        if !exe.is_empty() && pred(&exe, &cgroup) {
            v.push((pid, cmdline(pid)));
        }
    }
    v.truncate(50);
    v
}

/// Processes whose executable is one of the package's files.
pub fn running_from_files(files: &[String]) -> Vec<(u32, String)> {
    let set: HashSet<&str> = files.iter().map(String::as_str).collect();
    if set.is_empty() {
        return Vec::new();
    }
    // /bin -> /usr/bin on merged-usr systems
    running_matching(|exe, _| {
        set.contains(exe)
            || exe.strip_prefix("/usr").is_some_and(|e| set.contains(e))
            || set.contains(format!("/usr{exe}").as_str())
    })
}
