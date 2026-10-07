#![doc = include_str!("../README.md")]

include!(concat!(env!("OUT_DIR"), "/pathbase_client.rs"));

/// Percent-encode one path segment the way the generated operations do.
pub fn encode_segment(segment: &str) -> String {
    encode_path(segment)
}
