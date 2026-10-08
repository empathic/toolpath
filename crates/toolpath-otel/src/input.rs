//! Input bytes → OTLP/JSON deliveries: decompress, split Collector frames
//! and JSON lines, decode protobuf bodies, shape-check. Reads no attribute key.

use crate::entries::{Entries, charge_json};
use crate::error::{OtelError, Result};
use crate::otlp::is_otlp;
use serde_json::Value;

#[cfg(feature = "protobuf")]
use crate::protojson::decode_protobuf_within;

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
const PROTOBUF_EXTENSIONS: [&str; 3] = [".pb", ".binpb", ".protobuf"];
/// Nested compression layers (gzip or zstd, in any mix) allowed in one input.
pub(crate) const MAX_LAYERS: usize = 4;
/// [`decode_input`]'s limit on decompressed bytes across all layers and
/// frames of one input, to refuse decompression bombs. Bounds output, not
/// peak allocation.
pub(crate) const MAX_DECOMPRESSED: u64 = 1 << 30;
/// [`decode_input`]'s limit on decoded entries across all frames and
/// deliveries of one input, so a small input cannot expand into an
/// unbounded tree.
pub(crate) const MAX_ENTRIES: u64 = 1 << 20;

/// The bounds [`decode_input_with_limits`] decodes one input within.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    /// Decompressed bytes, across every compression layer and Collector
    /// frame (default 1 GiB).
    pub decompressed: u64,
    /// Entries decoded, across every frame and delivery (default
    /// 1,048,576): each element of a JSON array or protobuf repeated field,
    /// and each object or message nested as a field's value.
    pub entries: u64,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        DecodeLimits {
            decompressed: MAX_DECOMPRESSED,
            entries: MAX_ENTRIES,
        }
    }
}

impl DecodeLimits {
    /// These limits with `decompressed` bytes.
    pub fn with_decompressed(mut self, decompressed: u64) -> Self {
        self.decompressed = decompressed;
        self
    }

    /// These limits with `entries` entries.
    pub fn with_entries(mut self, entries: u64) -> Self {
        self.entries = entries;
        self
    }
}

/// Decode one input file (or request body) into OTLP/JSON request bodies,
/// the input [`derive_path`](crate::derive_path) takes: one JSON body, JSON
/// lines, an OTLP/HTTP protobuf body (feature `protobuf`), Collector
/// file-exporter frames, and gzip or zstd around any of them (feature
/// `compression`). `name` only enables file-extension rules. The numbered
/// sniffing rules cited below are in `docs/agents/formats/otel.md`,
/// "Framing and zstd".
///
/// # Errors
///
/// `NotOtlp`/`NotOtlpBody` ([`OtelError::is_not_otlp`]) when the input is
/// not OTLP at all (a file led by `0x00` included, unless its first frame
/// is an OTLP request); any other error is OTLP that is malformed, cut short,
/// more than 4 compression layers deep, or needs a feature. More than
/// 1 GiB decompressed is [`OtelError::TooLarge`], more than 1,048,576
/// entries ([`DecodeLimits::entries`]) is [`OtelError::TooManyEntries`]
/// (both [`OtelError::is_too_large`]).
pub fn decode_input(bytes: &[u8], name: Option<&str>) -> Result<Vec<Value>> {
    decode_input_with_limits(bytes, name, DecodeLimits::default())
}

/// [`decode_input`] with `limit` in place of its 1 GiB: one
/// budget of decompressed bytes shared by every compression layer and
/// Collector frame of the input. The entry cap stays the default.
///
/// # Errors
///
/// As [`decode_input`]; going over `limit` is [`OtelError::TooLarge`].
pub fn decode_input_with_limit(bytes: &[u8], name: Option<&str>, limit: u64) -> Result<Vec<Value>> {
    decode_input_with_limits(
        bytes,
        name,
        DecodeLimits::default().with_decompressed(limit),
    )
}

/// [`decode_input`] within `limits`.
///
/// # Errors
///
/// As [`decode_input`]; going over `limits.decompressed` is
/// [`OtelError::TooLarge`], over `limits.entries`
/// [`OtelError::TooManyEntries`].
pub fn decode_input_with_limits(
    bytes: &[u8],
    name: Option<&str>,
    limits: DecodeLimits,
) -> Result<Vec<Value>> {
    let cap = limits.decompressed;
    decode_layer(bytes, name, 0, cap, cap, &mut Entries::new(limits.entries))
}

/// `budget` is the decompressed output still allowed; `cap` is the whole
/// cap, which the error names.
fn decode_layer(
    bytes: &[u8],
    name: Option<&str>,
    depth: usize,
    cap: u64,
    budget: u64,
    entries: &mut Entries,
) -> Result<Vec<Value>> {
    if let Some((inner, suffix)) = decompress(bytes, depth, cap, budget)? {
        let len = inner.len() as u64;
        let inner_name = name.map(|n| n.strip_suffix(suffix).unwrap_or(n));
        return decode_layer(&inner, inner_name, depth + 1, cap, budget - len, entries);
    }
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(OtelError::NotOtlp);
    }
    if bytes[0] == 0x00 {
        return match split_frames(bytes) {
            Ok(frames) => decode_frames(frames, depth, cap, budget, entries),
            Err(framing) if first_frame_may_be_otlp(bytes, depth, cap, budget, *entries) => {
                Err(framing)
            }
            Err(framing) => Err(OtelError::NotOtlpBody(format!(
                "a leading 0x00 byte, but no Collector frame holding an OTLP request ({framing})"
            ))),
        };
    }
    match decode_text(bytes, entries) {
        Err(OtelError::Json(why)) if !looks_like_json(bytes) => match split_frames(bytes) {
            Ok(frames) => decode_frames(frames, depth, cap, budget, entries),
            Err(framing) if first_frame_is_otlp(bytes, depth, cap, budget, *entries) => {
                Err(framing)
            }
            Err(_) => decode_body(bytes, name, why, entries),
        },
        other => other,
    }
}

/// Rule 5's tie-break: a complete OTLP first frame means a Collector file
/// cut short after frame 0, so the framing error stands.
fn first_frame_is_otlp(
    bytes: &[u8],
    depth: usize,
    cap: u64,
    budget: u64,
    entries: Entries,
) -> bool {
    first_frame(bytes).is_some_and(|frame| {
        decode_first_frame(frame, depth, cap, budget, entries)
            .is_ok_and(|values| values.iter().any(carries_resources))
    })
}

/// Rule 3's tie-break: as rule 5's, but a first frame this build cannot
/// read (a feature it lacks, or over a limit) may be OTLP too.
fn first_frame_may_be_otlp(
    bytes: &[u8],
    depth: usize,
    cap: u64,
    budget: u64,
    entries: Entries,
) -> bool {
    first_frame(bytes).is_some_and(|frame| {
        match decode_first_frame(frame, depth, cap, budget, entries) {
            Ok(values) => values.iter().any(carries_resources),
            Err(OtelError::FeatureDisabled(_)) => {
                frame.starts_with(&[0x0a])
                    || frame.starts_with(&GZIP_MAGIC)
                    || frame.starts_with(&ZSTD_MAGIC)
            }
            Err(e) => e.is_too_large(),
        }
    })
}

/// The complete, non-empty first frame, if the input has one.
fn first_frame(bytes: &[u8]) -> Option<&[u8]> {
    let (head, tail) = bytes.split_first_chunk::<4>()?;
    usize::try_from(u32::from_be_bytes(*head))
        .ok()
        .and_then(|n| tail.get(..n))
        .filter(|frame| !frame.is_empty())
}

fn decode_first_frame(
    frame: &[u8],
    depth: usize,
    cap: u64,
    budget: u64,
    mut entries: Entries,
) -> Result<Vec<Value>> {
    let mut left = budget;
    decode_frame(frame, depth, cap, &mut left, &mut entries)
}

/// One compression layer: `None` without compression magic, else the
/// decompressed bytes and the file-name suffix to drop.
fn decompress(
    bytes: &[u8],
    depth: usize,
    cap: u64,
    budget: u64,
) -> Result<Option<(Vec<u8>, &'static str)>> {
    let gzip = bytes.starts_with(&GZIP_MAGIC);
    if !gzip && !bytes.starts_with(&ZSTD_MAGIC) {
        return Ok(None);
    }
    if depth >= MAX_LAYERS {
        return Err(OtelError::Decompress(format!(
            "more than {MAX_LAYERS} nested compression layers"
        )));
    }
    let (inner, suffix) = if gzip {
        (gunzip(bytes, budget)?, ".gz")
    } else {
        (unzstd(bytes, budget)?, ".zst")
    };
    if inner.len() as u64 > budget {
        return Err(OtelError::TooLarge { limit: cap });
    }
    Ok(Some((inner, suffix)))
}

/// `n` in the largest binary unit that states it exactly.
pub(crate) fn human_bytes(n: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1 << 30, "GiB"), (1 << 20, "MiB"), (1 << 10, "KiB")];
    UNITS
        .iter()
        .find(|(size, _)| n >= *size && n.is_multiple_of(*size))
        .map(|(size, unit)| format!("{} {unit}", n / size))
        .unwrap_or_else(|| format!("{n} B"))
}

/// Reads at most `limit + 1` bytes so the caller can tell an over-limit
/// layer from one exactly at it.
#[cfg(feature = "compression")]
fn gunzip(bytes: &[u8], limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(bytes)
        .take(limit.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|e| OtelError::Decompress(e.to_string()))?;
    Ok(out)
}

#[cfg(not(feature = "compression"))]
fn gunzip(_bytes: &[u8], _limit: u64) -> Result<Vec<u8>> {
    Err(OtelError::FeatureDisabled("compression"))
}

/// Every concatenated zstd frame, reading at most `limit + 1` bytes as
/// [`gunzip`] does.
#[cfg(feature = "compression")]
fn unzstd(bytes: &[u8], limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let want = limit.saturating_add(1);
    let mut src = bytes;
    let mut out = Vec::new();
    while !src.is_empty() && (out.len() as u64) < want {
        let decoder = ruzstd::decoding::StreamingDecoder::new(&mut src)
            .map_err(|e| OtelError::Decompress(format!("zstd: {e}")))?;
        decoder
            .take(want - out.len() as u64)
            .read_to_end(&mut out)
            .map_err(|e| OtelError::Decompress(format!("zstd: {e}")))?;
    }
    Ok(out)
}

#[cfg(not(feature = "compression"))]
fn unzstd(_bytes: &[u8], _limit: u64) -> Result<Vec<u8>> {
    Err(OtelError::FeatureDisabled("compression"))
}

/// Input meant to be JSON, so its JSON error is the error to report.
fn looks_like_json(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    matches!(
        bytes.iter().find(|b| !b.is_ascii_whitespace()),
        Some(b'{' | b'[')
    )
}

fn has_protobuf_extension(name: Option<&str>) -> bool {
    name.is_some_and(|n| {
        let n = n.to_ascii_lowercase();
        PROTOBUF_EXTENSIONS.iter().any(|e| n.ends_with(e))
    })
}

/// Rule 6. A body sniffed by content repeats `json_error`, since the file
/// may be broken JSON rather than protobuf.
fn decode_body(
    bytes: &[u8],
    name: Option<&str>,
    json_error: String,
    entries: &mut Entries,
) -> Result<Vec<Value>> {
    let named = has_protobuf_extension(name);
    #[cfg(feature = "protobuf")]
    {
        let value = decode_protobuf_within(bytes, entries).map_err(|e| match e {
            OtelError::NotOtlpBody(m) if !named => {
                OtelError::NotOtlpBody(format!("{m}; not JSON either ({json_error})"))
            }
            other => other,
        })?;
        if named || carries_records(&value) {
            Ok(vec![value])
        } else {
            Err(OtelError::NotOtlpBody(format!(
                "content-sniffed as protobuf, but the body carries no span or log record; \
                 not JSON either ({json_error})"
            )))
        }
    }
    #[cfg(not(feature = "protobuf"))]
    {
        let _ = (bytes, entries);
        if named {
            Err(OtelError::FeatureDisabled("protobuf"))
        } else {
            Err(OtelError::Json(json_error))
        }
    }
}

/// Arbitrary bytes often decode as a request of empty resource entries, so
/// a body read by content must carry a span or log record.
#[cfg(feature = "protobuf")]
fn carries_records(value: &Value) -> bool {
    let has = |signal: &str, scopes: &str, records: &str| {
        value
            .get(signal)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|r| r.get(scopes)?.as_array())
            .flatten()
            .any(|s| {
                s.get(records)
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty())
            })
    };
    has("resourceSpans", "scopeSpans", "spans") || has("resourceLogs", "scopeLogs", "logRecords")
}

fn carries_resources(value: &Value) -> bool {
    ["resourceSpans", "resourceLogs"].iter().any(|k| {
        value
            .get(k)
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
    })
}

/// Collector frames (4-byte big-endian length, then the bytes) that must
/// tile the input. Checked slicing: a bad length is an error, never a panic.
/// Nothing is collected: four zero bytes are a frame.
fn split_frames(bytes: &[u8]) -> Result<Frames<'_>> {
    let mut i = 0;
    let mut rest = bytes;
    while !rest.is_empty() {
        let Some((head, tail)) = rest.split_first_chunk::<4>() else {
            return Err(OtelError::Framing(format!(
                "frame {i} is cut short: {} of its 4 length bytes",
                rest.len()
            )));
        };
        let len = u32::from_be_bytes(*head);
        let Some((_, after)) = usize::try_from(len)
            .ok()
            .and_then(|n| tail.split_at_checked(n))
        else {
            return Err(OtelError::Framing(format!(
                "frame {i} declares {len} bytes but {} remain",
                tail.len()
            )));
        };
        i += 1;
        rest = after;
    }
    Ok(Frames(bytes))
}

/// Frames [`split_frames`] found to tile their input.
struct Frames<'a>(&'a [u8]);

impl<'a> Iterator for Frames<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_first_chunk::<4>()?;
        let n = usize::try_from(u32::from_be_bytes(*head)).ok()?;
        let (body, rest) = tail.split_at_checked(n)?;
        self.0 = rest;
        Some(body)
    }
}

/// One decompressed-output budget and one entry budget run across all
/// frames.
fn decode_frames(
    frames: Frames<'_>,
    depth: usize,
    cap: u64,
    budget: u64,
    entries: &mut Entries,
) -> Result<Vec<Value>> {
    let mut left = budget;
    let mut out = Vec::new();
    for (i, frame) in frames.enumerate() {
        if frame.is_empty() {
            return Err(OtelError::Framing(format!("frame {i} is empty")));
        }
        out.extend(
            decode_frame(frame, depth, cap, &mut left, entries).map_err(|e| in_frame(i, e))?,
        );
    }
    Ok(out)
}

fn decode_frame(
    frame: &[u8],
    depth: usize,
    cap: u64,
    budget: &mut u64,
    entries: &mut Entries,
) -> Result<Vec<Value>> {
    if let Some((inner, _)) = decompress(frame, depth, cap, *budget)? {
        *budget -= inner.len() as u64;
        return decode_frame(&inner, depth + 1, cap, budget, entries);
    }
    if looks_like_json(frame) {
        return match decode_text(frame, entries) {
            Err(OtelError::NotOtlp) => Err(OtelError::Json("not an OTLP object".into())),
            other => other,
        };
    }
    #[cfg(feature = "protobuf")]
    {
        decode_protobuf_within(frame, entries).map(|v| vec![v])
    }
    #[cfg(not(feature = "protobuf"))]
    {
        Err(OtelError::FeatureDisabled("protobuf"))
    }
}

fn in_frame(i: usize, e: OtelError) -> OtelError {
    match e {
        // Framed bytes are OTLP output gone wrong, never a stray file to skip.
        OtelError::Protobuf(m) | OtelError::NotOtlpBody(m) => {
            OtelError::Protobuf(format!("frame {i}: {m}"))
        }
        OtelError::Json(m) => OtelError::Json(format!("frame {i}: {m}")),
        OtelError::Decompress(m) => OtelError::Decompress(format!("frame {i}: {m}")),
        other => other,
    }
}

/// One JSON value or JSON lines; [`decode_layer`] decides whether a `Json`
/// error stands or the bytes are protobuf. Entries are charged only for
/// text that decodes; over the cap, text that may be protobuf is a `Json`
/// error, so it is still tried as protobuf.
fn decode_text(bytes: &[u8], entries: &mut Entries) -> Result<Vec<Value>> {
    let text =
        std::str::from_utf8(bytes).map_err(|e| OtelError::Json(format!("not UTF-8 text: {e}")))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut charged = *entries;
    charge_json(text.as_bytes(), &mut charged).map_err(|e| {
        if looks_like_json(bytes) {
            e
        } else {
            OtelError::Json(e.to_string())
        }
    })?;
    let values = parse_text(text)?;
    *entries = charged;
    Ok(values)
}

fn parse_text(text: &str) -> Result<Vec<Value>> {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return if is_otlp(&v) {
            Ok(vec![v])
        } else {
            Err(OtelError::NotOtlp)
        };
    }
    let mut values: Vec<(usize, Value)> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .map_err(|e| OtelError::Json(format!("line {}: {e}", i + 1)))?;
        values.push((i + 1, v));
    }
    if !values.iter().any(|(_, v)| is_otlp(v)) {
        return Err(OtelError::NotOtlp);
    }
    if let Some((n, _)) = values.iter().find(|(_, v)| !is_otlp(v)) {
        return Err(OtelError::Json(format!("line {n}: not an OTLP object")));
    }
    Ok(values.into_iter().map(|(_, v)| v).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_limit_is_one_gib_and_four_layers() {
        assert_eq!(MAX_DECOMPRESSED, 1 << 30);
        assert_eq!(MAX_LAYERS, 4);
        assert_eq!(MAX_ENTRIES, 1 << 20);
        assert_eq!(
            DecodeLimits::default(),
            DecodeLimits::default()
                .with_decompressed(MAX_DECOMPRESSED)
                .with_entries(MAX_ENTRIES)
        );
    }

    #[test]
    fn human_bytes_names_the_exact_value() {
        assert_eq!(human_bytes(1 << 30), "1 GiB");
        assert_eq!(human_bytes(1 << 20), "1 MiB");
        assert_eq!(human_bytes(1536), "1536 B");
        assert_eq!(human_bytes(3 << 30), "3 GiB");
        assert_eq!(human_bytes(1536 << 20), "1536 MiB");
        assert_eq!(human_bytes(512 << 10), "512 KiB");
        assert_eq!(human_bytes(1024), "1 KiB");
        assert_eq!(human_bytes(1000), "1000 B");
        assert_eq!(human_bytes(0), "0 B");
    }

    #[test]
    fn frames_must_tile_the_input() {
        assert_eq!(
            split_frames(&[0, 0, 0, 1, 7, 0, 0, 0, 0])
                .unwrap()
                .collect::<Vec<_>>(),
            vec![&[7u8][..], &[][..]]
        );
        let err = split_frames(&[0, 0, 0, 1, 7, 0, 0]).map(drop).unwrap_err();
        assert!(
            matches!(&err, OtelError::Framing(m) if m == "frame 1 is cut short: 2 of its 4 length bytes"),
            "{err:?}"
        );
        let err = split_frames(&[0, 0, 0, 5, 1, 2]).map(drop).unwrap_err();
        assert!(
            matches!(&err, OtelError::Framing(m) if m == "frame 0 declares 5 bytes but 2 remain"),
            "{err:?}"
        );
        // The largest declarable length never slices or overflows.
        assert!(split_frames(&[0xff, 0xff, 0xff, 0xff, 1]).is_err());
    }

    #[test]
    fn a_leading_bom_before_a_brace_is_json_text() {
        assert!(looks_like_json(b"\xef\xbb\xbf {"));
        assert!(looks_like_json(b" \n["));
        assert!(!looks_like_json(b"\x0a\x00"));
        assert!(!looks_like_json(b"hello {"));
    }
}

#[cfg(all(test, feature = "compression"))]
mod compression_tests {
    use super::*;
    use std::io::Write;

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        e.write_all(bytes).unwrap();
        e.finish().unwrap()
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// `head -c 100 /dev/zero | zstd -q -c -19` (zstd 1.5.7).
    const HUNDRED_ZEROS: &str = "28b52ffd046845000010000001003f012c2f502cc9";
    /// `printf '{"resourceSpans":[]}' | zstd -q -c -19 --no-check` (zstd 1.5.7).
    const EMPTY_TRACES: &str = "28b52ffd0068a100007b227265736f757263655370616e73223a5b5d7d";
    const BODY: &[u8] = br#"{"resourceSpans":[]}"#;

    fn too_large(err: &OtelError, cap: u64) -> bool {
        matches!(err, OtelError::TooLarge { limit } if *limit == cap)
    }

    #[test]
    fn decompressed_output_is_capped_across_layers() {
        let body = br#"{"resourceSpans":[]}"#;
        let bomb = gzip(&vec![b' '; 4096]);
        assert!(bomb.len() < 100);
        let err = decode_input_with_limit(&bomb, None, 1024).unwrap_err();
        assert!(too_large(&err, 1024), "{err:?}");
        assert_eq!(
            err.to_string(),
            "cannot decompress: decompressed output exceeds 1 KiB"
        );

        let once = gzip(body);
        let twice = gzip(&once);
        let need = (once.len() + body.len()) as u64;
        assert!(decode_input_with_limit(&twice, None, need).is_ok());
        // The error names the whole cap, not what the inner layer had left.
        let err = decode_input_with_limit(&twice, None, need - 1).unwrap_err();
        assert!(too_large(&err, need - 1), "{err:?}");
    }

    #[test]
    fn zstd_output_cap_is_enforced() {
        let z = hex(HUNDRED_ZEROS);
        let (out, suffix) = decompress(&z, 0, 100, 100).unwrap().unwrap();
        assert_eq!((out, suffix), (vec![0u8; 100], ".zst"));
        let err = decompress(&z, 0, 99, 99).unwrap_err();
        assert!(too_large(&err, 99), "{err:?}");
    }

    #[test]
    fn gzip_and_zstd_share_one_budget() {
        let z = hex(EMPTY_TRACES);
        let g = gzip(&z);
        let need = (z.len() + BODY.len()) as u64;
        let v = decode_input_with_limit(&g, Some("t.json.zst.gz"), need).unwrap();
        assert_eq!(v, vec![serde_json::json!({"resourceSpans": []})]);
        let err = decode_input_with_limit(&g, None, need - 1).unwrap_err();
        assert!(too_large(&err, need - 1), "{err:?}");
    }

    #[test]
    fn frames_share_one_budget() {
        let z = hex(EMPTY_TRACES);
        let mut framed = Vec::new();
        for _ in 0..2 {
            framed.extend((z.len() as u32).to_be_bytes());
            framed.extend(&z);
        }
        let need = 2 * BODY.len() as u64;
        assert_eq!(
            decode_input_with_limit(&framed, None, need).unwrap().len(),
            2
        );
        let err = decode_input_with_limit(&framed, None, need - 1).unwrap_err();
        assert!(too_large(&err, need - 1), "{err:?}");
    }

    #[test]
    fn concatenated_zstd_frames_decode_as_one_stream() {
        let z = [hex(HUNDRED_ZEROS), hex(HUNDRED_ZEROS)].concat();
        assert_eq!(unzstd(&z, 1000).unwrap().len(), 200);
    }

    #[test]
    fn concatenated_zstd_frames_stop_at_the_cap() {
        // Two good frames, then bytes that are no zstd frame.
        let z = [hex(HUNDRED_ZEROS), hex(HUNDRED_ZEROS), vec![0xff; 4]].concat();
        let err = decompress(&z, 0, 150, 150).unwrap_err();
        assert!(too_large(&err, 150), "{err:?}");
        let err = decompress(&z, 0, 1000, 1000).unwrap_err();
        assert!(matches!(err, OtelError::Decompress(_)), "{err:?}");
    }

    #[test]
    fn a_truncated_zstd_stream_is_decompress() {
        let z = hex(HUNDRED_ZEROS);
        assert!(matches!(
            unzstd(&z[..z.len() - 3], 1000),
            Err(OtelError::Decompress(_))
        ));
    }

    // `printf '\n\0' | zstd -q -c -19 --no-check`: a protobuf body with one
    // empty resourceSpans entry.
    #[test]
    fn a_zstd_name_suffix_is_dropped() {
        let z = hex("28b52ffd00681100000a00");
        let named = decode_input_with_limit(&z, Some("t.binpb.zst"), 1 << 20);
        let sniffed = decode_input_with_limit(&z, Some("t.zst"), 1 << 20);
        #[cfg(feature = "protobuf")]
        {
            assert_eq!(
                named.unwrap(),
                vec![serde_json::json!({"resourceSpans": [{}]})]
            );
            assert!(
                matches!(sniffed, Err(OtelError::NotOtlpBody(_))),
                "{sniffed:?}"
            );
        }
        #[cfg(not(feature = "protobuf"))]
        {
            assert!(
                matches!(named, Err(OtelError::FeatureDisabled("protobuf"))),
                "{named:?}"
            );
            assert!(matches!(sniffed, Err(OtelError::Json(_))), "{sniffed:?}");
        }
    }
}

#[cfg(all(test, not(feature = "protobuf")))]
mod protobuf_off_tests {
    use super::*;

    #[test]
    fn binary_without_the_feature_is_feature_disabled_or_json() {
        let body = [0x0a, 0x00];
        assert!(matches!(
            decode_input(&body, Some("t.binpb")),
            Err(OtelError::FeatureDisabled("protobuf"))
        ));
        assert!(matches!(
            decode_input(&body, Some("t.bin")),
            Err(OtelError::Json(_))
        ));
        let framed = [0x00, 0x00, 0x00, 0x02, 0x0a, 0x00];
        assert!(matches!(
            decode_input(&framed, None),
            Err(OtelError::FeatureDisabled("protobuf"))
        ));
    }
}

#[cfg(all(test, not(feature = "compression")))]
mod compression_off_tests {
    use super::*;

    #[test]
    fn zstd_without_the_feature_is_feature_disabled() {
        let z = [0x28, 0xb5, 0x2f, 0xfd, 0x00];
        assert!(matches!(
            decode_input(&z, None),
            Err(OtelError::FeatureDisabled("compression"))
        ));
    }
}
