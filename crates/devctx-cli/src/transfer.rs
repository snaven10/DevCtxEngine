//! The on-disk shape of an exported memory: one JSON object per line.
//!
//! JSONL rather than a database file because the case that justifies exporting
//! at all is handing memories to someone on a different release — and a DuckDB
//! file is readable only by the build that wrote it. A text line survives that,
//! greps, diffs, streams without loading the lot into memory, and can be
//! repaired by hand when one row is wrong.

use anyhow::{Context, Result};
use devctx_store::Memory;
use serde::{Deserialize, Serialize};

/// The embedding of a memory, with what produced it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Embedding {
    /// Registry key of the model, e.g. `ml-granite`.
    pub model: String,
    /// Vector width. Carried beside the name because two implementations can
    /// share a model name and not share a vector space — measured at 0.76–0.87
    /// cosine between them, where the same space would be 1.00.
    pub dim: usize,
    pub vector: Vec<f32>,
}

/// One exported memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferLine {
    #[serde(flatten)]
    pub memory: Memory,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Embedding>,
}

/// Serialize one memory as a single line.
pub fn to_line(m: &Memory, embedding: Option<Embedding>) -> Result<String> {
    let line = TransferLine {
        memory: m.clone(),
        embedding,
    };
    serde_json::to_string(&line).context("serializing a memory")
}

/// Parse one line back.
///
/// The error names what it was reading. An import of a thousand lines that
/// fails with "expected value at line 1 column 1" and nothing else leaves
/// nobody anywhere to look.
pub fn from_line(s: &str) -> Result<TransferLine> {
    serde_json::from_str(s).with_context(|| {
        let preview: String = s.chars().take(60).collect();
        format!("reading a line of the export: {preview}…")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Memory {
        Memory {
            id: "mem_a".into(),
            title: "Título con acento".into(),
            content: "línea uno\nlínea dos".into(),
            memory_type: "decision".into(),
            scope: "group".into(),
            project: "@group:REVFA".into(),
            topic_key: "auth".into(),
            tags: "a,b".into(),
            repo: "api".into(),
            normalized_hash: "abc123".into(),
            created_at: "100".into(),
            updated_at: "200".into(),
            ..Default::default()
        }
    }

    /// A line must survive the trip unchanged, newlines and accents included:
    /// memories are prose, and prose is where a lossy encoding shows up as a
    /// corrupted sentence rather than an error.
    #[test]
    fn a_memory_round_trips_through_a_line() {
        let m = sample();
        let line = to_line(&m, None).unwrap();
        assert!(!line.contains('\n'), "one memory per line, always");

        let back = from_line(&line).unwrap();
        assert_eq!(back.memory, m);
        assert!(back.embedding.is_none());
    }

    /// The embedding travels with the model that produced it. Without the name
    /// and width beside it an importer cannot tell a reusable vector from one
    /// that would rank everything wrongly.
    #[test]
    fn an_embedding_travels_with_its_model_and_width() {
        let e = Embedding {
            model: "ml-granite".into(),
            dim: 3,
            vector: vec![0.5, 0.25, 0.125],
        };
        let line = to_line(&sample(), Some(e.clone())).unwrap();
        let back = from_line(&line).unwrap().embedding.expect("carried");
        assert_eq!(back.model, "ml-granite");
        assert_eq!(back.dim, 3);
        assert_eq!(back.vector, e.vector);
    }

    /// A damaged line names itself.
    #[test]
    fn a_damaged_line_reports_what_it_could_not_read() {
        let err = from_line("{not json").unwrap_err().to_string();
        assert!(
            err.contains("line"),
            "the message must locate the problem: {err}"
        );
    }

    /// A file written by an older build lacks fields a newer one added, and
    /// must still import: refusing the whole file over one absent key would
    /// make every upgrade a migration.
    #[test]
    fn a_line_missing_optional_fields_still_reads() {
        let line = r#"{"id":"mem_x","content":"just this","project":"@global"}"#;
        let back = from_line(line).unwrap();
        assert_eq!(back.memory.id, "mem_x");
        assert_eq!(back.memory.content, "just this");
        assert_eq!(back.memory.title, "", "absent fields take their default");
    }
}
