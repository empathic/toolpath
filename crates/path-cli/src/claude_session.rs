//! The identity of a Claude Code session file that `path` writes for
//! another host: the content-addressed session ID, and the rules for
//! the `cwd` the session is keyed on. `p export claude` and
//! `path resume --remote` share them.

use anyhow::{Context, Result};

/// The content-addressed session ID of the document `json`: a
/// v4-shaped UUID from the first 128 bits of the SHA-256 of its
/// RFC 8785 (JCS) form. Key order and whitespace in `json` do not
/// change the ID.
pub(crate) fn generate_content_addressed_session_id(json: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    let document: serde_json::Value =
        serde_json::from_str(json).context("Failed to parse toolpath document")?;
    let canonical = serde_json_canonicalizer::to_string(&document).context("serialize document")?;
    let digest = Sha256::digest(canonical.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Ok(uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string())
}

/// Claude Code keys a session on its canonical `cwd` string: an
/// absolute Unix path on one line with no `.`, `..`, or empty
/// component. One trailing `/` is dropped. Unix rules apply whatever
/// the local host is, and the directory is not required to exist
/// here, since it may be on another machine.
pub(crate) fn parse_posix_dir(raw: &str) -> Result<String> {
    use typed_path::{Utf8Component, Utf8UnixComponent, Utf8UnixPath, Utf8UnixPathBuf};

    if raw.contains('\n') {
        anyhow::bail!("the directory must be a single line (got {raw:?})");
    }
    let path = Utf8UnixPath::new(raw);
    if !path.is_absolute() {
        anyhow::bail!("the directory must be an absolute POSIX path (got {raw:?})");
    }
    let mut normalized = Utf8UnixPathBuf::new();
    for component in path.components() {
        match component {
            Utf8UnixComponent::CurDir | Utf8UnixComponent::ParentDir => {
                anyhow::bail!("the directory must not contain `.` or `..` (got {raw:?})")
            }
            component => normalized.push(component.as_str()),
        }
    }
    let trimmed = raw
        .strip_suffix('/')
        .filter(|t| !t.is_empty())
        .unwrap_or(raw);
    if normalized.as_str() != trimmed {
        anyhow::bail!("the directory must be in normalized form, {normalized} (got {raw:?})");
    }
    Ok(normalized.into_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_posix_dir_rejects_unnormalized_paths() {
        for bad in ["relative/dir", "/a/../b", "/a/./b", "/a//b", "", "/a\nb"] {
            assert!(parse_posix_dir(bad).is_err(), "{bad:?}");
        }
        assert_eq!(parse_posix_dir("/a/b/").unwrap(), "/a/b");
        assert_eq!(parse_posix_dir("/").unwrap(), "/");
        assert_eq!(parse_posix_dir("//").unwrap(), "/");
    }

    /// A fixed document and the ID `generate_content_addressed_session_id`
    /// returns for it. `DOC_REORDERED` is the same document with other
    /// key order and whitespace.
    const DOC: &str = r#"{"a":1,"b":{"c":[1,2],"d":"x"}}"#;
    const DOC_REORDERED: &str = "{ \"b\": {\"d\": \"x\", \"c\": [1, 2]}, \"a\": 1 }";
    const DOC_CONTENT_ADDRESSED_ID: &str = "402a3ca5-2530-407e-9029-f96879adff54";

    #[test]
    fn generate_content_addressed_session_id_is_a_v4_uuid_of_the_key_sorted_document() {
        let id = generate_content_addressed_session_id(DOC).unwrap();
        assert_eq!(id, DOC_CONTENT_ADDRESSED_ID);
        assert_eq!(
            generate_content_addressed_session_id(DOC_REORDERED).unwrap(),
            DOC_CONTENT_ADDRESSED_ID
        );
        assert_ne!(
            generate_content_addressed_session_id(r#"{"a":2}"#).unwrap(),
            DOC_CONTENT_ADDRESSED_ID
        );
        let uuid = uuid::Uuid::parse_str(&id).unwrap();
        assert_eq!(uuid.get_version_num(), 4);
        assert_eq!(uuid.get_variant(), uuid::Variant::RFC4122);
        assert!(generate_content_addressed_session_id("not json").is_err());
    }
}
