use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use pi_harness::AppUpdateStatus;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const RELEASE_API: &str = "https://api.github.com/repos/fahmiirsyadk/torii/releases/latest";
const CHECK_INTERVAL_MS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpdateCandidate {
    pub version: Version,
    pub url: String,
    pub asset_name: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct UpdateCache {
    checked_at_ms: u64,
    candidate: Option<UpdateCandidate>,
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

pub async fn check(force: bool) -> Result<Option<UpdateCandidate>> {
    let current = Version::parse(env!("CARGO_PKG_VERSION")).context("invalid compiled version")?;
    if !force
        && let Some(cache) = read_cache()
        && now_ms().saturating_sub(cache.checked_at_ms) < CHECK_INTERVAL_MS
    {
        return Ok(cache
            .candidate
            .filter(|candidate| candidate.version > current));
    }
    let response = client()
        .get(RELEASE_API)
        .send()
        .await
        .context("failed to check the latest Torii release")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        write_cache(None);
        return Ok(None);
    }
    let response = response
        .error_for_status()
        .context("Torii release check was rejected")?;
    let release: Release = response
        .json()
        .await
        .context("invalid Torii release response")?;
    let version_text = release.tag_name.trim_start_matches('v');
    let version = Version::parse(version_text)
        .with_context(|| format!("invalid Torii release version: {}", release.tag_name))?;
    if version <= current {
        write_cache(None);
        return Ok(None);
    }
    let asset_name = release_asset_name(&version);
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == asset_name)
        .ok_or_else(|| anyhow!("release v{version} has no asset for {}", target_name()))?;
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| anyhow!("release asset {} has no valid SHA-256 digest", asset.name))?
        .to_ascii_lowercase();
    let candidate = UpdateCandidate {
        version,
        url: asset.browser_download_url,
        asset_name: asset.name,
        size_bytes: asset.size,
        sha256,
    };
    write_cache(Some(candidate.clone()));
    Ok(Some(candidate))
}

pub async fn install<F>(candidate: &UpdateCandidate, mut progress: F) -> Result<()>
where
    F: FnMut(AppUpdateStatus),
{
    let root = install_root().ok_or_else(|| {
        anyhow!("this Torii executable is not running from a versioned installation")
    })?;
    let downloads = root.join("downloads");
    fs::create_dir_all(&downloads)?;
    let archive = downloads.join(&candidate.asset_name);
    let temporary_archive = downloads.join(format!(".{}.part", candidate.asset_name));
    let response = client()
        .get(&candidate.url)
        .send()
        .await
        .context("failed to download the Torii update")?
        .error_for_status()
        .context("Torii update download was rejected")?;
    let total = response.content_length().unwrap_or(candidate.size_bytes);
    let mut stream = response.bytes_stream();
    let mut file = File::create(&temporary_archive)?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("failed while downloading the Torii update")?;
        file.write_all(&chunk)?;
        hasher.update(&chunk);
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        progress(AppUpdateStatus::Downloading {
            version: candidate.version.to_string(),
            downloaded_bytes: downloaded,
            total_bytes: total,
        });
    }
    if downloaded != candidate.size_bytes {
        let _ = fs::remove_file(&temporary_archive);
        bail!(
            "release asset size mismatch: expected {}, received {}",
            candidate.size_bytes,
            downloaded
        );
    }
    file.sync_all()?;
    drop(file);
    let actual = format!("{:x}", hasher.finalize());
    if actual != candidate.sha256 {
        let _ = fs::remove_file(&temporary_archive);
        bail!(
            "SHA-256 mismatch for {}: expected {}, received {}",
            candidate.asset_name,
            candidate.sha256,
            actual
        );
    }
    crate::task::replace_file(&temporary_archive, &archive)?;

    let versions = root.join("versions");
    fs::create_dir_all(&versions)?;
    let version = candidate.version.to_string();
    let destination = versions.join(&version);
    if !destination.is_dir() {
        let temporary = versions.join(format!(".{version}.extracting"));
        if temporary.exists() {
            fs::remove_dir_all(&temporary)?;
        }
        fs::create_dir_all(&temporary)?;
        extract_archive(&archive, &temporary)?;
        validate_layout(&temporary)?;
        fs::rename(&temporary, &destination)?;
    }
    health_check(&destination).await?;
    activate(&root, &version)?;
    progress(AppUpdateStatus::Ready { version });
    Ok(())
}

pub fn candidate_status(candidate: &UpdateCandidate) -> AppUpdateStatus {
    AppUpdateStatus::Available {
        version: candidate.version.to_string(),
        size_bytes: candidate.size_bytes,
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(format!("torii/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("static HTTP client configuration must be valid")
}

fn install_root() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let bin = executable.parent()?;
    let version = bin.parent()?;
    let versions = version.parent()?;
    (versions.file_name()? == "versions").then(|| versions.parent().map(Path::to_path_buf))?
}

fn cache_path() -> Option<PathBuf> {
    std::env::var_os("TORII_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .map(|home| home.join(".torii"))
        })
        .map(|root| root.join("update-state.json"))
}

fn read_cache() -> Option<UpdateCache> {
    let bytes = fs::read(cache_path()?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_cache(candidate: Option<UpdateCandidate>) {
    let Some(path) = cache_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let temporary = parent.join(".update-state.tmp");
    let cache = UpdateCache {
        checked_at_ms: now_ms(),
        candidate,
    };
    let result = (|| -> Result<()> {
        let mut file = File::create(&temporary)?;
        serde_json::to_writer(&mut file, &cache)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        crate::task::replace_file(&temporary, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn activate(root: &Path, version: &str) -> Result<()> {
    if let Some(current) = read_pointer(root, "current") {
        write_pointer(root, "previous", &current)?;
    }
    write_pointer(root, "current", version)?;
    write_pointer(root, "pending", version)?;
    Ok(())
}

pub fn read_pointer(root: &Path, name: &str) -> Option<String> {
    let value = fs::read_to_string(root.join(name)).ok()?;
    let value = value.trim();
    Version::parse(value).ok()?;
    Some(value.into())
}

pub fn write_pointer(root: &Path, name: &str, version: &str) -> Result<()> {
    Version::parse(version).with_context(|| format!("invalid {name} version pointer"))?;
    let temporary = root.join(format!(".{name}.tmp"));
    {
        let mut file = File::create(&temporary)?;
        writeln!(file, "{version}")?;
        file.sync_all()?;
    }
    crate::task::replace_file(&temporary, &root.join(name))?;
    crate::task::sync_directory(root)?;
    Ok(())
}

async fn health_check(version_root: &Path) -> Result<()> {
    let executable = version_root.join("bin").join(executable_name("torii"));
    let status = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new(&executable)
            .arg("--package-health-check")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await
    .context("updated Torii health check timed out")?
    .with_context(|| format!("failed to start {}", executable.display()))?;
    if !status.success() {
        bail!("updated Torii failed its packaged health check");
    }
    Ok(())
}

fn validate_layout(root: &Path) -> Result<()> {
    let executable = root.join("bin").join(executable_name("torii"));
    let sidecar = root.join("libexec").join(executable_name("torii-sidecar"));
    if !executable.is_file() || !sidecar.is_file() {
        bail!(
            "release archive is missing {} or {}",
            executable.display(),
            sidecar.display()
        );
    }
    Ok(())
}

fn extract_archive(archive: &Path, destination: &Path) -> Result<()> {
    if archive
        .extension()
        .is_some_and(|extension| extension == "zip")
    {
        extract_zip(archive, destination)
    } else {
        let file = File::open(archive)?;
        let decoder = GzDecoder::new(file);
        extract_tar(decoder, destination)
    }
}

fn extract_zip(archive: &Path, destination: &Path) -> Result<()> {
    let file = File::open(archive)?;
    let mut archive = zip::ZipArchive::new(file)?;
    if archive.len() > 10_000 {
        bail!("release archive contains too many entries");
    }
    let mut extracted_bytes = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
        {
            bail!("release archive contains a symbolic link");
        }
        extracted_bytes = extracted_bytes.saturating_add(entry.size());
        if extracted_bytes > 1024 * 1024 * 1024 {
            bail!("release archive expands beyond 1 GiB");
        }
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| anyhow!("unsafe path in release archive"))?;
        let output = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&output)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = File::create(&output)?;
        io::copy(&mut entry, &mut file)?;
    }
    Ok(())
}

fn extract_tar(reader: impl Read, destination: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    let mut entries = 0_usize;
    let mut extracted_bytes = 0_u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        entries += 1;
        if entries > 10_000 {
            bail!("release archive contains too many entries");
        }
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            bail!("release archive contains a non-file entry");
        }
        extracted_bytes = extracted_bytes.saturating_add(entry.header().size()?);
        if extracted_bytes > 1024 * 1024 * 1024 {
            bail!("release archive expands beyond 1 GiB");
        }
        let relative = entry.path()?.into_owned();
        if relative.components().any(|component| {
            !matches!(
                component,
                std::path::Component::CurDir | std::path::Component::Normal(_)
            )
        }) {
            bail!("unsafe path in release archive");
        }
        entry.unpack_in(destination)?;
    }
    Ok(())
}

fn release_asset_name(version: &Version) -> String {
    format!(
        "torii-v{version}-{}.{}",
        target_name(),
        if cfg!(windows) { "zip" } else { "tar.gz" }
    )
}

fn target_name() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        _ => "unsupported",
    }
}

fn executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("torii-{label}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn release_asset_matches_the_current_target() {
        let version = Version::new(1, 2, 3);
        let name = release_asset_name(&version);
        assert!(name.starts_with("torii-v1.2.3-"));
        assert!(name.ends_with(if cfg!(windows) { ".zip" } else { ".tar.gz" }));
    }

    #[test]
    fn extracts_the_release_tar_layout_with_dot_prefixed_paths() {
        let root = temporary_directory("extract-test");
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::create_dir_all(source.join("libexec")).unwrap();
        fs::write(source.join("bin").join(executable_name("torii")), b"host").unwrap();
        fs::write(
            source
                .join("libexec")
                .join(executable_name("torii-sidecar")),
            b"sidecar",
        )
        .unwrap();
        let archive_path = root.join("release.tar.gz");
        let archive = File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(archive, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        builder.append_dir_all(".", &source).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        fs::create_dir_all(&destination).unwrap();

        extract_archive(&archive_path, &destination).unwrap();
        validate_layout(&destination).unwrap();

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn version_pointers_are_validated_and_replaced() {
        let root = temporary_directory("pointer-test");
        fs::create_dir_all(&root).unwrap();
        write_pointer(&root, "current", "1.2.3").unwrap();
        assert_eq!(read_pointer(&root, "current").as_deref(), Some("1.2.3"));
        assert!(write_pointer(&root, "current", "../escape").is_err());
        assert_eq!(read_pointer(&root, "current").as_deref(), Some("1.2.3"));
        fs::remove_dir_all(root).unwrap();
    }
}
