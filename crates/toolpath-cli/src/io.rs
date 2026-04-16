use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::path::PathBuf;
use toolpath::v1::Document;

pub enum InputSpec {
    Stdin,
    File(PathBuf),
}

pub enum OutputSpec {
    Stdout,
    File(PathBuf),
}

impl InputSpec {
    pub fn from_opt(p: Option<PathBuf>) -> Self {
        match p {
            Some(p) if p.as_os_str() == "-" => Self::Stdin,
            Some(p) => Self::File(p),
            None => Self::Stdin,
        }
    }

    pub fn from_str(s: &str) -> Self {
        if s == "-" {
            Self::Stdin
        } else {
            Self::File(PathBuf::from(s))
        }
    }

    pub fn read_string(&self) -> Result<String> {
        match self {
            Self::Stdin => {
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .context("Failed to read from stdin")?;
                Ok(buf)
            }
            Self::File(path) => std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read {:?}", path)),
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Stdin => "<stdin>",
            Self::File(p) => p.to_str().unwrap_or("<file>"),
        }
    }
}

impl OutputSpec {
    pub fn from_opt(p: Option<PathBuf>) -> Self {
        match p {
            Some(p) if p.as_os_str() == "-" => Self::Stdout,
            Some(p) => Self::File(p),
            None => Self::Stdout,
        }
    }

    pub fn write_str(&self, s: &str) -> Result<()> {
        match self {
            Self::Stdout => {
                let mut out = std::io::stdout().lock();
                out.write_all(s.as_bytes())
                    .context("Failed to write to stdout")?;
                Ok(())
            }
            Self::File(path) => std::fs::write(path, s)
                .with_context(|| format!("Failed to write {:?}", path)),
        }
    }
}

pub fn read_document(input: &InputSpec) -> Result<Document> {
    let content = input.read_string()?;
    Document::from_json(&content)
        .with_context(|| format!("Failed to parse Toolpath document from {}", input.label()))
}

pub fn write_document(doc: &Document, out: &OutputSpec, pretty: bool) -> Result<()> {
    let json = if pretty {
        doc.to_json_pretty()
    } else {
        doc.to_json()
    }
    .context("failed to serialize document")?;
    let line = if matches!(out, OutputSpec::Stdout) {
        format!("{}\n", json)
    } else {
        json
    };
    out.write_str(&line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_spec_from_opt_none_is_stdin() {
        assert!(matches!(InputSpec::from_opt(None), InputSpec::Stdin));
    }

    #[test]
    fn input_spec_from_opt_dash_is_stdin() {
        assert!(matches!(
            InputSpec::from_opt(Some(PathBuf::from("-"))),
            InputSpec::Stdin
        ));
    }

    #[test]
    fn input_spec_from_opt_file() {
        let s = InputSpec::from_opt(Some(PathBuf::from("foo.json")));
        match s {
            InputSpec::File(p) => assert_eq!(p, PathBuf::from("foo.json")),
            _ => panic!("expected File"),
        }
    }

    #[test]
    fn input_spec_from_str_dash_is_stdin() {
        assert!(matches!(InputSpec::from_str("-"), InputSpec::Stdin));
    }

    #[test]
    fn input_spec_from_str_file() {
        match InputSpec::from_str("doc.json") {
            InputSpec::File(p) => assert_eq!(p, PathBuf::from("doc.json")),
            _ => panic!("expected File"),
        }
    }

    #[test]
    fn output_spec_from_opt_none_is_stdout() {
        assert!(matches!(OutputSpec::from_opt(None), OutputSpec::Stdout));
    }

    #[test]
    fn output_spec_from_opt_dash_is_stdout() {
        assert!(matches!(
            OutputSpec::from_opt(Some(PathBuf::from("-"))),
            OutputSpec::Stdout
        ));
    }

    #[test]
    fn read_document_file_roundtrip() {
        use std::io::Write as _;
        use toolpath::v1::Step;
        let step = Step::new("s1", "human:alex", "2026-01-01T00:00:00Z");
        let doc = Document::Step(step);
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}", doc.to_json().unwrap()).unwrap();
        f.flush().unwrap();
        let parsed = read_document(&InputSpec::File(f.path().to_path_buf())).unwrap();
        assert!(matches!(parsed, Document::Step(_)));
    }

    #[test]
    fn write_document_file() {
        use toolpath::v1::Step;
        let step = Step::new("s1", "human:alex", "2026-01-01T00:00:00Z");
        let doc = Document::Step(step);
        let f = tempfile::NamedTempFile::new().unwrap();
        write_document(&doc, &OutputSpec::File(f.path().to_path_buf()), true).unwrap();
        let back = std::fs::read_to_string(f.path()).unwrap();
        assert!(back.contains("\"Step\""));
    }

    #[test]
    fn read_document_file_missing() {
        let result = read_document(&InputSpec::File(PathBuf::from("/nonexistent/x.json")));
        assert!(result.is_err());
    }

    #[test]
    fn read_document_invalid_json() {
        use std::io::Write as _;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "not json").unwrap();
        f.flush().unwrap();
        let result = read_document(&InputSpec::File(f.path().to_path_buf()));
        assert!(result.is_err());
    }
}
