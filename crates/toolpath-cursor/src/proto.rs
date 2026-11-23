//! The protobuf wire format, as far as Cursor's stores use it: varints and
//! length-delimited fields. The IDE's `conversationState` and the
//! `cursor-agent` CLI's root blob (`agent.v1.ConversationStateStructure`)
//! are both built from these, so the two writers share one encoder.

use crate::error::{CursorError, Result};

/// Append `n` as a base-128 varint.
pub fn put_varint(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// Append field `field` as a length-delimited value (wire type 2): bytes,
/// a string or an embedded message.
pub fn put_bytes(out: &mut Vec<u8>, field: u32, data: &[u8]) {
    put_varint(out, (u64::from(field) << 3) | 2);
    put_varint(out, data.len() as u64);
    out.extend_from_slice(data);
}

/// One length-delimited field, as a new buffer.
pub fn bytes_field(field: u32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 4);
    put_bytes(&mut out, field, data);
    out
}

/// A field read back from the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

/// Every field of one message, in wire order. Fixed-width fields (wire
/// types 1 and 5) are skipped; groups (3, 4) are refused, as nothing
/// Cursor writes uses them.
pub fn fields(mut buf: &[u8]) -> Result<Vec<(u32, Value<'_>)>> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        let key = take_varint(&mut buf)?;
        let field = u32::try_from(key >> 3).map_err(|_| malformed("field number"))?;
        match key & 7 {
            0 => out.push((field, Value::Varint(take_varint(&mut buf)?))),
            1 => buf = buf.get(8..).ok_or_else(|| malformed("fixed64"))?,
            2 => {
                let len =
                    usize::try_from(take_varint(&mut buf)?).map_err(|_| malformed("length"))?;
                let (data, rest) = (
                    buf.get(..len).ok_or_else(|| malformed("length"))?,
                    &buf[len..],
                );
                out.push((field, Value::Bytes(data)));
                buf = rest;
            }
            5 => buf = buf.get(4..).ok_or_else(|| malformed("fixed32"))?,
            t => return Err(malformed(&format!("wire type {t}"))),
        }
    }
    Ok(out)
}

/// Every length-delimited value of `field`, in order.
pub fn repeated_bytes<'a>(msg: &[(u32, Value<'a>)], field: u32) -> Vec<&'a [u8]> {
    msg.iter()
        .filter_map(|(f, v)| match v {
            Value::Bytes(b) if *f == field => Some(*b),
            _ => None,
        })
        .collect()
}

/// The last length-delimited value of `field` (protobuf's rule for a
/// singular field seen twice).
pub fn last_bytes<'a>(msg: &[(u32, Value<'a>)], field: u32) -> Option<&'a [u8]> {
    repeated_bytes(msg, field).pop()
}

fn take_varint(buf: &mut &[u8]) -> Result<u64> {
    let mut n = 0u64;
    for shift in (0..64).step_by(7) {
        let (&b, rest) = buf.split_first().ok_or_else(|| malformed("varint"))?;
        *buf = rest;
        n |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(n);
        }
    }
    Err(malformed("varint"))
}

fn malformed(detail: &str) -> CursorError {
    CursorError::MalformedPayload {
        what: "protobuf".into(),
        detail: format!("truncated or invalid {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip_at_the_byte_boundaries() {
        for n in [0u64, 1, 127, 128, 300, 16_383, 16_384, u64::MAX] {
            let mut buf = Vec::new();
            put_varint(&mut buf, n);
            let mut s = buf.as_slice();
            assert_eq!(take_varint(&mut s).unwrap(), n);
            assert!(s.is_empty());
        }
        let mut buf = Vec::new();
        put_varint(&mut buf, 300);
        assert_eq!(buf, [0xac, 0x02]);
    }

    #[test]
    fn fields_read_back_what_put_bytes_wrote() {
        let mut msg = bytes_field(1, b"a");
        put_bytes(&mut msg, 8, &[0u8; 200]);
        msg.extend([0x20, 0x05]); // field 4, varint 5
        put_bytes(&mut msg, 1, b"bc");
        let f = fields(&msg).unwrap();
        assert_eq!(repeated_bytes(&f, 1), [b"a".as_slice(), b"bc".as_slice()]);
        assert_eq!(repeated_bytes(&f, 8)[0].len(), 200);
        assert!(f.contains(&(4, Value::Varint(5))));
        assert_eq!(last_bytes(&f, 1), Some(b"bc".as_slice()));
    }

    #[test]
    fn a_truncated_message_is_an_error() {
        let msg = bytes_field(1, b"hello");
        assert!(fields(&msg[..msg.len() - 1]).is_err());
        assert!(fields(&[0x80]).is_err());
    }
}
