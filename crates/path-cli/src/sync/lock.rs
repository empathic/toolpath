//! What every uploader does before it decides anything: take the
//! upload lock and check that the server has the sync API.

use super::api::{ApiFailure, SyncApi};
use crate::config::UPLOAD_LOCK_FILE_NAME;
use anyhow::{Context, Result};
use std::path::Path;

/// The exclusive lock `share` and `sync` hold across decide, send, and
/// record. The manifest lock is only ever taken inside it.
pub(crate) fn lock_uploads(config_dir: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(config_dir)
        .with_context(|| format!("create {}", config_dir.display()))?;
    let path = config_dir.join(UPLOAD_LOCK_FILE_NAME);
    let file =
        std::fs::File::create(&path).with_context(|| format!("create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    file.lock()
        .with_context(|| format!("lock {}", path.display()))?;
    Ok(file)
}

/// A server without the sync API still accepts `POST /graphs`, so no
/// uploader may reach a create without knowing. The meta route for a
/// graph that cannot exist answers `not_found` on a capable server and
/// an untyped 404 on an old one.
pub(crate) fn supports_sync(api: &dyn SyncApi, repo: &str) -> Result<(), String> {
    match api.meta(repo, &uuid::Uuid::nil().to_string()) {
        Ok(_) | Err(ApiFailure::NotFound) => Ok(()),
        Err(ApiFailure::UpgradeRequired) => Err(format!(
            "{repo} does not support sync; upgrade Pathbase before uploading to it"
        )),
        Err(other) => Err(format!("cannot check {repo} for sync support: {other}")),
    }
}
