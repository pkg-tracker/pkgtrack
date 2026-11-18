//! Checks GitHub for a newer release of Package Tracker itself.
//!
//! One HTTPS request to the GitHub releases API at startup; nothing about the
//! machine is sent. Users can turn it off in the sidebar (or with
//! PKGTRACK_NO_UPDATE_CHECK=1), and can skip a specific version.
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::time::Duration;

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// "owner/repo". Official builds get it from GitHub Actions; local builds
/// fall back to the `repository` field in Cargo.toml.
pub fn repo() -> &'static str {
    option_env!("PKGTRACK_REPO")
        .or(option_env!("GITHUB_REPOSITORY"))
        .unwrap_or_else(|| env!("CARGO_PKG_REPOSITORY").trim_start_matches("https://github.com/").trim_end_matches('/'))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub url: String,
    /// First lines of the release notes.
    pub notes: String,
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"))
        .join("pkgtrack")
}

fn off_file() -> PathBuf {
    config_dir().join("no-update-check")
}

fn skip_file() -> PathBuf {
    config_dir().join("skip-version")
}

pub fn enabled() -> bool {
    std::env::var_os("PKGTRACK_NO_UPDATE_CHECK").is_none() && !off_file().exists()
}

pub fn set_enabled(on: bool) -> Result<()> {
    if on {
        match std::fs::remove_file(off_file()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    } else {
        std::fs::create_dir_all(config_dir())?;
        std::fs::write(off_file(), "")?;
        Ok(())
    }
}

pub fn skip(version: &str) -> Result<()> {
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(skip_file(), version)?;
    Ok(())
}

fn skipped() -> Option<String> {
    std::fs::read_to_string(skip_file()).ok().map(|s| s.trim().to_string())
}

/// Numeric parts of "v1.2.3" / "1.2.3-rc1" → [1, 2, 3].
fn parse(v: &str) -> Vec<u64> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let core = v.split(['-', '+']).next().unwrap_or("");
    core.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (mut a, mut b) = (parse(candidate), parse(current));
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    a > b
}

fn parse_release(json: &str) -> Result<Release> {
    let v: serde_json::Value = serde_json::from_str(json).context("unexpected reply from GitHub")?;
    let tag = v["tag_name"].as_str().context("release has no tag")?;
    let url = v["html_url"].as_str().unwrap_or_default().to_string();
    let notes: String = v["body"].as_str().unwrap_or_default().lines().take(12).collect::<Vec<_>>().join("\n");
    Ok(Release { version: tag.trim_start_matches(['v', 'V']).to_string(), url, notes: notes.trim().to_string() })
}

/// Latest release, if it is newer than this build and not skipped by the user.
pub fn check() -> Result<Option<Release>> {
    if !enabled() {
        return Ok(None);
    }
    let api = format!("https://api.github.com/repos/{}/releases/latest", repo());
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).build();
    let body = match agent
        .get(&api)
        .set("User-Agent", &format!("pkgtrack/{CURRENT}"))
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(r) => r.into_string()?,
        // No release published yet.
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(ureq::Error::Status(code, _)) => bail!("GitHub returned HTTP {code}"),
        Err(e) => return Err(e.into()),
    };
    let rel = parse_release(&body)?;
    let wanted = is_newer(&rel.version, CURRENT) && skipped().as_deref() != Some(rel.version.as_str());
    Ok(wanted.then_some(rel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("0.1.0-rc1", "0.1.0"));
    }

    #[test]
    fn parses_github_release() {
        let r = parse_release(r#"{"tag_name":"v1.2.3","html_url":"https://github.com/o/r/releases/tag/v1.2.3","body":"- Fix\r\n- New"}"#).unwrap();
        assert_eq!(r.version, "1.2.3");
        assert_eq!(r.notes, "- Fix\n- New");
        assert!(parse_release("{}").is_err());
    }

    #[test]
    fn repo_is_owner_slash_name() {
        let r = repo();
        assert_eq!(r.split('/').count(), 2, "{r}");
    }
}
