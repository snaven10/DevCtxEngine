//! Reciprocal rank fusion: combine ranked lists by *position*, not by score.
//!
//! This exists because scores are often incomparable between the lists being
//! merged — a vector similarity against a BM25 weight, or two vector scores from
//! different embedding models. Position is the one thing they agree on, and an
//! item that surfaces in several lists is rewarded for it.

use std::collections::HashMap;
use std::hash::Hash;

/// The standard RRF damping constant.
pub const RRF_K: f32 = 60.0;

/// Fuse ranked lists, keeping the first occurrence of each item.
///
/// `key` identifies an item across lists. Ties break on the key so the order is
/// stable across runs rather than dependent on hash iteration.
pub fn fuse_by_rank<T, K, F>(lists: Vec<Vec<T>>, key: F, limit: usize) -> Vec<T>
where
    K: Eq + Hash + Ord + Clone,
    F: Fn(&T) -> K,
{
    let mut scores: HashMap<K, f32> = HashMap::new();
    let mut items: HashMap<K, T> = HashMap::new();

    for list in lists {
        for (rank, item) in list.into_iter().enumerate() {
            let k = key(&item);
            *scores.entry(k.clone()).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
            items.entry(k).or_insert(item);
        }
    }

    let mut out: Vec<(K, T)> = items.into_iter().collect();
    out.sort_by(|a, b| {
        scores[&b.0]
            .partial_cmp(&scores[&a.0])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    out.truncate(limit);
    out.into_iter().map(|(_, v)| v).collect()
}

/// The fused score an item would receive, for callers that need to show it.
pub fn rank_score(positions: &[usize]) -> f32 {
    positions
        .iter()
        .map(|r| 1.0 / (RRF_K + *r as f32 + 1.0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_item_in_both_lists_outranks_one_that_leads_a_single_list() {
        let a = vec!["a_top", "both", "a_third"];
        let b = vec!["b_top", "both"];
        let fused = fuse_by_rank(vec![a, b], |s| s.to_string(), 10);
        assert_eq!(fused[0], "both");
        assert_eq!(fused.len(), 4, "deduplicated across lists");
    }

    #[test]
    fn order_is_stable_and_the_limit_is_honored() {
        let lists = vec![vec!["x", "y"], vec!["y", "x"]];
        let a = fuse_by_rank(lists.clone(), |s| s.to_string(), 10);
        let b = fuse_by_rank(lists, |s| s.to_string(), 10);
        assert_eq!(a, b, "ties must not depend on hash iteration order");
        assert_eq!(
            fuse_by_rank(vec![vec!["x", "y"]], |s| s.to_string(), 1).len(),
            1
        );
        assert!(fuse_by_rank(Vec::<Vec<&str>>::new(), |s| s.to_string(), 5).is_empty());
    }

    #[test]
    fn appearing_earlier_and_more_often_scores_higher() {
        assert!(rank_score(&[0]) > rank_score(&[5]));
        assert!(rank_score(&[0, 0]) > rank_score(&[0]));
    }
}
