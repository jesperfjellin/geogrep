use std::cmp::Ordering;
use std::path::PathBuf;

use crate::search::SearchStats;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LayerSummary {
    pub score: u8,
    pub path: PathBuf,
    pub layer: String,
    /// True when the source layer has a non-`wkbNone` geometry type, i.e.
    /// it can produce a spatial extract that loads in QGIS as features on a
    /// map. False for tabular sources like geometry-less CSVs.
    pub is_spatial: bool,
    pub best: String,
    pub matched_features: usize,
    pub exact_values: usize,
    /// FIDs of value-matched features, retained so `--extract` can re-fetch
    /// them without rescanning. Sorted ascending for stable behavior.
    pub matched_fids: Vec<u64>,
}

pub fn emit_scan_summary(stats: &SearchStats) {
    println!(
        "Datasets found: {} ({}). Total files checked: {}.",
        stats.datasets_found,
        format_byte_size(stats.dataset_bytes),
        stats.files_checked
    );
    println!();
}

pub fn format_byte_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;
    let b = bytes as f64;
    if b >= TB {
        format!("{:.1} TB", b / TB)
    } else if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
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
            is_spatial: true,
            best: "field = value".to_owned(),
            matched_features,
            exact_values,
            matched_fids: Vec::new(),
        }
    }

    #[test]
    fn format_byte_size_auto_scales_units() {
        assert_eq!(format_byte_size(0), "0 B");
        assert_eq!(format_byte_size(512), "512 B");
        assert_eq!(format_byte_size(1024), "1.0 KB");
        assert_eq!(format_byte_size(1024 + 512), "1.5 KB");
        assert_eq!(format_byte_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_byte_size(1024 * 1024 * 1024), "1.0 GB");
        assert_eq!(format_byte_size(1024_u64.pow(4)), "1.0 TB");
        assert_eq!(format_byte_size(45_u64 * 1024_u64.pow(3) + 1024_u64.pow(3) / 3), "45.3 GB");
    }
}
