//! `ujira self update`: replace the running binary with a GitHub release build.
//!
//! Each release ships `ujira-<target>.tar.gz` plus a `.sha256` beside it. The checksum is required,
//! not best-effort: this binary holds a JIRA token, and a swap that skipped verification would be
//! the one place an attacker on the network could put code next to it.

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

pub const REPO: &str = "wseaton/ujira";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const TARGET: &str = env!("UJIRA_TARGET");
const USER_AGENT: &str = concat!("ujira/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub html_url: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

impl Release {
    pub fn version(&self) -> &str {
        self.tag_name.strip_prefix('v').unwrap_or(&self.tag_name)
    }

    fn asset(&self, name: &str) -> Result<&Asset> {
        self.assets
            .iter()
            .find(|a| a.name == name)
            .with_context(|| {
                format!(
                    "release {} has no asset named {name} (available: {})",
                    self.tag_name,
                    self.assets
                        .iter()
                        .map(|a| a.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }
}

/// The tarball name the release workflow publishes for this build's target.
pub fn asset_name() -> String {
    format!("ujira-{TARGET}.tar.gz")
}

/// Resolve the release, then download, verify, and swap. `version` is an exact `X.Y.Z`; `None`
/// means latest. `dry_run` stops after resolving and reports what would happen.
#[tracing::instrument(level = "debug")]
pub async fn update(version: Option<&str>, dry_run: bool) -> Result<String> {
    let http = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("building the http client")?;
    let release = fetch_release(&http, version).await?;
    let target_version = release.version();
    if target_version == CURRENT_VERSION {
        return Ok(format!(
            "already on {CURRENT_VERSION} ({})",
            release.html_url
        ));
    }
    let name = asset_name();
    let tarball = release.asset(&name)?;
    let checksum = release.asset(&format!("{name}.sha256"))?;
    let exe = std::env::current_exe().context("locating the running executable")?;
    if dry_run {
        return Ok(format!(
            "would update {CURRENT_VERSION} -> {target_version}\n  from: {}\n  into: {}\n  notes: {}",
            tarball.browser_download_url,
            exe.display(),
            release.html_url
        ));
    }

    let bytes = download(&http, &tarball.browser_download_url).await?;
    let expected = parse_sha256(&String::from_utf8_lossy(
        &download(&http, &checksum.browser_download_url).await?,
    ))?;
    verify_sha256(&bytes, &expected)?;
    let binary = extract_binary(&bytes)?;
    install(&exe, &binary)?;
    verify_installed(&exe, target_version)?;
    Ok(format!(
        "updated {CURRENT_VERSION} -> {target_version} at {}\n{}",
        exe.display(),
        release.html_url
    ))
}

async fn fetch_release(http: &reqwest::Client, version: Option<&str>) -> Result<Release> {
    let url = match version {
        Some(v) => {
            let v = validate_version(v)?;
            format!("https://api.github.com/repos/{REPO}/releases/tags/v{v}")
        }
        None => format!("https://api.github.com/repos/{REPO}/releases/latest"),
    };
    tracing::debug!(%url, "resolving release");
    let resp = http
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("querying github releases")?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::NOT_FOUND {
        bail!("no such release (https://github.com/{REPO}/releases)");
    }
    if !status.is_success() {
        bail!(
            "github releases query failed ({status}): {}",
            crate::client::truncate(&text, 400)
        );
    }
    let release: Release = serde_json::from_str(&text).context("parsing the github release")?;
    tracing::debug!(tag = %release.tag_name, assets = release.assets.len(), "release resolved");
    Ok(release)
}

async fn download(http: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    tracing::debug!(%url, "downloading");
    let resp = http
        .get(url)
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("downloading {url} failed ({status})");
    }
    let bytes = resp
        .bytes()
        .await
        .with_context(|| format!("reading {url}"))?;
    tracing::debug!(%url, bytes = bytes.len(), "downloaded");
    Ok(bytes.to_vec())
}

/// An exact `X.Y.Z` or `X.Y.Z-pre` (release candidates are prereleases, which `latest` skips, so
/// they are only reachable by name). A leading `v` is tolerated and dropped.
pub fn validate_version(v: &str) -> Result<&str> {
    let v = v.strip_prefix('v').unwrap_or(v);
    let core = v.split_once('-').map_or(v, |(core, _)| core);
    let ok = core.split('.').count() == 3 && core.split('.').all(|p| p.parse::<u64>().is_ok());
    ensure!(
        ok,
        "version must be an exact X.Y.Z (or X.Y.Z-rc.N), got {v:?}"
    );
    Ok(v)
}

/// The hex digest from a `sha256sum`-style line (`<hex>  <filename>`), or a bare digest.
pub fn parse_sha256(text: &str) -> Result<String> {
    let hex = text
        .split_whitespace()
        .next()
        .context("checksum file is empty")?
        .to_ascii_lowercase();
    ensure!(
        hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "checksum file does not contain a sha256 digest: {:?}",
        crate::client::truncate(text, 80)
    );
    Ok(hex)
}

pub fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<()> {
    let actual: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    ensure!(
        actual == expected_hex,
        "checksum mismatch: release says {expected_hex}, download is {actual}"
    );
    tracing::debug!(sha256 = %actual, "checksum verified");
    Ok(())
}

/// The `ujira` entry out of a `.tar.gz`.
pub fn extract_binary(tar_gz: &[u8]) -> Result<Vec<u8>> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tar_gz));
    for entry in archive.entries().context("reading the release tarball")? {
        let mut entry = entry.context("reading a tarball entry")?;
        let path = entry.path().context("reading a tarball entry path")?;
        if path.file_name().is_some_and(|n| n == "ujira") {
            let mut out = Vec::new();
            entry
                .read_to_end(&mut out)
                .context("reading ujira out of the tarball")?;
            ensure!(!out.is_empty(), "the tarball's ujira entry is empty");
            return Ok(out);
        }
    }
    bail!("the release tarball contains no `ujira` binary")
}

/// Write the new binary beside the current one (same filesystem, so the final rename is atomic),
/// marked executable.
fn stage(exe: &Path, binary: &[u8]) -> Result<tempfile::NamedTempFile> {
    let dir = exe
        .parent()
        .context("the running executable has no parent directory")?;
    let mut staged = tempfile::Builder::new()
        .prefix(".ujira-update-")
        .tempfile_in(dir)
        .with_context(|| format!("creating a temp file in {}", dir.display()))?;
    std::io::Write::write_all(&mut staged, binary).context("writing the new binary")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(staged.path(), std::fs::Permissions::from_mode(0o755))
            .context("marking the new binary executable")?;
    }
    Ok(staged)
}

fn install(exe: &Path, binary: &[u8]) -> Result<()> {
    let staged = stage(exe, binary)?;
    tracing::debug!(staged = %staged.path().display(), "swapping executable");
    self_replace::self_replace(staged.path()).context("replacing the running executable")?;
    Ok(())
}

/// Run the freshly installed binary and make sure it reports the version we meant to install.
fn verify_installed(exe: &Path, expected_version: &str) -> Result<()> {
    let out = std::process::Command::new(exe)
        .arg("--version")
        .output()
        .with_context(|| format!("running {} --version", exe.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    ensure!(
        out.status.success() && stdout.contains(expected_version),
        "the installed binary reports {:?} instead of {expected_version}",
        stdout.trim()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (name, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            tar.append_data(&mut h, name, *data).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn asset_name_follows_the_release_workflow() {
        assert_eq!(
            asset_name(),
            format!("ujira-{}.tar.gz", env!("UJIRA_TARGET"))
        );
    }

    #[test]
    fn release_version_strips_the_v() {
        let r = Release {
            tag_name: "v0.7.0".into(),
            html_url: String::new(),
            assets: vec![],
        };
        assert_eq!(r.version(), "0.7.0");
    }

    #[test]
    fn parse_sha256_accepts_sha256sum_lines_and_bare_digests() {
        let hex = "a".repeat(64);
        assert_eq!(
            parse_sha256(&format!("{hex}  ujira-x.tar.gz\n")).unwrap(),
            hex
        );
        assert_eq!(parse_sha256(&hex.to_uppercase()).unwrap(), hex);
        assert!(parse_sha256("").is_err());
        assert!(parse_sha256("nothex").is_err());
        assert!(parse_sha256(&"a".repeat(63)).is_err());
    }

    #[test]
    fn verify_sha256_matches_known_digest() {
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        verify_sha256(b"abc", abc).unwrap();
        let err = verify_sha256(b"abd", abc).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
    }

    #[test]
    fn extract_binary_finds_ujira_in_the_tarball() {
        let archive = tar_gz(&[("README", b"x"), ("ujira", b"\x7fELF fake")]);
        assert_eq!(extract_binary(&archive).unwrap(), b"\x7fELF fake");
    }

    #[test]
    fn extract_binary_rejects_tarballs_without_ujira() {
        let archive = tar_gz(&[("other", b"x")]);
        let err = extract_binary(&archive).unwrap_err();
        assert!(err.to_string().contains("no `ujira` binary"), "{err}");
        assert!(extract_binary(b"not a tarball").is_err());
    }

    #[test]
    fn validate_version_accepts_releases_and_prereleases_only() {
        assert_eq!(validate_version("0.8.0").unwrap(), "0.8.0");
        assert_eq!(validate_version("v0.8.0").unwrap(), "0.8.0");
        assert_eq!(validate_version("0.8.0-rc.1").unwrap(), "0.8.0-rc.1");
        for bad in ["0.8", "latest", "0.8.x", "1.2.3.4", ""] {
            assert!(validate_version(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn stage_writes_an_executable_beside_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ujira");
        std::fs::File::create(&exe)
            .unwrap()
            .write_all(b"old")
            .unwrap();
        let staged = stage(&exe, b"new").unwrap();
        assert_eq!(staged.path().parent().unwrap(), dir.path());
        assert_eq!(std::fs::read(staged.path()).unwrap(), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(staged.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "mode {mode:o}");
        }
        assert_eq!(
            std::fs::read(&exe).unwrap(),
            b"old",
            "staging must not touch the target"
        );
    }
}
