use std::cmp::Ordering;
use std::path::PathBuf;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LayerSummary {
    pub score: u8,
    pub path: PathBuf,
    pub layer: String,
    pub best: String,
    pub matched_features: usize,
    pub exact_values: usize,
}

pub fn rank_summaries(summaries: &mut [LayerSummary]) {
    summaries.sort_by(cmp_summary_rank);
}

fn cmp_summary_rank(a: &LayerSummary, b: &LayerSummary) -> Ordering {
    b.score
        .cmp(&a.score)
        .then_with(|| b.exact_values.cmp(&a.exact_values))
        .then_with(|| b.matched_features.cmp(&a.matched_features))
        .then_with(|| a.path.cmp(&b.path))
        .then_with(|| a.layer.cmp(&b.layer))
        .then_with(|| a.best.cmp(&b.best))
}

pub fn emit_layer_summary(summary: &LayerSummary) {
    println!("{:>3}  {}", summary.score, summary.path.display());
    println!("     layer: {}", summary.layer);
    println!("     best: {}", summary.best);
    println!(
        "     matches: {} features, {} exact values",
        summary.matched_features, summary.exact_values
    );
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_by_score_exact_matches_match_count_then_path() {
        let mut summaries = vec![
            summary(99, 10, 100, "b.fgb"),
            summary(100, 1, 1, "c.fgb"),
            summary(100, 5, 10, "b.fgb"),
            summary(100, 5, 20, "a.fgb"),
        ];

        rank_summaries(&mut summaries);

        let paths: Vec<_> = summaries
            .iter()
            .map(|summary| summary.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, ["a.fgb", "b.fgb", "c.fgb", "b.fgb"]);
    }

    fn summary(
        score: u8,
        exact_values: usize,
        matched_features: usize,
        path: &str,
    ) -> LayerSummary {
        LayerSummary {
            score,
            path: path.into(),
            layer: "layer".to_owned(),
            best: "field = value".to_owned(),
            matched_features,
            exact_values,
        }
    }
}
