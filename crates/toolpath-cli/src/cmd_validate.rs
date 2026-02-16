use anyhow::{Context, Result};
use std::path::PathBuf;
use toolpath::v1::Document;

pub fn run(input: PathBuf) -> Result<()> {
    let content =
        std::fs::read_to_string(&input).with_context(|| format!("Failed to read {:?}", input))?;
    validate_content(&content)
}

fn validate_content(content: &str) -> Result<()> {
    match Document::from_json(content) {
        Ok(doc) => {
            let kind = match &doc {
                Document::Graph(g) => format!("Graph (id: {})", g.graph.id),
                Document::Path(p) => format!("Path (id: {}, {} steps)", p.path.id, p.steps.len()),
                Document::Step(s) => format!("Step (id: {})", s.step.id),
            };
            println!("Valid: {}", kind);
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!("Invalid: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_validate_valid_step() {
        let json = r#"{"Step":{"step":{"id":"s1","actor":"human:alex","timestamp":"2026-01-01T00:00:00Z"},"change":{}}}"#;
        assert!(validate_content(json).is_ok());
    }

    #[test]
    fn test_validate_valid_path() {
        let json = r#"{"Path":{"path":{"id":"p1","head":"s1"},"steps":[]}}"#;
        assert!(validate_content(json).is_ok());
    }

    #[test]
    fn test_validate_valid_graph() {
        let json = r#"{"Graph":{"graph":{"id":"g1"},"paths":[]}}"#;
        assert!(validate_content(json).is_ok());
    }

    #[test]
    fn test_validate_invalid_json() {
        assert!(validate_content("not json").is_err());
    }

    #[test]
    fn test_validate_invalid_structure() {
        assert!(validate_content(r#"{"Unknown":{}}"#).is_err());
    }

    #[test]
    fn test_run_with_temp_file() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, r#"{{"Step":{{"step":{{"id":"s1","actor":"human:alex","timestamp":"2026-01-01T00:00:00Z"}},"change":{{}}}}}}"#).unwrap();
        f.flush().unwrap();
        assert!(run(f.path().to_path_buf()).is_ok());
    }

    #[test]
    fn test_run_nonexistent_file() {
        assert!(run(PathBuf::from("/nonexistent/file.json")).is_err());
    }
}
