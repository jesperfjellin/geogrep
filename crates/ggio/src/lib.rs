//! Streaming reader over vector geospatial datasets.
//!
//! v0: a single `describe` smoke-test function that opens a dataset via GDAL
//! and prints its layers, fields, and a handful of features. The real
//! `Reader` trait will be extracted once we've validated the GDAL surface.

use anyhow::{Context, Result};
use gdal::Dataset;
use gdal::vector::LayerAccess;
use std::path::Path;

pub fn describe(path: &Path) -> Result<()> {
    let dataset = Dataset::open(path).with_context(|| format!("opening {}", path.display()))?;

    let driver = dataset.driver();
    println!("path:   {}", path.display());
    println!("driver: {} ({})", driver.long_name(), driver.short_name());
    println!("layers: {}", dataset.layer_count());

    for idx in 0..dataset.layer_count() {
        let mut layer = dataset
            .layer(idx)
            .with_context(|| format!("reading layer {idx}"))?;

        println!();
        println!("  [{idx}] layer: {}", layer.name());

        let defn = layer.defn();
        for field in defn.fields() {
            println!(
                "      field: {:<24} type={:?}",
                field.name(),
                field.field_type()
            );
        }

        for (i, feature) in layer.features().enumerate() {
            if i >= 3 {
                println!("      ... (truncated)");
                break;
            }
            println!("      feature fid={:?}", feature.fid());
            for (name, value) in feature.fields() {
                println!("        {name}: {value:?}");
            }
        }
    }

    Ok(())
}
