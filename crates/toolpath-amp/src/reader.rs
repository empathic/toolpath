//! Parse an Amp thread export document into a [`Session`].
//!
//! The reader is tolerant by default: a message that fails to parse is
//! logged to stderr and skipped (the schema is reverse-engineered and Amp
//! versions churn fast), and unknown content-block types are preserved as
//! [`Block::Unknown`]. Pass `strict = true` to error instead — strict mode
//! also rejects unknown block types, which makes it the tripwire for schema
//! drift in tests.

use crate::error::{ConvoError, Result};
use crate::types::{Block, Message, Session, ThreadExport};
use serde_json::Value;
use std::path::Path;

pub struct ExportReader;

impl ExportReader {
    /// Parse an export document (tolerant mode).
    pub fn parse_export(json: &str) -> Result<ThreadExport> {
        Self::parse_export_with(json, false)
    }

    /// Parse an export document with explicit strictness.
    pub fn parse_export_with(json: &str, strict: bool) -> Result<ThreadExport> {
        match serde_json::from_str::<ThreadExport>(json) {
            Ok(export) => {
                if strict && let Some(pos) = first_unknown_block(&export) {
                    return Err(ConvoError::InvalidFormat(format!(
                        "unknown content-block type in message {} (strict mode)",
                        pos
                    )));
                }
                Ok(export)
            }
            Err(e) if strict => Err(ConvoError::Json(e)),
            Err(_) => Self::salvage(json),
        }
    }

    /// Read an export file into a [`Session`] (tolerant mode).
    pub fn read_file<P: AsRef<Path>>(path: P) -> Result<Session> {
        Self::read_file_with(path, false)
    }

    /// Read an export file with explicit strictness.
    pub fn read_file_with<P: AsRef<Path>>(path: P, strict: bool) -> Result<Session> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)?;
        let export = Self::parse_export_with(&content, strict)?;
        let mut session = Session::from_export(export);
        session.source_path = Some(path.to_path_buf());
        Ok(session)
    }

    /// Tolerant fallback: re-parse keeping only the messages that fit the
    /// wire structs, warning about the ones that don't.
    fn salvage(json: &str) -> Result<ThreadExport> {
        let mut val: Value = serde_json::from_str(json)?;
        let obj = val
            .as_object_mut()
            .ok_or_else(|| ConvoError::InvalidFormat("export is not a JSON object".into()))?;
        let raw_messages = match obj.remove("messages") {
            Some(Value::Array(items)) => items,
            Some(other) => {
                return Err(ConvoError::InvalidFormat(format!(
                    "messages is not an array (got {})",
                    kind_of(&other)
                )));
            }
            None => Vec::new(),
        };
        let mut envelope: ThreadExport = serde_json::from_value(val)?;
        for (idx, raw) in raw_messages.into_iter().enumerate() {
            match serde_json::from_value::<Message>(raw) {
                Ok(m) => envelope.messages.push(m),
                Err(e) => {
                    eprintln!("Warning: skipping malformed message {} : {}", idx + 1, e);
                }
            }
        }
        Ok(envelope)
    }
}

fn first_unknown_block(export: &ThreadExport) -> Option<usize> {
    export.messages.iter().enumerate().find_map(|(i, m)| {
        m.content
            .iter()
            .any(|b| matches!(b, Block::Unknown(_)))
            .then_some(i + 1)
    })
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = include_str!("../tests/fixtures/real-session.json");

    #[test]
    fn parses_real_fixture() {
        let export = ExportReader::parse_export(REAL).unwrap();
        assert_eq!(export.messages.len(), 24);
    }

    #[test]
    fn strict_parses_real_fixture_cleanly() {
        // The real capture contains only known block types.
        let export = ExportReader::parse_export_with(REAL, true).unwrap();
        assert_eq!(export.messages.len(), 24);
    }

    #[test]
    fn read_file_sets_source_path_and_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("T-test.json");
        std::fs::write(&path, REAL).unwrap();
        let session = ExportReader::read_file(&path).unwrap();
        assert_eq!(session.id, "T-019fa4db-29cf-70c9-8d9b-81524df70e52");
        assert_eq!(session.source_path.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn tolerant_skips_malformed_message() {
        // A message without `role` cannot fit the wire struct.
        let json = r#"{"id":"T-x","messages":[{"bogus":1},{"role":"user","content":[{"type":"text","text":"hi"}]}]}"#;
        let export = ExportReader::parse_export(json).unwrap();
        assert_eq!(export.messages.len(), 1);
        assert_eq!(export.messages[0].role, "user");
    }

    #[test]
    fn strict_errors_on_malformed_message() {
        let json = r#"{"id":"T-x","messages":[{"bogus":1}]}"#;
        assert!(matches!(
            ExportReader::parse_export_with(json, true).unwrap_err(),
            ConvoError::Json(_)
        ));
    }

    #[test]
    fn strict_errors_on_unknown_block_type() {
        let json =
            r#"{"id":"T-x","messages":[{"role":"assistant","content":[{"type":"hologram"}]}]}"#;
        assert!(matches!(
            ExportReader::parse_export_with(json, true).unwrap_err(),
            ConvoError::InvalidFormat(_)
        ));
        // Tolerant mode keeps it as an Unknown block instead.
        let export = ExportReader::parse_export(json).unwrap();
        assert!(matches!(export.messages[0].content[0], Block::Unknown(_)));
    }

    #[test]
    fn invalid_json_errors_in_both_modes() {
        assert!(ExportReader::parse_export("{nope",).is_err());
        assert!(ExportReader::parse_export_with("{nope", true).is_err());
    }

    #[test]
    fn non_object_export_errors() {
        assert!(matches!(
            ExportReader::parse_export("[1,2,3]").unwrap_err(),
            // serde fails the typed parse, salvage then rejects the shape
            ConvoError::InvalidFormat(_) | ConvoError::Json(_)
        ));
    }
}
