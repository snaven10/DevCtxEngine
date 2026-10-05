//! One reader — and the contract — for the answers of `/search`,
//! `/search_routes` and `/routes_for_handler`.
//!
//! # The contract (PLAN-008 TASK-010)
//!
//! Those tools **always** answer with an object. The rows live under `results`
//! (`search` — bound to a project or to a group — and `search_project`) or
//! `routes` (`search_routes`, `routes_for_handler`). Servers before the
//! PLAN-008 review answered the group search and `search_project` under `hits`;
//! [`search_hits`] still reads that key. Next to the rows, every one may carry:
//!
//! | field | meaning |
//! |---|---|
//! | `total` | rows that matched before paging (routes) |
//! | `next_offset` | pass it as `offset` to get the next page; absent on the last |
//! | `omitted` | `{count, reason: "limit"\|"budget", next_offset?}` — what was left out. Absent means nothing was |
//! | `omitted_for_budget` | legacy alias of the budget cut, with the names of what was dropped |
//! | `branch_fallback` | answered from another branch than the checked-out one |
//! | `warning` | the index comes from an older extractor |
//!
//! Until 0.9 the same tools answered a *bare array* when nothing needed saying,
//! and the object only otherwise. A reader that assumed the array lost every
//! hit the moment the object appeared, so every consumer reads through
//! [`search_hits`], which still accepts the bare array (older servers and
//! clients) and the object, and never mistakes a wrapper for a hit.

use serde_json::Value;

/// A search or routes answer, normalised.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchHits {
    /// The rows themselves. Never contains the wrapper object.
    pub hits: Vec<Value>,
    /// Present when the answer came from a branch other than the checked-out one.
    pub branch_fallback: Option<Value>,
    /// What was left out (`omitted`, or the legacy `omitted_for_budget` when
    /// the answer predates it).
    pub omitted: Option<Value>,
    /// The legacy `omitted_for_budget` note as the answer carried it (it names
    /// what was dropped, which `omitted` does not).
    pub omitted_for_budget: Option<Value>,
    /// Rows that matched before paging, when the answer says so.
    pub total: Option<u64>,
    /// Offset of the next page, when there is one.
    pub next_offset: Option<u64>,
    /// Present when the index is stale (older extractor).
    pub warning: Option<Value>,
}

impl SearchHits {
    /// True when there is any note a reader should be told about.
    pub fn has_notes(&self) -> bool {
        self.branch_fallback.is_some() || self.warning.is_some() || self.omitted.is_some()
    }

    /// The notes as one JSON object (`branch_fallback`, `warning`, `omitted`,
    /// and the legacy `omitted_for_budget`), or `None` when there are none.
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
            m.insert("omitted".into(), v.clone());
        }
        if let Some(v) = &self.omitted_for_budget {
            m.insert("omitted_for_budget".into(), v.clone());
        }
        Some(Value::Object(m))
    }
}

/// Read the object of the contract above, or the legacy bare array. Rows come
/// from `results` / `routes` (also `hits`, the key older servers used for
/// `search_project` and the group search), plus
/// the optional notes. Anything else yields no
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
                omitted: note("omitted").or_else(|| note("omitted_for_budget")),
                omitted_for_budget: note("omitted_for_budget"),
                total: m.get("total").and_then(|x| x.as_u64()),
                next_offset: m.get("next_offset").and_then(|x| x.as_u64()),
                warning: note("warning"),
            }
        }
        _ => SearchHits::default(),
    }
}

/// The score to compare a hit on across stores: `raw_score` (the retriever's
/// cosine, or RRF in hybrid, before the kind penalty or a cross-encoder
/// rewrote it) when the hit carries one, else `score`, else `0.0`.
///
/// A group's fan-out ranks members against each other; a clamped or reranked
/// `score` means something different in each store, the retriever's does not
/// (same model and width, which the fan-out checks).
pub fn hit_raw_score(hit: &Value) -> f64 {
    hit.get("raw_score")
        .and_then(|v| v.as_f64())
        .or_else(|| hit.get("score").and_then(|v| v.as_f64()))
        .unwrap_or(0.0)
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

    /// Fixup G: cross-store comparisons read the retriever's score.
    #[test]
    fn raw_score_wins_over_a_rewritten_score() {
        assert_eq!(hit_raw_score(&json!({"score": 0.5, "raw_score": 0.8})), 0.8);
        assert_eq!(hit_raw_score(&json!({"score": 0.5})), 0.5);
        assert_eq!(hit_raw_score(&json!({})), 0.0);
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

    /// The current contract: `omitted` + paging fields, no legacy key.
    #[test]
    fn the_unified_object_carries_paging_and_omitted() {
        let h = search_hits(&json!({
            "routes": [{"path": "/x"}],
            "total": 45,
            "next_offset": 20,
            "omitted": {"count": 25, "reason": "limit", "next_offset": 20},
        }));
        assert_eq!(h.hits.len(), 1);
        assert_eq!(h.total, Some(45));
        assert_eq!(h.next_offset, Some(20));
        assert_eq!(h.omitted.as_ref().unwrap()["reason"], "limit");
        assert!(h.omitted_for_budget.is_none());
        assert_eq!(h.notes_json().unwrap()["omitted"]["count"], 25);
    }

    /// Both spellings of the budget cut at once (what the servers send for a
    /// release): `omitted` wins, the legacy note keeps its item names.
    #[test]
    fn omitted_and_its_legacy_alias_are_both_kept() {
        let h = search_hits(&json!({
            "results": [],
            "omitted": {"count": 2, "reason": "budget"},
            "omitted_for_budget": {"count": 2, "items": ["a.rs:1", "b.rs:9"]},
        }));
        assert_eq!(h.omitted.as_ref().unwrap()["reason"], "budget");
        assert_eq!(h.omitted_for_budget.as_ref().unwrap()["items"][1], "b.rs:9");
    }

    /// An old server answers a bare array and the legacy object; neither has
    /// the new fields and neither may lose its rows.
    #[test]
    fn old_shapes_still_read() {
        let bare = search_hits(&json!([{"file": "a.rs"}]));
        assert_eq!(bare.hits.len(), 1);
        assert!(bare.total.is_none() && bare.next_offset.is_none());
        let legacy = search_hits(&json!({
            "routes": [{"path": "/x"}],
            "omitted_for_budget": {"count": 4},
        }));
        assert_eq!(legacy.hits.len(), 1);
        assert_eq!(legacy.omitted.unwrap()["count"], 4);
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
