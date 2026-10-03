//! Update decisions without I/O: versions, the GitHub release, the file for this platform and
//! its checksum.

use std::fmt::{self, Write as _};

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// `1.2.3` or `v1.2.3`; pre-releases are not offered as updates.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.strip_prefix('v').unwrap_or(raw);
        let mut parts = raw.split('.').map(str::parse::<u32>);
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) => {
                Some(Self { major, minor, patch })
            }
            _ => None,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    WindowsX64,
    MacArm,
    MacX64,
    LinuxX64,
}

impl Target {
    /// The platform this binary was built for, if releases are published for it.
    #[must_use]
    pub fn current() -> Option<Self> {
        if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            Some(Self::WindowsX64)
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Some(Self::MacArm)
        } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            Some(Self::MacX64)
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")) {
            Some(Self::LinuxX64)
        } else {
            None
        }
    }

    #[must_use]
    pub const fn triple(self) -> &'static str {
        match self {
            Self::WindowsX64 => "x86_64-pc-windows-msvc",
            Self::MacArm => "aarch64-apple-darwin",
            Self::MacX64 => "x86_64-apple-darwin",
            Self::LinuxX64 => "x86_64-unknown-linux-gnu",
        }
    }

    /// The bare executable published next to the archives, for the updater.
    #[must_use]
    pub fn binary(self) -> String {
        match self {
            Self::WindowsX64 => format!("agent-hub-{}.exe", self.triple()),
            Self::MacArm | Self::MacX64 | Self::LinuxX64 => format!("agent-hub-{}", self.triple()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub assets: Vec<Asset>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReleaseError {
    #[error("ответ GitHub не похож на релиз: {0}")]
    Json(#[from] serde_json::Error),
    #[error("непонятная версия релиза «{0}»")]
    Version(String),
}

#[derive(Deserialize)]
struct ReleaseJson {
    tag_name: String,
    assets: Vec<AssetJson>,
}

#[derive(Deserialize)]
struct AssetJson {
    name: String,
    browser_download_url: String,
}

pub fn parse_release(raw: &str) -> Result<Release, ReleaseError> {
    let ReleaseJson { tag_name, assets } = serde_json::from_str(raw)?;
    let version = Version::parse(&tag_name).ok_or(ReleaseError::Version(tag_name))?;
    let assets = assets
        .into_iter()
        .map(|AssetJson { name, browser_download_url }| Asset { name, url: browser_download_url })
        .collect();
    Ok(Release { version, assets })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub version: Version,
    pub binary: String,
    pub checksum: String,
}

/// A newer release that carries both our executable and its checksum.
#[must_use]
pub fn offer(release: &Release, current: Version, target: Target) -> Option<Offer> {
    if release.version <= current {
        return None;
    }
    let name = target.binary();
    let url = |wanted: &str| {
        release.assets.iter().find(|asset| asset.name == wanted).map(|asset| asset.url.clone())
    };
    Some(Offer {
        version: release.version,
        binary: url(&name)?,
        checksum: url(&format!("{name}.sha256"))?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChecksumError {
    #[error("файл контрольной суммы пуст или испорчен")]
    Malformed,
    #[error("контрольная сумма не совпала: файл повреждён при скачивании")]
    Mismatch,
}

/// `checksum` is a `sha256sum` line: the hex digest, then the file name.
pub fn verify(binary: &[u8], checksum: &str) -> Result<(), ChecksumError> {
    const HEX_LEN: usize = 64;
    let expected = checksum
        .split_whitespace()
        .next()
        .filter(|hex| hex.len() == HEX_LEN && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or(ChecksumError::Malformed)?;
    if sha256_hex(binary).eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(ChecksumError::Mismatch)
    }
}

/// Lower-case hex SHA-256, as `sha256sum` prints it.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::new(), |mut hex, byte| {
        // Writing into a String cannot fail.
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn version(raw: &str) -> Version {
        Version::parse(raw).unwrap()
    }

    const RELEASE: &str = r#"{
        "tag_name": "v0.3.0",
        "name": "agent-hub 0.3.0",
        "assets": [
            {"name": "agent-hub-x86_64-unknown-linux-gnu", "browser_download_url": "https://example.test/linux"},
            {"name": "agent-hub-x86_64-unknown-linux-gnu.sha256", "browser_download_url": "https://example.test/linux.sha256"},
            {"name": "agent-hub-x86_64-pc-windows-msvc.exe", "browser_download_url": "https://example.test/windows"},
            {"name": "agent-hub-x86_64-pc-windows-msvc.zip", "browser_download_url": "https://example.test/windows.zip"}
        ]
    }"#;

    #[rstest]
    #[case("0.1.0", Some((0, 1, 0)))]
    #[case("v1.20.3", Some((1, 20, 3)))]
    #[case("1.2", None)]
    #[case("1.2.3.4", None)]
    #[case("1.2.x", None)]
    #[case("1.2.3-beta", None)]
    fn versions_are_parsed(#[case] raw: &str, #[case] expected: Option<(u32, u32, u32)>) {
        let parsed = Version::parse(raw).map(|v| (v.major, v.minor, v.patch));
        assert_eq!(parsed, expected);
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(version("0.10.0") > version("0.9.9"));
        assert_eq!(version("v1.2.3").to_string(), "1.2.3");
    }

    #[rstest]
    #[case(Target::WindowsX64, "agent-hub-x86_64-pc-windows-msvc.exe")]
    #[case(Target::MacArm, "agent-hub-aarch64-apple-darwin")]
    #[case(Target::MacX64, "agent-hub-x86_64-apple-darwin")]
    #[case(Target::LinuxX64, "agent-hub-x86_64-unknown-linux-gnu")]
    fn each_target_has_its_binary(#[case] target: Target, #[case] name: &str) {
        assert_eq!(target.binary(), name);
    }

    #[test]
    fn newer_release_with_our_files_is_offered() {
        let release = parse_release(RELEASE).unwrap();
        assert_eq!(
            offer(&release, version("0.2.9"), Target::LinuxX64),
            Some(Offer {
                version: version("0.3.0"),
                binary: "https://example.test/linux".to_owned(),
                checksum: "https://example.test/linux.sha256".to_owned(),
            })
        );
    }

    #[rstest]
    #[case("0.3.0")]
    #[case("0.4.0")]
    fn current_or_older_release_is_not_offered(#[case] current: &str) {
        let release = parse_release(RELEASE).unwrap();
        assert_eq!(offer(&release, version(current), Target::LinuxX64), None);
    }

    #[rstest]
    #[case(Target::WindowsX64)]
    #[case(Target::MacArm)]
    fn release_without_our_files_is_not_offered(#[case] target: Target) {
        let release = parse_release(RELEASE).unwrap();
        assert_eq!(offer(&release, version("0.1.0"), target), None);
    }

    #[rstest]
    #[case("not json")]
    #[case(r#"{"tag_name": "latest", "assets": []}"#)]
    fn malformed_release_is_an_error(#[case] raw: &str) {
        assert!(parse_release(raw).is_err());
    }

    #[test]
    fn checksum_matches_the_sha256_line() {
        // sha256("abc")
        let line = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  agent-hub";
        assert_eq!(verify(b"abc", line), Ok(()));
        assert_eq!(verify(b"abd", line), Err(ChecksumError::Mismatch));
        assert_eq!(verify(b"abc", ""), Err(ChecksumError::Malformed));
        assert_eq!(verify(b"abc", "zz  agent-hub"), Err(ChecksumError::Malformed));
    }
}
