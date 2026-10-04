//! One reader for the answers of `/search`, `/search_routes` and
//! `/routes_for_handler`.
//!
//! Those answers come in two shapes: a bare JSON array (the common case) or an
//! object that carries the array under `results` (search) or `routes` (routes)
//! next to optional notes — `branch_fallback`, `warning`, `omitted_for_budget`.
//! A consumer that assumes the array silently loses every hit the moment the
//! object shape appears, so every consumer reads through [`search_hits`].

use serde_json::Value;

/// A search or routes answer, normalised.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchHits {
    /// The rows themselves. Never contains the wrapper object.
    pub hits: Vec<Value>,
    /// Present when the answer came from a branch other than the checked-out one.
    pub branch_fallback: Option<Value>,
    /// Present when the answer was truncated to fit the output budget.
    pub omitted: Option<Value>,
    /// Present when the index is stale (older extractor).
    pub warning: Option<Value>,
}

impl SearchHits {
    /// True when there is any note a reader should be told about.
    pub fn has_notes(&self) -> bool {
        self.branch_fallback.is_some() || self.warning.is_some() || self.omitted.is_some()
    }

    /// The notes as one JSON object (`branch_fallback`, `warning`,
    /// `omitted_for_budget`), or `None` when there are none.
    pub fn notes_json(&self) -> Option<Value> {
        if !self.has_notes() {
            return None;
        }
        let mut m = serde_json::Map::new();
        if let Some(v) = &self.branch_fallback {
            m.insert("branch_fallback".into(), v.clone());
        }
        if let Some(v) = &self.warning {
            m.insert("warning".into(), v.clone());
        }
        if let Some(v) = &self.omitted {
            m.insert("omitted_for_budget".into(), v.clone());
        }
        Some(Value::Object(m))
    }
}

/// Read a bare array, or an object with `results` / `routes` (also `hits`, the
/// key of `search_project`) plus the optional notes. Anything else yields no
/// hits: a wrapper object is never mistaken for a hit.
pub fn search_hits(v: &Value) -> SearchHits {
    match v {
        Value::Array(a) => SearchHits {
            hits: a.clone(),
            ..Default::default()
        },
        Value::Object(m) => {
            let hits = ["results", "routes", "hits"]
                .iter()
                .find_map(|k| m.get(*k).and_then(|x| x.as_array()))
                .cloned()
                .unwrap_or_default();
            let note = |k: &str| m.get(k).filter(|x| !x.is_null()).cloned();
            SearchHits {
                hits,
                branch_fallback: note("branch_fallback"),
                omitted: note("omitted_for_budget"),
                warning: note("warning"),
            }
        }
        _ => SearchHits::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_bare_array_is_the_hits() {
        let h = search_hits(&json!([{"file": "a.rs"}, {"file": "b.rs"}]));
        assert_eq!(h.hits.len(), 2);
        assert!(!h.has_notes());
        assert!(h.notes_json().is_none());
    }

    #[test]
    fn results_with_branch_fallback() {
        let h = search_hits(&json!({
            "results": [{"file": "a.rs"}],
            "branch_fallback": {"current": "b", "used": "a"},
        }));
        assert_eq!(h.hits.len(), 1);
        assert_eq!(h.branch_fallback.as_ref().unwrap()["used"], "a");
        assert!(h.warning.is_none() && h.omitted.is_none());
        assert_eq!(h.notes_json().unwrap()["branch_fallback"]["current"], "b");
    }

    #[test]
    fn results_with_omitted_for_budget() {
        let h = search_hits(&json!({
            "results": [{"file": "a.rs"}],
            "omitted_for_budget": {"count": 3},
        }));
        assert_eq!(h.hits.len(), 1);
        assert_eq!(h.omitted.unwrap()["count"], 3);
    }

    #[test]
    fn routes_with_warning_and_fallback() {
        let h = search_hits(&json!({
            "routes": [{"path": "/x"}, {"path": "/y"}],
            "warning": "older extractor",
            "branch_fallback": {"used": "main"},
        }));
        assert_eq!(h.hits.len(), 2);
        assert_eq!(h.warning.unwrap(), "older extractor");
        assert!(h.branch_fallback.is_some());
    }

    #[test]
    fn search_project_hits_key_is_read() {
        let h = search_hits(&json!({"hits": [{"file": "a.rs"}], "project": "p"}));
        assert_eq!(h.hits.len(), 1);
    }

    #[test]
    fn unknown_shapes_never_fabricate_a_hit() {
        assert!(search_hits(&json!({"something": "else"})).hits.is_empty());
        assert!(search_hits(&json!("text")).hits.is_empty());
        assert!(search_hits(&json!(null)).hits.is_empty());
        assert!(search_hits(&json!({"results": null})).hits.is_empty());
    }

    #[test]
    fn null_notes_are_absent() {
        let h = search_hits(&json!({"results": [], "warning": null}));
        assert!(h.warning.is_none());
    }
}
