use crate::error::Result;
use crate::types::Conversation;
use std::io::Write;

pub struct ConversationWriter;

impl ConversationWriter {
    /// Writes `conv` as Claude Code session-file JSONL: preamble
    /// lines, then entries, one JSON value per line, each line
    /// newline-terminated.
    pub fn write_conversation<W: Write>(conv: &Conversation, mut w: W) -> Result<()> {
        // Trailing newline matters: Claude Code appends to this file on resume,
        // and without it the first appended entry lands on the last line.
        for raw in &conv.preamble {
            serde_json::to_writer(&mut w, raw)?;
            w.write_all(b"\n")?;
        }
        for entry in &conv.entries {
            serde_json::to_writer(&mut w, entry)?;
            w.write_all(b"\n")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::ConversationReader;
    use crate::types::ConversationEntry;
    use serde_json::Value;

    fn conversation() -> Conversation {
        let mut convo = Conversation::new("test-session".to_string());
        convo
            .preamble
            .push(serde_json::json!({"type": "summary", "summary": "s"}));
        let entries = [
            r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello"}}"#,
            r#"{"uuid":"uuid-2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":"Hi"}}"#,
        ];
        for entry_json in entries {
            let entry: ConversationEntry = serde_json::from_str(entry_json).unwrap();
            convo.add_entry(entry);
        }
        convo
    }

    #[test]
    fn one_record_per_line_newline_terminated() {
        let convo = conversation();

        let mut buf = Vec::new();
        ConversationWriter::write_conversation(&convo, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();

        assert!(out.ends_with('\n'));
        assert!(!out.ends_with("\n\n"));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("\"summary\""));
        for line in &lines {
            serde_json::from_str::<Value>(line).unwrap();
        }
    }

    #[test]
    fn round_trips_through_the_reader() {
        let convo = conversation();
        let file = tempfile::NamedTempFile::new().unwrap();
        ConversationWriter::write_conversation(&convo, file.as_file()).unwrap();

        let back = ConversationReader::read_conversation(file.path()).unwrap();
        let entries = |c: &Conversation| serde_json::to_value(&c.entries).unwrap();
        assert_eq!(entries(&back), entries(&convo));
        assert_eq!(back.preamble, convo.preamble);
    }
}
