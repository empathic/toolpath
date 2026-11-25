//! A minimal, schema-free protobuf wire-format reader.
//!
//! The `agy` (Antigravity CLI) on-disk messages (`gemini_coder.Conversation`
//! / `Trajectory` / `Step` and the `exa.cortex_pb.*` bodies they embed) are
//! not publicly specified: the field layout here comes from decoding real
//! `agy` 1.4.8 samples with `protoc --decode_raw` (see
//! `test-fixtures/antigravity/SOURCE.md`), cross-checked against the
//! embedded `FileDescriptorProto`s recovered from the 1.1.13 binary
//! (`docs/agents/formats/antigravity.md`). Field *numbers* for the
//! messages we read matched across both versions wherever we could check
//! both (`Step.user_input = 19`, `Step.planner_response = 20`,
//! `CortexStepPlannerResponse.response = 1`, `ChatToolCall.{id,name,
//! arguments_json} = {1,2,3}`, `ModelUsageStats.{model,input_tokens,
//! output_tokens,message_id} = {1,2,3,7}`); one drifted
//! (`CortexStepUserInput`'s populated text field is 2, not 1 — see
//! [`user_input_text`]).
//!
//! Rather than generate a full `prost` client for a schema we only
//! partially know (and that upstream does not publish for the on-disk
//! format — the Apache-2.0 SDK protos are a different, SDK-level wire),
//! we read the wire format directly: walk tags, keep the few fields we
//! need, skip the rest. This degrades gracefully as `agy` adds fields
//! (unknown tags are just skipped) and never panics on a field it
//! doesn't recognize.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireType {
    Varint,
    Fixed64,
    LengthDelimited,
    Fixed32,
}

impl WireType {
    fn from_tag(tag: u64) -> Option<Self> {
        match tag & 0x7 {
            0 => Some(Self::Varint),
            1 => Some(Self::Fixed64),
            2 => Some(Self::LengthDelimited),
            5 => Some(Self::Fixed32),
            _ => None, // groups (3/4) are legacy and unused by this schema
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

impl<'a> Value<'a> {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Varint(v) => Some(*v),
            Value::Fixed64(v) => Some(*v),
            Value::Fixed32(v) => Some(*v as u64),
            Value::Bytes(_) => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        self.as_u64().map(|v| v as i64)
    }

    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&'a str> {
        self.as_bytes().and_then(|b| std::str::from_utf8(b).ok())
    }
}

impl fmt::Debug for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "protobuf wire-format error: {}", self.0)
    }
}

pub struct ParseError(&'static str);

fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0;
    loop {
        let byte = *buf.get(*pos)?;
        *pos += 1;
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

/// Iterate every top-level `(field_number, value)` pair in a serialized
/// message, in wire order. Malformed trailing bytes stop iteration rather
/// than panicking (closed binary, no schema — be permissive).
pub fn fields(buf: &[u8]) -> impl Iterator<Item = (u32, Value<'_>)> {
    FieldIter { buf, pos: 0 }
}

struct FieldIter<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for FieldIter<'a> {
    type Item = (u32, Value<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.buf.len() {
            return None;
        }
        let tag = read_varint(self.buf, &mut self.pos)?;
        let field_num = (tag >> 3) as u32;
        let wire_type = WireType::from_tag(tag)?;
        let value = match wire_type {
            WireType::Varint => Value::Varint(read_varint(self.buf, &mut self.pos)?),
            WireType::Fixed64 => {
                let bytes: [u8; 8] = self.buf.get(self.pos..self.pos + 8)?.try_into().ok()?;
                self.pos += 8;
                Value::Fixed64(u64::from_le_bytes(bytes))
            }
            WireType::Fixed32 => {
                let bytes: [u8; 4] = self.buf.get(self.pos..self.pos + 4)?.try_into().ok()?;
                self.pos += 4;
                Value::Fixed32(u32::from_le_bytes(bytes))
            }
            WireType::LengthDelimited => {
                let len = read_varint(self.buf, &mut self.pos)? as usize;
                let start = self.pos;
                let end = start.checked_add(len)?;
                let slice = self.buf.get(start..end)?;
                self.pos = end;
                Value::Bytes(slice)
            }
        };
        if field_num == 0 {
            // Invalid tag (field 0 never occurs on the wire); treat as
            // end-of-message rather than yielding a bogus field.
            return None;
        }
        Some((field_num, value))
    }
}

/// First occurrence of `field` at the top level of `buf`.
pub fn first<'a>(buf: &'a [u8], field: u32) -> Option<Value<'a>> {
    fields(buf).find(|(f, _)| *f == field).map(|(_, v)| v)
}

/// First occurrence of `field` as a length-delimited submessage.
pub fn first_message(buf: &[u8], field: u32) -> Option<&[u8]> {
    first(buf, field).and_then(|v| v.as_bytes())
}

/// First occurrence of `field` as a UTF-8 string.
pub fn first_str(buf: &[u8], field: u32) -> Option<&str> {
    first(buf, field).and_then(|v| v.as_str())
}

/// First occurrence of `field` as a varint.
pub fn first_u64(buf: &[u8], field: u32) -> Option<u64> {
    first(buf, field).and_then(|v| v.as_u64())
}

/// Every occurrence of `field` as a length-delimited submessage, in order.
pub fn all_messages(buf: &[u8], field: u32) -> Vec<&[u8]> {
    fields(buf)
        .filter(|(f, _)| *f == field)
        .filter_map(|(_, v)| v.as_bytes())
        .collect()
}

/// Decode a `google.protobuf.Timestamp { seconds = 1; nanos = 2; }`
/// submessage into a `chrono::DateTime<Utc>`. Returns `None` if the bytes
/// don't decode to a plausible timestamp.
pub fn timestamp(buf: &[u8]) -> Option<chrono::DateTime<chrono::Utc>> {
    let seconds = first_u64(buf, 1)? as i64;
    let nanos = first_u64(buf, 2).unwrap_or(0) as u32;
    chrono::DateTime::from_timestamp(seconds, nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-encode a tiny message to exercise the reader without any
    /// real `agy` bytes: field 1 = varint 42, field 2 = string "hi".
    fn sample() -> Vec<u8> {
        vec![
            0x08, 0x2a, // tag (1<<3|0), varint 42
            0x12, 0x02, b'h', b'i', // tag (2<<3|2), len 2, "hi"
        ]
    }

    #[test]
    fn reads_varint_and_string_fields() {
        let buf = sample();
        assert_eq!(first_u64(&buf, 1), Some(42));
        assert_eq!(first_str(&buf, 2), Some("hi"));
        assert_eq!(first_u64(&buf, 3), None);
    }

    #[test]
    fn fields_iterates_in_order() {
        let buf = sample();
        let all: Vec<_> = fields(&buf).map(|(f, _)| f).collect();
        assert_eq!(all, vec![1, 2]);
    }

    #[test]
    fn truncated_buffer_does_not_panic() {
        let mut buf = sample();
        buf.truncate(3); // cut mid-string
        let all: Vec<_> = fields(&buf).collect();
        // Reads the first field cleanly, then stops rather than panicking.
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn empty_buffer_yields_no_fields() {
        assert!(fields(&[]).next().is_none());
    }

    fn encode_varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
    }

    #[test]
    fn timestamp_decodes_seconds_and_nanos() {
        // field1 (varint, tag 0x08) = seconds, field2 (varint, tag 0x10) = nanos.
        let mut buf = Vec::new();
        buf.push(0x08);
        encode_varint(1_700_000_000, &mut buf);
        buf.push(0x10);
        encode_varint(123, &mut buf);
        let ts = timestamp(&buf).unwrap();
        assert_eq!(ts.timestamp(), 1_700_000_000);
        assert_eq!(ts.timestamp_subsec_nanos(), 123);
    }
}
