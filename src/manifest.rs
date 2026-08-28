//! Build the static-assets manifest for a directory.
//!
//! Cloudflare's asset hash is: hex(sha256(base64(file_content) + extension))[..32]
//! where `extension` is the file extension without the leading dot.

use anyhow::{bail, Context, Result};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub const MAX_FILES: usize = 1_000; // temp account limit
pub const MAX_FILE_SIZE: u64 = 5 * 1024 * 1024; // 5 MiB per asset on temp accounts

#[derive(Debug, Clone)]
pub struct AssetEntry {
    /// Manifest key, e.g. "/index.html"
    pub manifest_path: String,
    /// Absolute path on disk
    pub disk_path: PathBuf,
    pub hash: String,
    pub size: u64,
    /// File content, base64-encoded (kept for upload)
    pub content_b64: String,
}

/// Compute Cloudflare's asset hash for a file's content and extension.
pub fn asset_hash(content_b64: &str, extension: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content_b64.as_bytes());
    hasher.update(extension.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..32].to_string()
}

/// Walk `dir` and produce the manifest entries, sorted by path.
pub fn build_manifest(dir: &Path) -> Result<Vec<AssetEntry>> {
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }

    let mut entries: BTreeMap<String, AssetEntry> = BTreeMap::new();

    for item in WalkDir::new(dir).follow_links(false) {
        let item = item.context("walking directory")?;
        if !item.file_type().is_file() {
            continue;
        }
        // Skip hidden files/dirs (.git, .DS_Store, ...)
        let rel = item
            .path()
            .strip_prefix(dir)
            .context("computing relative path")?;
        if rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }

        let content = std::fs::read(item.path())
            .with_context(|| format!("reading {}", item.path().display()))?;
        let size = content.len() as u64;
        if size > MAX_FILE_SIZE {
            bail!(
                "{} is {} bytes; temporary accounts cap each asset at 5 MiB",
                item.path().display(),
                size
            );
        }

        let content_b64 = base64::engine::general_purpose::STANDARD.encode(&content);
        let extension = item
            .path()
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        let hash = asset_hash(&content_b64, &extension);

        let manifest_path = format!("/{}", rel.to_string_lossy().replace('\\', "/"));
        entries.insert(
            manifest_path.clone(),
            AssetEntry {
                manifest_path,
                disk_path: item.path().to_path_buf(),
                hash,
                size,
                content_b64,
            },
        );
    }

    if entries.is_empty() {
        bail!("no deployable files found in {}", dir.display());
    }
    if entries.len() > MAX_FILES {
        bail!(
            "{} files found; temporary accounts cap deployments at {} files",
            entries.len(),
            MAX_FILES
        );
    }

    Ok(entries.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cfdrop-manifest-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn hash_matches_reference_scheme() {
        // Reference: hex(sha256(base64("hello") + "html"))[..32]
        // base64("hello") = "aGVsbG8="
        let h = asset_hash("aGVsbG8=", "html");
        // Independently computed digest of the ASCII string "aGVsbG8=html"
        let mut hasher = Sha256::new();
        hasher.update(b"aGVsbG8=html");
        let expect: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(h, expect[..32]);
        assert_eq!(h.len(), 32);
    }

    #[test]
    fn walks_nested_dirs_and_skips_hidden() {
        let dir = tmpdir("walk");
        fs::write(dir.join("index.html"), "<h1>hi</h1>").unwrap();
        fs::create_dir_all(dir.join("css")).unwrap();
        fs::write(dir.join("css/site.css"), "body{}").unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".git/config"), "x").unwrap();
        fs::write(dir.join(".DS_Store"), "x").unwrap();

        let entries = build_manifest(&dir).unwrap();
        let paths: Vec<&str> = entries.iter().map(|e| e.manifest_path.as_str()).collect();
        assert_eq!(paths, vec!["/css/site.css", "/index.html"]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_empty_dir() {
        let dir = tmpdir("empty");
        assert!(build_manifest(&dir).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn no_extension_file_hashes_without_extension() {
        let dir = tmpdir("noext");
        fs::write(dir.join("LICENSE"), "MIT").unwrap();
        let entries = build_manifest(&dir).unwrap();
        assert_eq!(entries.len(), 1);
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"MIT");
        assert_eq!(entries[0].hash, asset_hash(&b64, ""));
        let _ = fs::remove_dir_all(dir);
    }
}
