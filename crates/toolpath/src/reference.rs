//! Portable references to an immutable graph document's scoped step.
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// A graph document URI followed by individually encoded path and step IDs.
/// The document URI is preserved verbatim, including storage-version queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BaseReference {
    document: String,
    path: String,
    step: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceError(pub &'static str);
impl fmt::Display for ReferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ReferenceError {}

impl BaseReference {
    pub fn new(
        document: impl Into<String>,
        path: impl Into<String>,
        step: impl Into<String>,
    ) -> Result<Self, ReferenceError> {
        let result = Self {
            document: document.into(),
            path: path.into(),
            step: step.into(),
        };
        let parsed = url::Url::parse(&result.document)
            .map_err(|_| ReferenceError("base.from requires an absolute document URI"))?;
        if result.document.contains('#')
            || result
                .document
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(ReferenceError(
                "document URI must not contain a fragment or whitespace",
            ));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(ReferenceError("base.from must not embed credentials"));
        }
        validate_escapes(&result.document)?;
        if result.path.is_empty()
            || result.step.is_empty()
            || result
                .path
                .chars()
                .chain(result.step.chars())
                .any(char::is_control)
        {
            return Err(ReferenceError(
                "base.from requires nonempty path and step IDs without control characters",
            ));
        }
        Ok(result)
    }
    pub fn document_uri(&self) -> &str {
        &self.document
    }
    pub fn path_id(&self) -> &str {
        &self.path
    }
    pub fn step_id(&self) -> &str {
        &self.step
    }
}
fn validate_escapes(value: &str) -> Result<(), ReferenceError> {
    let bytes = value.as_bytes();
    for (i, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && (i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit())
        {
            return Err(ReferenceError("malformed percent escape in base.from"));
        }
    }
    Ok(())
}
fn decode(value: &str) -> Result<String, ReferenceError> {
    validate_escapes(value)?;
    if value
        .bytes()
        .any(|b| !(b.is_ascii_alphanumeric() || b"-._~%".contains(&b)))
    {
        return Err(ReferenceError(
            "base.from IDs must be URI-component encoded",
        ));
    }
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            out.push(
                u8::from_str_radix(&value[i + 1..i + 3], 16)
                    .map_err(|_| ReferenceError("invalid escape"))?,
            );
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ReferenceError("base.from IDs must decode to UTF-8"))
}
fn encode(value: &str, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            write!(f, "{}", b as char)?;
        } else {
            write!(f, "%{b:02X}")?;
        }
    }
    Ok(())
}
impl FromStr for BaseReference {
    type Err = ReferenceError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (document, fragment) = value
            .split_once('#')
            .ok_or(ReferenceError("base.from requires a path/step fragment"))?;
        let (path, step) = fragment.split_once('/').ok_or(ReferenceError(
            "base.from requires one literal fragment slash",
        ))?;
        Self::new(document, decode(path)?, decode(step)?)
    }
}
impl fmt::Display for BaseReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#", self.document)?;
        encode(&self.path, f)?;
        f.write_str("/")?;
        encode(&self.step, f)
    }
}
impl TryFrom<String> for BaseReference {
    type Error = ReferenceError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
impl From<BaseReference> for String {
    fn from(value: BaseReference) -> Self {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portable_roundtrip_and_single_decode() {
        for document in [
            "https://host/u/o/r/graphs/g?version=one",
            "s3://bucket/a.json?versionId=v%2F1",
            "file:///archives/g.json",
        ] {
            let r = BaseReference::new(document, "path/%#é", "step%2F/ #").unwrap();
            assert_eq!(r.to_string().parse::<BaseReference>().unwrap(), r);
            assert_eq!(r.document_uri(), document);
            let json = serde_json::to_string(&r).unwrap();
            assert_eq!(serde_json::from_str::<BaseReference>(&json).unwrap(), r);
        }
    }
    #[test]
    fn structural_base_json_and_jsonl_roundtrip() {
        use crate::v1::{Base, Graph, Path, Step};
        let reference =
            BaseReference::new("s3://bucket/g.json?versionId=1", "p/one", "s%two").unwrap();
        let mut path = Path::new(
            "child",
            Some(Base::from_reference(reference.clone())),
            "new",
        );
        path.steps
            .push(Step::new("new", "human:a", "2026-09-10T00:00:00Z"));
        path.path.head = "new".into();
        let graph = Graph::from_path(path);
        let json = serde_json::to_value(&graph).unwrap();
        assert!(json["paths"][0]["path"]["base"].get("uri").is_none());
        let decoded: Graph = serde_json::from_value(json).unwrap();
        assert_eq!(
            decoded
                .single_path()
                .unwrap()
                .path
                .base
                .as_ref()
                .unwrap()
                .from
                .as_ref(),
            Some(&reference)
        );
        let stream = graph.to_jsonl_string().unwrap();
        let decoded = Graph::from_jsonl_str(&stream).unwrap();
        assert_eq!(
            decoded
                .single_path()
                .unwrap()
                .path
                .base
                .as_ref()
                .unwrap()
                .from
                .as_ref(),
            Some(&reference)
        );
    }
    #[test]
    fn malformed_references_are_rejected() {
        for value in [
            "relative#p/s",
            "https://h/x",
            "https://h/x#p",
            "https://h/x#p/s/t",
            "https://h/x#/s",
            "https://h/x#p/",
            "https://h/x#p/%FF",
            "https://h/x#p/%x0",
            "https://h/x#p/s#x",
            "https://user:secret@h/x#p/s",
            "https://h/x#p/a b",
            "https://h/%zz#p/s",
            "https://h/x#p/%00",
        ] {
            assert!(value.parse::<BaseReference>().is_err(), "accepted {value}");
        }
    }
}
