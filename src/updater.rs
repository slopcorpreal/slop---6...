use serde::Deserialize;
use std::{env, path::Path, sync::mpsc, thread, time::Duration};

const CONNECT_TIMEOUT_SECS: u64 = 5;
const READ_TIMEOUT_SECS: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AppVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl AppVersion {
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim_start_matches('v');
        let mut parts = trimmed.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
        })
    }

    pub fn with_commit_count_minor(base_version: &str, commit_count: &str) -> Option<Self> {
        let mut base = Self::parse(base_version)?;
        base.minor = commit_count.parse().ok()?;
        Some(base)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateUrgency {
    None,
    Minor,
    Major,
}

pub fn classify_update(current: AppVersion, latest: AppVersion) -> UpdateUrgency {
    if latest.major > current.major {
        UpdateUrgency::Major
    } else if latest.major == current.major && latest.minor > current.minor {
        UpdateUrgency::Minor
    } else {
        UpdateUrgency::None
    }
}

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub latest_version: AppVersion,
    pub html_url: String,
    pub urgency: UpdateUrgency,
    pub instructions: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallKind {
    CargoInstalled,
    StandaloneBinary,
    Unknown,
}

#[derive(Debug, Deserialize)]
struct LatestRelease {
    tag_name: String,
    html_url: String,
}

fn detect_install_kind(exe_path: Option<&Path>) -> InstallKind {
    let Some(exe_path) = exe_path else {
        return InstallKind::Unknown;
    };
    let display = exe_path.to_string_lossy();
    if display.contains(".cargo/bin") {
        InstallKind::CargoInstalled
    } else {
        InstallKind::StandaloneBinary
    }
}

fn platform_hint() -> &'static str {
    match (env::consts::OS, env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x86_64",
        ("windows", "aarch64") => "windows-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        _ => "your-platform",
    }
}

fn update_instructions(kind: InstallKind, package_name: &str) -> String {
    match kind {
        InstallKind::CargoInstalled => {
            format!("Installed via cargo. Update with:\n\ncargo install --force {}", package_name)
        }
        InstallKind::StandaloneBinary => format!(
            "Standalone binary detected. Download the {platform} asset from GitHub releases and replace your current executable.",
            platform = platform_hint()
        ),
        InstallKind::Unknown => format!(
            "Could not detect installation type. For cargo installs use `cargo install --force {}`; otherwise download your platform binary from GitHub releases.",
            package_name
        ),
    }
}

fn fetch_update_info(
    owner: &str,
    repo: &str,
    current: AppVersion,
    package_name: &str,
    package_version: &str,
    exe_path: Option<&Path>,
) -> Result<UpdateInfo, String> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
    let release: LatestRelease = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .timeout_read(Duration::from_secs(READ_TIMEOUT_SECS))
        .build()
        .get(&url)
        .set("User-Agent", &format!("{package_name}/{package_version}"))
        .call()
        .map_err(|e| format!("Update check failed: {e}"))?
        .into_json()
        .map_err(|e| format!("Invalid update response: {e}"))?;

    let latest = AppVersion::parse(&release.tag_name)
        .ok_or_else(|| format!("Unsupported release tag version: {}", release.tag_name))?;
    let urgency = classify_update(current, latest);
    Ok(UpdateInfo {
        latest_version: latest,
        html_url: release.html_url,
        urgency,
        instructions: update_instructions(detect_install_kind(exe_path), package_name),
    })
}

pub fn spawn_update_check(
    owner: String,
    repo: String,
    current: AppVersion,
    package_name: String,
    package_version: String,
) -> mpsc::Receiver<Result<UpdateInfo, String>> {
    let exe_path = env::current_exe().ok();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = fetch_update_info(
            &owner,
            &repo,
            current,
            &package_name,
            &package_version,
            exe_path.as_deref(),
        );
        let _ = tx.send(result);
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_count_replaces_minor_version() {
        let version = AppVersion::with_commit_count_minor("1.7.3", "42").unwrap();
        assert_eq!(
            version,
            AppVersion {
                major: 1,
                minor: 42,
                patch: 3
            }
        );
    }

    #[test]
    fn update_classification_prioritizes_major_and_minor() {
        let current = AppVersion {
            major: 1,
            minor: 10,
            patch: 0,
        };
        assert_eq!(
            classify_update(
                current,
                AppVersion {
                    major: 2,
                    minor: 0,
                    patch: 0
                }
            ),
            UpdateUrgency::Major
        );
        assert_eq!(
            classify_update(
                current,
                AppVersion {
                    major: 1,
                    minor: 11,
                    patch: 0
                }
            ),
            UpdateUrgency::Minor
        );
        assert_eq!(
            classify_update(
                current,
                AppVersion {
                    major: 1,
                    minor: 10,
                    patch: 9
                }
            ),
            UpdateUrgency::None
        );
    }
}
