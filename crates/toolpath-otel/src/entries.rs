//! The entry budget: counts what decoding an input would build, before it is
//! built, so a small input cannot expand into an unbounded tree. An entry is
//! an element of an array or repeated field, or an object nested as a field
//! value; the delivery itself is free.

use crate::error::{OtelError, Result};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Entries {
    cap: u64,
    left: u64,
}

impl Entries {
    pub(crate) fn new(cap: u64) -> Self {
        Entries { cap, left: cap }
    }

    fn charge(&mut self) -> Result<()> {
        self.left = self
            .left
            .checked_sub(1)
            .ok_or(OtelError::TooManyEntries { limit: self.cap })?;
        Ok(())
    }
}

/// serde_json's default recursion limit: deeper text fails to parse, so
/// nothing past it is built.
const JSON_DEPTH: usize = 128;

/// Charges the entries `text` holds as JSON: every array element, and every
/// object or array that is an object member's value. Lenient: text that is
/// not JSON is still scanned, and the parse that follows reports it.
pub(crate) fn charge_json(text: &[u8], entries: &mut Entries) -> Result<()> {
    struct Open {
        array: bool,
        empty: bool,
    }
    let mut stack: Vec<Open> = Vec::new();
    let (mut in_string, mut escaped) = (false, false);
    for &b in text {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        let parent = stack.last_mut();
        let in_array = parent.as_ref().is_some_and(|p| p.array);
        if let Some(p) = parent.filter(|p| p.array && p.empty && b != b']') {
            p.empty = false;
            entries.charge()?;
        }
        match b {
            b'"' => in_string = true,
            b',' if in_array => entries.charge()?,
            b'{' | b'[' => {
                if stack.len() >= JSON_DEPTH {
                    return Ok(());
                }
                if !stack.is_empty() && !in_array {
                    entries.charge()?;
                }
                stack.push(Open {
                    array: b == b'[',
                    empty: true,
                });
            }
            b'}' | b']' => {
                stack.pop();
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(feature = "protobuf")]
pub(crate) use wire::charge_protobuf;

#[cfg(feature = "protobuf")]
mod wire {
    use super::Entries;
    use crate::error::OtelError;

    /// prost's decode recursion limit (`prost::RECURSION_LIMIT`).
    const DEPTH: usize = 100;

    /// The OTLP messages whose fields hold entries.
    #[derive(Clone, Copy)]
    enum Msg {
        TracesRequest,
        ResourceSpans,
        ScopeSpans,
        Span,
        SpanEvent,
        SpanLink,
        LogsRequest,
        ResourceLogs,
        ScopeLogs,
        LogRecord,
        Resource,
        Scope,
        KeyValue,
        AnyValue,
        ArrayValue,
        KvList,
        EntityRef,
    }

    enum Child {
        Msg(Msg),
        /// Charged, holds no entries (a status, an entity-ref key string).
        Leaf,
    }

    /// The entry a length-delimited field `n` of `msg` is, if any.
    fn child(msg: Msg, n: u64) -> Option<Child> {
        use Msg::*;
        Some(match (msg, n) {
            (TracesRequest, 1) => Child::Msg(ResourceSpans),
            (LogsRequest, 1) => Child::Msg(ResourceLogs),
            (ResourceSpans | ResourceLogs, 1) => Child::Msg(Resource),
            (ResourceSpans, 2) => Child::Msg(ScopeSpans),
            (ResourceLogs, 2) => Child::Msg(ScopeLogs),
            (ScopeSpans | ScopeLogs, 1) => Child::Msg(Scope),
            (ScopeSpans, 2) => Child::Msg(Span),
            (ScopeLogs, 2) => Child::Msg(LogRecord),
            (Span, 9)
            | (SpanEvent, 3)
            | (SpanLink, 4)
            | (LogRecord, 6)
            | (Resource, 1)
            | (Scope, 3)
            | (KvList, 1) => Child::Msg(KeyValue),
            (Span, 11) => Child::Msg(SpanEvent),
            (Span, 13) => Child::Msg(SpanLink),
            (Resource, 3) => Child::Msg(EntityRef),
            (LogRecord, 5) | (KeyValue, 2) | (ArrayValue, 1) => Child::Msg(AnyValue),
            (AnyValue, 5) => Child::Msg(ArrayValue),
            (AnyValue, 6) => Child::Msg(KvList),
            (Span, 15) | (EntityRef, 3 | 4) => Child::Leaf,
            _ => return None,
        })
    }

    /// Why a walk ended early.
    enum Stop {
        /// prost fails here too, so nothing past it is built.
        Malformed,
        Over(OtelError),
    }

    /// Charges the entries prost would build decoding `bytes` as either
    /// request (the larger count, since the signal is chosen after decoding).
    pub(crate) fn charge_protobuf(bytes: &[u8], entries: &mut Entries) -> crate::error::Result<()> {
        let mut most = *entries;
        for request in [Msg::TracesRequest, Msg::LogsRequest] {
            let mut trial = *entries;
            match walk(bytes, request, 0, &mut trial) {
                Ok(()) | Err(Stop::Malformed) => {}
                Err(Stop::Over(e)) => return Err(e),
            }
            if trial.left < most.left {
                most = trial;
            }
        }
        *entries = most;
        Ok(())
    }

    fn walk(mut bytes: &[u8], msg: Msg, depth: usize, entries: &mut Entries) -> Result<(), Stop> {
        if depth >= DEPTH {
            return Err(Stop::Malformed);
        }
        while !bytes.is_empty() {
            let key = varint(&mut bytes)?;
            let (field, wire_type) = (key >> 3, key & 7);
            if field == 0 {
                return Err(Stop::Malformed);
            }
            match (child(msg, field), wire_type) {
                (Some(child), 2) => {
                    let body = len_delimited(&mut bytes)?;
                    entries.charge().map_err(Stop::Over)?;
                    if let Child::Msg(m) = child {
                        walk(body, m, depth + 1, entries)?;
                    }
                }
                (Some(_), _) => return Err(Stop::Malformed),
                (None, _) => skip(&mut bytes, field, wire_type, depth)?,
            }
        }
        Ok(())
    }

    fn varint(bytes: &mut &[u8]) -> Result<u64, Stop> {
        let mut v = 0u64;
        for i in 0..10 {
            let (&b, rest) = bytes.split_first().ok_or(Stop::Malformed)?;
            *bytes = rest;
            v |= u64::from(b & 0x7f) << (7 * i);
            if b < 0x80 {
                return Ok(v);
            }
        }
        Err(Stop::Malformed)
    }

    fn take<'a>(bytes: &mut &'a [u8], n: u64) -> Result<&'a [u8], Stop> {
        let n = usize::try_from(n).map_err(|_| Stop::Malformed)?;
        let (head, rest) = bytes.split_at_checked(n).ok_or(Stop::Malformed)?;
        *bytes = rest;
        Ok(head)
    }

    fn len_delimited<'a>(bytes: &mut &'a [u8]) -> Result<&'a [u8], Stop> {
        let n = varint(bytes)?;
        take(bytes, n)
    }

    /// Skips one field prost does not keep, groups included.
    fn skip(bytes: &mut &[u8], field: u64, wire_type: u64, depth: usize) -> Result<(), Stop> {
        match wire_type {
            0 => varint(bytes).map(drop),
            1 => take(bytes, 8).map(drop),
            2 => len_delimited(bytes).map(drop),
            5 => take(bytes, 4).map(drop),
            3 => {
                if depth >= DEPTH {
                    return Err(Stop::Malformed);
                }
                loop {
                    let key = varint(bytes)?;
                    let (inner, inner_type) = (key >> 3, key & 7);
                    if inner_type == 4 {
                        return if inner == field {
                            Ok(())
                        } else {
                            Err(Stop::Malformed)
                        };
                    }
                    skip(bytes, inner, inner_type, depth + 1)?;
                }
            }
            _ => Err(Stop::Malformed),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn count(bytes: &[u8]) -> u64 {
            let mut e = Entries::new(u64::MAX);
            charge_protobuf(bytes, &mut e).unwrap();
            u64::MAX - e.left
        }

        #[test]
        fn counts_resources_scopes_spans_and_attributes() {
            // resourceSpans[{resource{attributes[{}]}, scopeSpans[{spans[{}, {}], scope{}}]}]
            let bytes = [
                0x0a, 0x0c, 0x0a, 0x02, 0x0a, 0x00, 0x12, 0x06, 0x12, 0x00, 0x12, 0x00, 0x0a, 0x00,
            ];
            assert_eq!(count(&bytes), 7);
        }

        #[test]
        fn unknown_fields_and_groups_are_skipped_not_counted() {
            // field 5 varint, field 6 group holding a varint, then one resource entry.
            assert_eq!(count(&[0x28, 0x01, 0x33, 0x08, 0x01, 0x34, 0x0a, 0x00]), 1);
        }

        #[test]
        fn a_walk_stops_where_prost_would_fail() {
            // One resource entry, then a key-field with the wrong wire type.
            assert_eq!(count(&[0x0a, 0x00, 0x08, 0x01, 0x0a, 0x00]), 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(text: &str) -> u64 {
        let mut e = Entries::new(u64::MAX);
        charge_json(text.as_bytes(), &mut e).unwrap();
        u64::MAX - e.left
    }

    #[test]
    fn json_charges_array_elements_and_nested_objects() {
        assert_eq!(count(r#"{"resourceSpans":[]}"#), 1);
        assert_eq!(count(r#"{"resourceSpans":[{},{},{}]}"#), 4);
        assert_eq!(
            count(r#"{"a":[{"key":"k","value":{"stringValue":"[{,}]"}}]}"#),
            3
        );
        assert_eq!(count(r#"[1, 2, [3]]"#), 4);
        assert_eq!(count("{\"x\":1}\n{\"y\":[1]}\n"), 2);
        assert_eq!(count(r#"{"s":"\"[1,2]"}"#), 0);
    }

    #[test]
    fn going_over_names_the_cap() {
        let mut e = Entries::new(2);
        let err = charge_json(b"[1,2,3]", &mut e).unwrap_err();
        assert!(
            matches!(err, OtelError::TooManyEntries { limit: 2 }),
            "{err:?}"
        );
    }
}
