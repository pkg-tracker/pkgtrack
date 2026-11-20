//! Records when and from which commit this binary was built, for the About window.
use std::process::Command;

fn out(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).output().ok()?;
    let s = String::from_utf8(o.stdout).ok()?.trim().to_string();
    (o.status.success() && !s.is_empty()).then_some(s)
}

fn main() {
    // Reproducible builds set SOURCE_DATE_EPOCH; otherwise use today (UTC).
    let date = match std::env::var("SOURCE_DATE_EPOCH") {
        Ok(t) => out("date", &["-u", "-d", &format!("@{t}"), "+%Y-%m-%d"]),
        Err(_) => out("date", &["-u", "+%Y-%m-%d"]),
    }
    .unwrap_or_default();
    let commit = std::env::var("GITHUB_SHA")
        .ok()
        .map(|s| s.chars().take(7).collect())
        .or_else(|| out("git", &["rev-parse", "--short=7", "HEAD"]))
        .unwrap_or_default();
    let dirty = std::env::var("GITHUB_SHA").is_err()
        && out("git", &["status", "--porcelain", "--untracked-files=no"]).is_some();
    let by = if std::env::var("GITHUB_ACTIONS").is_ok() { "GitHub Actions (official release build)" } else { "a local build" };
    println!("cargo:rustc-env=PKGTRACK_BUILD_DATE={date}");
    println!("cargo:rustc-env=PKGTRACK_COMMIT={commit}{}", if dirty { " (with uncommitted changes)" } else { "" });
    println!("cargo:rustc-env=PKGTRACK_BUILT_BY={by}");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}
