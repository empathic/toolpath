//! Read and parse Gemini CLI chat files and project logs.

use crate::error::{ConvoError, Result};
use crate::types::{ChatFile, LogEntry};
use serde_json::{Map, Value};
use std::fs;
use std::path::Path;

pub struct ConversationReader;

impl ConversationReader {
    /// Parse a chat file in either shape Gemini CLI writes: one JSON
    /// document (before 0.47, and what `p export gemini -o` writes) or the
    /// 0.47 JSONL record stream (see [`Self::parse_chat`]).
    pub fn read_chat_file<P: AsRef<Path>>(path: P) -> Result<ChatFile> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(ConvoError::ConversationNotFound(path.display().to_string()));
        }
        let bytes = fs::read(path)?;
        Self::parse_chat(&bytes)
            .map_err(|e| ConvoError::Other(anyhow::anyhow!("{}: {}", path.display(), e)))
    }

    /// Parse chat-file bytes. A single JSON document is read as the
    /// `ChatFile` it is. Anything else is read as the 0.47 record stream,
    /// replayed the way gemini-cli's own `loadConversationRecord` does:
    ///
    /// - a metadata record (`sessionId` and `projectHash` strings) merges
    ///   into the header, and any `messages` it carries are upserted;
    /// - a message record (a string `id`) is upserted by id: a later record
    ///   for the same id replaces the earlier one in its original position
    ///   (0.47 re-appends a message when its tool calls complete);
    /// - `{"$set": {...}}` merges into the header; a `messages` array in it
    ///   replaces every message so far;
    /// - `{"$rewindTo": id}` drops that message and everything after it
    ///   (every message when the id is unknown).
    ///
    /// A line that is not JSON is skipped, as gemini-cli skips it (a write
    /// cut short). A message record the `GeminiMessage` type refuses is an
    /// error, not a dropped line. A stream that never names `sessionId` and
    /// `projectHash` is an error.
    pub fn parse_chat(bytes: &[u8]) -> std::result::Result<ChatFile, String> {
        if let Ok(chat) = serde_json::from_slice::<ChatFile>(bytes) {
            return Ok(chat);
        }
        let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
        let mut header: Map<String, Value> = Map::new();
        let mut messages = Messages::default();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(Value::Object(record)) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(Value::String(id)) = record.get("$rewindTo") {
                messages.rewind_to(id);
            } else if matches!(record.get("id"), Some(Value::String(_))) {
                messages.upsert(Value::Object(record));
            } else if let Some(Value::Object(set)) = record.get("$set") {
                let mut set = set.clone();
                if let Some(Value::Array(list)) = set.remove("messages") {
                    messages = Messages::default();
                    list.into_iter().for_each(|m| messages.upsert(m));
                }
                header.extend(set);
            } else if matches!(record.get("sessionId"), Some(Value::String(_)))
                && matches!(record.get("projectHash"), Some(Value::String(_)))
            {
                let mut record = record;
                if let Some(Value::Array(list)) = record.remove("messages") {
                    list.into_iter().for_each(|m| messages.upsert(m));
                }
                header.extend(record);
            }
        }
        if !matches!(header.get("sessionId"), Some(Value::String(_)))
            || !matches!(header.get("projectHash"), Some(Value::String(_)))
        {
            return Err(
                "not a chat file: no JSON document, and no record names sessionId and projectHash"
                    .into(),
            );
        }
        header.insert("messages".into(), Value::Array(messages.into_values()));
        serde_json::from_value(Value::Object(header)).map_err(|e| e.to_string())
    }

    /// Return the byte-length of a chat file on disk.
    pub fn file_size<P: AsRef<Path>>(path: P) -> Result<u64> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(ConvoError::ConversationNotFound(path.display().to_string()));
        }
        Ok(fs::metadata(path)?.len())
    }

    /// Parse `logs.json` — a JSON array of lightweight log entries.
    /// Returns an empty vec when the file is absent or malformed (the log
    /// is auxiliary; callers should not fail the whole operation when it's
    /// missing).
    pub fn read_logs<P: AsRef<Path>>(path: P) -> Result<Vec<LogEntry>> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(path)?;
        match serde_json::from_slice::<Vec<LogEntry>>(&bytes) {
            Ok(v) => Ok(v),
            Err(e) => {
                eprintln!(
                    "Warning: Failed to parse Gemini log file {}: {}",
                    path.display(),
                    e
                );
                Ok(Vec::new())
            }
        }
    }
}

/// Messages by id in first-seen order, as a JS `Map` keeps them.
#[derive(Default)]
struct Messages(Vec<(String, Value)>);

impl Messages {
    fn upsert(&mut self, message: Value) {
        let Some(id) = message.get("id").and_then(Value::as_str).map(str::to_owned) else {
            return;
        };
        match self.0.iter_mut().find(|(k, _)| *k == id) {
            Some((_, slot)) => *slot = message,
            None => self.0.push((id, message)),
        }
    }

    fn rewind_to(&mut self, id: &str) {
        let at = self.0.iter().position(|(k, _)| k == id).unwrap_or(0);
        self.0.truncate(at);
    }

    fn into_values(self) -> Vec<Value> {
        self.0.into_iter().map(|(_, v)| v).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_chat(body: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn test_read_chat_file() {
        let f = write_chat(
            r#"{"sessionId":"s","projectHash":"h","messages":[{"id":"m","timestamp":"t","type":"user","content":"hi"}]}"#,
        );
        let chat = ConversationReader::read_chat_file(f.path()).unwrap();
        assert_eq!(chat.session_id, "s");
        assert_eq!(chat.messages.len(), 1);
    }

    const STREAM: &str = concat!(
        r#"{"sessionId":"s","projectHash":"h","startTime":"2026-05-07T18:28:01Z","lastUpdated":"2026-05-07T18:28:01Z","kind":"main"}"#,
        "\n",
        r#"{"id":"u1","timestamp":"t","type":"user","content":"hi"}"#,
        "\n",
        r#"{"$set":{"lastUpdated":"2026-05-07T18:28:02Z"}}"#,
        "\n",
        r#"{"id":"g1","timestamp":"t","type":"gemini","content":"looking"}"#,
        "\n",
        r#"{"id":"g1","timestamp":"t","type":"gemini","content":"looking","toolCalls":[{"id":"c","name":"ls","args":{},"status":"success","timestamp":"t"}]}"#,
        "\n",
        "{\"id\":\"cut",
        "\n",
        r#"{"$set":{"lastUpdated":"2026-05-07T18:28:09Z"}}"#,
        "\n"
    );

    #[test]
    fn test_read_jsonl_stream_upserts_and_applies_set() {
        let f = write_chat(STREAM);
        let chat = ConversationReader::read_chat_file(f.path()).unwrap();
        assert_eq!(chat.session_id, "s");
        let ids: Vec<&str> = chat.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["u1", "g1"], "a re-appended message replaces itself");
        assert_eq!(
            chat.messages[1].tool_calls().len(),
            1,
            "the later record wins"
        );
        assert_eq!(
            chat.last_updated.unwrap().to_rfc3339(),
            "2026-05-07T18:28:09+00:00",
            "$set merges into the header"
        );
    }

    #[test]
    fn test_read_jsonl_rewind_and_set_messages() {
        let rewound = format!("{STREAM}{}\n", r#"{"$rewindTo":"g1"}"#);
        let chat = ConversationReader::parse_chat(rewound.as_bytes()).unwrap();
        assert_eq!(chat.messages.len(), 1);
        let replaced = format!(
            "{STREAM}{}\n",
            r#"{"$set":{"messages":[{"id":"x","timestamp":"t","type":"user","content":"only"}]}}"#
        );
        let chat = ConversationReader::parse_chat(replaced.as_bytes()).unwrap();
        let ids: Vec<&str> = chat.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["x"]);
    }

    #[test]
    fn test_read_jsonl_without_identity_is_an_error() {
        let err =
            ConversationReader::parse_chat(b"{\"id\":\"m\",\"type\":\"user\"}\n{\"$set\":{}}\n")
                .unwrap_err();
        assert!(err.contains("sessionId"), "{err}");
    }

    #[test]
    fn test_read_chat_file_nonexistent() {
        let err = ConversationReader::read_chat_file("/nonexistent.json").unwrap_err();
        matches!(err, ConvoError::ConversationNotFound(_));
    }

    #[test]
    fn test_read_chat_file_invalid_json() {
        let f = write_chat("not json");
        let err = ConversationReader::read_chat_file(f.path()).unwrap_err();
        matches!(err, ConvoError::Other(_));
    }

    #[test]
    fn test_read_logs() {
        let f = write_chat(
            r#"[{"sessionId":"s","messageId":0,"type":"user","message":"hi","timestamp":"t"}]"#,
        );
        let logs = ConversationReader::read_logs(f.path()).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].message, "hi");
    }

    #[test]
    fn test_read_logs_absent() {
        let logs = ConversationReader::read_logs("/nonexistent.json").unwrap();
        assert!(logs.is_empty());
    }

    #[test]
    fn test_read_logs_malformed_returns_empty() {
        let f = write_chat("{");
        let logs = ConversationReader::read_logs(f.path()).unwrap();
        assert!(logs.is_empty());
    }

    #[test]
    fn test_file_size() {
        let f = write_chat(r#"{"messages":[]}"#);
        let size = ConversationReader::file_size(f.path()).unwrap();
        assert!(size > 0);
    }

    #[test]
    fn test_file_size_nonexistent() {
        let err = ConversationReader::file_size("/nonexistent.json").unwrap_err();
        matches!(err, ConvoError::ConversationNotFound(_));
    }
}
