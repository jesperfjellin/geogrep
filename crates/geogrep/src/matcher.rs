use crate::normalize::{compact, normalize};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

/// A pre-normalized query, computed once and reused against every candidate.
pub struct Query {
    pub normalized: String,
    pub compact: String,
    trailing_boundary: bool,
    normalized_self_score: i64,
    compact_self_score: i64,
}

impl Query {
    pub fn new(raw: &str) -> Self {
        let normalized = normalize(raw);
        let compact = compact(&normalized);
        let trailing_boundary = has_trailing_boundary(raw);
        let matcher = SkimMatcherV2::default();
        let normalized_self_score = self_score(&matcher, &normalized);
        let compact_self_score = self_score(&matcher, &compact);
        Self {
            normalized,
            compact,
            trailing_boundary,
            normalized_self_score,
            compact_self_score,
        }
    }

    pub fn is_exact_match(&self, candidate: &str) -> bool {
        normalize(candidate) == self.normalized
    }
}

/// Score `candidate` against `query` on a 0..=100 scale.
///
/// Exact normalized/compact equality scores 100. Everything else is delegated to
/// Skim's mature fuzzy matcher and normalized to fit the CLI score scale.
pub fn score(query: &Query, candidate: &str) -> u8 {
    let cand_norm = normalize(candidate);
    if cand_norm.is_empty() {
        return 0;
    }
    let cand_compact = compact(&cand_norm);

    if query.trailing_boundary && !has_boundary_match(&query.normalized, &cand_norm) {
        return 0;
    }

    if cand_norm == query.normalized || cand_compact == query.compact {
        return 100;
    }

    let matcher = SkimMatcherV2::default();
    let normalized = fuzzy_score(
        &matcher,
        &cand_norm,
        &query.normalized,
        query.normalized_self_score,
    );
    let compact = fuzzy_score(
        &matcher,
        &cand_compact,
        &query.compact,
        query.compact_self_score,
    );
    normalized.max(compact)
}

fn self_score(matcher: &SkimMatcherV2, pattern: &str) -> i64 {
    matcher.fuzzy_match(pattern, pattern).unwrap_or(1).max(1)
}

fn has_trailing_boundary(raw: &str) -> bool {
    raw.chars()
        .rev()
        .find(|c| !c.is_whitespace())
        .is_some_and(|c| !c.is_alphanumeric())
}

fn has_boundary_match(query: &str, normalized_candidate: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    normalized_candidate
        .match_indices(query)
        .any(|(idx, _)| is_boundary_match_at(normalized_candidate, query, idx))
}

fn is_boundary_match_at(candidate: &str, query: &str, start: usize) -> bool {
    candidate[start..].starts_with(query)
        && candidate[..start]
            .chars()
            .next_back()
            .is_none_or(|c| c == ' ')
        && candidate[start + query.len()..]
            .chars()
            .next()
            .is_none_or(|c| c == ' ')
}

fn fuzzy_score(
    matcher: &SkimMatcherV2,
    candidate: &str,
    pattern: &str,
    pattern_self_score: i64,
) -> u8 {
    matcher
        .fuzzy_match(candidate, pattern)
        .map(|score| {
            ((score.max(0) as f64 / pattern_self_score as f64) * 99.0)
                .round()
                .clamp(0.0, 99.0) as u8
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_is_100() {
        let q = Query::new("Ogna/Snåsavassdraget");
        assert_eq!(score(&q, "Ogna/Snåsavassdraget"), 100);
    }

    #[test]
    fn case_and_diacritic_fold_match() {
        let q = Query::new("ogna snasavassdraget");
        assert_eq!(score(&q, "Ogna/Snåsavassdraget"), 100);
    }

    #[test]
    fn spaced_compound_variant_matches() {
        let q = Query::new("ram berg veien 41");
        let s = score(&q, "Rambergveien 41");
        assert!(s >= 90, "expected >=90, got {s}");
    }

    #[test]
    fn partial_query_matches_longer_value() {
        let q = Query::new("ogna snasa");
        let s = score(&q, "Ogna/Snåsavassdraget");
        assert!(s >= 80, "expected >=80, got {s}");
    }

    #[test]
    fn shorter_suffix_does_not_match_longer_query() {
        let q = Query::new("Ogna/Snåsavassdraget");
        let s = score(&q, "Isavassdraget");
        assert!(s < 80, "expected <80, got {s}");
    }

    #[test]
    fn different_hierarchy_does_not_match_shared_suffix() {
        let q = Query::new("Ogna/Snåsavassdraget");
        let s = score(&q, "Heddøla-hjartdøla/Skiensvassdraget");
        assert!(s < 80, "expected <80, got {s}");
    }

    #[test]
    fn same_hierarchy_suffix_does_not_match_missing_query_token() {
        let q = Query::new("Ogna/Snåsavassdraget");
        let s = score(&q, "Forra/Snåsavassdraget");
        assert!(s < 80, "expected <80, got {s}");
    }

    #[test]
    fn boundary_match_beats_internal_suffix_match() {
        let q = Query::new("Ogna");
        let boundary = score(&q, "Ogna/Snåsavassdraget");
        let internal = score(&q, "Bogna");
        assert!(
            boundary > internal,
            "expected boundary score {boundary} > internal score {internal}"
        );
    }

    #[test]
    fn trailing_separator_requires_boundary_after_query() {
        let q = Query::new("Ogna/");
        assert!(score(&q, "Ogna/Snåsavassdraget") >= 80);
        assert_eq!(score(&q, "Ognaåni"), 0);
    }

    #[test]
    fn unrelated_text_scores_low() {
        let q = Query::new("Rambergveien 41");
        let s = score(&q, "Completely different string");
        assert!(s < 50, "expected <50, got {s}");
    }
}
