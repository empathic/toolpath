//! A content-addressed cache of verified agent downloads and their unpacked executables.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

use crate::lock::{Archive, Platform};

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn at(root: impl Into<PathBuf>) -> Cache {
        Cache { root: root.into() }
    }

    /// `$XDG_CACHE_HOME/toolpath/agents`, else `$HOME/.cache/toolpath/agents`.
    pub fn default_location() -> Result<Cache> {
        let base = match std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
            Some(x) => PathBuf::from(x),
            None => PathBuf::from(std::env::var_os("HOME").ok_or_else(|| anyhow!("no HOME"))?)
                .join(".cache"),
        };
        Ok(Cache::at(base.join("toolpath/agents")))
    }

    fn blob(&self, sha: &str) -> PathBuf {
        self.root.join(sha).join("blob")
    }

    fn unpacked(&self, sha: &str) -> PathBuf {
        self.root.join(sha).join("root")
    }

    /// Keep `bytes` under their sha256, which must be `expected`; refuses otherwise. The blob has
    /// no execute bit: only [`Cache::install`] makes something runnable.
    pub fn store(&self, expected: &str, bytes: &[u8]) -> Result<PathBuf> {
        let got = sha256_hex(bytes);
        if got != expected {
            bail!("sha256 mismatch: the lock says {expected}, the download is {got}");
        }
        let path = self.blob(expected);
        std::fs::create_dir_all(path.parent().expect("has a parent"))?;
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// The cached blob for `sha`, re-hashed on every lookup: an entry that no longer matches its
    /// name is treated as absent.
    pub fn lookup(&self, sha: &str) -> Option<PathBuf> {
        let p = self.blob(sha);
        let bytes = std::fs::read(&p).ok()?;
        (sha256_hex(&bytes) == sha).then_some(p)
    }

    /// Download `platform.url` unless a valid blob is cached, verify it, keep it.
    pub fn fetch(&self, platform: &Platform) -> Result<PathBuf> {
        if let Some(p) = self.lookup(&platform.sha256) {
            return Ok(p);
        }
        // Agent binaries run to hundreds of MB (claude's is ~275 MB): reqwest's blocking default
        // of 30 s for the whole body is too short, so the limit is per download, generous.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(1800))
            .build()?;
        let bytes = client
            .get(&platform.url)
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.bytes())
            .with_context(|| format!("download {}", platform.url))?;
        self.store(&platform.sha256, &bytes)
    }

    /// The executable for a fetched platform entry, unpacked once from the verified blob.
    pub fn install(&self, platform: &Platform) -> Result<PathBuf> {
        let blob = self.lookup(&platform.sha256).ok_or_else(|| {
            anyhow!(
                "{} is not in the cache (fetch it first)",
                &platform.sha256[..12]
            )
        })?;
        let root = self.unpacked(&platform.sha256);
        let exe = match (platform.archive, &platform.member) {
            (Archive::Binary, _) => root.join("agent"),
            (Archive::TarGz, Some(m)) => root.join(m),
            (Archive::TarGz, None) => bail!("a tar-gz entry must name its member"),
        };
        if exe.is_file() {
            return Ok(exe);
        }
        let tmp = root.with_extension("part");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        match platform.archive {
            Archive::Binary => {
                std::fs::copy(&blob, tmp.join("agent"))?;
            }
            Archive::TarGz => {
                let gz = flate2::read::GzDecoder::new(std::fs::File::open(&blob)?);
                let mut ar = tar::Archive::new(gz);
                ar.set_preserve_permissions(false);
                // `unpack` refuses entries that would land outside `tmp` (`..`, absolute paths).
                ar.unpack(&tmp)?;
            }
        }
        let staged = tmp.join(exe.strip_prefix(&root).expect("under root"));
        if !staged.is_file() {
            bail!("{} has no member {}", platform.url, staged.display());
        }
        make_executable(&staged)?;
        std::fs::rename(&tmp, &root)?;
        Ok(exe)
    }
}

#[cfg(unix)]
fn make_executable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform(sha: &str, archive: Archive, member: Option<&str>) -> Platform {
        Platform {
            target: "t".into(),
            url: "https://example.invalid/x".into(),
            sha256: sha.into(),
            archive,
            member: member.map(String::from),
        }
    }

    #[test]
    fn a_blob_is_kept_only_under_its_own_hash_and_a_mismatch_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path());
        let sha = sha256_hex(b"agent bytes");
        let p = cache.store(&sha, b"agent bytes").unwrap();
        assert_eq!(cache.lookup(&sha), Some(p.clone()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0, "a blob is never executable");
        }
        let e = cache.store(&sha, b"tampered").unwrap_err().to_string();
        assert!(e.contains("sha256 mismatch"), "{e}");
    }

    #[test]
    fn a_corrupted_cache_entry_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path());
        let sha = sha256_hex(b"good");
        let p = cache.store(&sha, b"good").unwrap();
        std::fs::write(&p, b"bad").unwrap();
        assert_eq!(cache.lookup(&sha), None);
        assert!(
            cache
                .install(&platform(&sha, Archive::Binary, None))
                .is_err()
        );
    }

    #[test]
    fn install_unpacks_the_named_member_of_a_verified_tar_gz_and_makes_only_it_runnable() {
        let mut tgz = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut tgz, flate2::Compression::default());
            let mut b = tar::Builder::new(gz);
            let body = b"#!/bin/sh\necho hi\n";
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, "pkg/bin/tool", &body[..]).unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path());
        let sha = sha256_hex(&tgz);
        cache.store(&sha, &tgz).unwrap();
        let exe = cache
            .install(&platform(&sha, Archive::TarGz, Some("pkg/bin/tool")))
            .unwrap();
        assert!(exe.ends_with("pkg/bin/tool"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        let e = cache
            .install(&platform(&sha, Archive::TarGz, Some("pkg/bin/other")))
            .unwrap_err()
            .to_string();
        assert!(e.contains("has no member"), "{e}");
    }
}
