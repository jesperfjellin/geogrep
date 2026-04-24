use std::path::Path;

pub struct LayerSummary<'a> {
    pub score: u8,
    pub path: &'a Path,
    pub layer: String,
    pub best: String,
    pub matched_features: usize,
    pub exact_values: usize,
}

pub fn emit_layer_summary(summary: &LayerSummary<'_>) {
    println!("{:>3}  {}", summary.score, summary.path.display());
    println!("     layer: {}", summary.layer);
    println!("     best: {}", summary.best);
    println!(
        "     matches: {} features, {} exact values",
        summary.matched_features, summary.exact_values
    );
    println!();
}
