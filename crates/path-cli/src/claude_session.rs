//! The identity of a Claude Code session file that `p export claude`
//! writes: the session ID derived from the document.

use anyhow::{Context, Result};

/// The session ID derived from the document `json`: a v4-shaped UUID
/// from the first 128 bits of the SHA-256 of its RFC 8785 (JCS) form.
/// Key order and whitespace in `json` do not change the ID.
pub(crate) fn session_id_from_document_hash(json: &str) -> Result<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed document and the ID `session_id_from_document_hash`
    /// returns for it. `DOC_REORDERED` is the same document with other
    /// key order and whitespace.
    const DOC: &str = r#"{"a":1,"b":{"c":[1,2],"d":"x"}}"#;
    const DOC_REORDERED: &str = "{ \"b\": {\"d\": \"x\", \"c\": [1, 2]}, \"a\": 1 }";
    const DOC_DERIVED_ID: &str = "402a3ca5-2530-407e-9029-f96879adff54";

    #[test]
    fn session_id_from_document_hash_is_a_v4_uuid_of_the_key_sorted_document() {
        let id = session_id_from_document_hash(DOC).unwrap();
        assert_eq!(id, DOC_DERIVED_ID);
        assert_eq!(
            session_id_from_document_hash(DOC_REORDERED).unwrap(),
            DOC_DERIVED_ID
        );
        assert_ne!(
            session_id_from_document_hash(r#"{"a":2}"#).unwrap(),
            DOC_DERIVED_ID
        );
        let uuid = uuid::Uuid::parse_str(&id).unwrap();
        assert_eq!(uuid.get_version_num(), 4);
        assert_eq!(uuid.get_variant(), uuid::Variant::RFC4122);
        assert!(session_id_from_document_hash("not json").is_err());
    }
}
