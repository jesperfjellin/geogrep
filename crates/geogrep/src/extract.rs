use anyhow::{Context, Result, bail};
use gdal::{Dataset, DriverManager};
use gdal::vector::{Feature, LayerAccess, LayerOptions, OGRFieldType};
use std::collections::HashSet;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use crate::output::LayerSummary;

const SIZE_WARNING_THRESHOLD_BYTES: u64 = 100 * 1024 * 1024;
const MB: f64 = 1024.0 * 1024.0;
const OUTPUT_DRIVER: &str = "FlatGeobuf";
const OUTPUT_EXTENSION: &str = "fgb";

pub struct Extraction {
    pub output_path: PathBuf,
    pub features_written: usize,
}

pub fn extract_dominant(summaries: &[LayerSummary], raw_query: &str) -> Result<Option<Extraction>> {
    let Some((target, fell_back_to_tabular)) = pick_dominant(summaries) else {
        eprintln!("--extract: nothing to extract — no value matches above threshold.");
        return Ok(None);
    };

    if fell_back_to_tabular {
        eprintln!(
            "--extract: no spatial sources matched — extracting from tabular dataset; \
             output has no geometry or CRS."
        );
    }

    let dir = extract_dir()?;
    if !ensure_extract_dir(&dir)? {
        eprintln!("--extract: aborted by user.");
        return Ok(None);
    }

    let estimate = estimate_output_size(target)?;
    let output_path = output_path_for(target, raw_query, &dir);

    if estimate > SIZE_WARNING_THRESHOLD_BYTES && !confirm_large_extract(target, estimate)? {
        eprintln!("--extract: aborted by user.");
        return Ok(None);
    }

    let written = write_extract(target, &output_path)?;
    Ok(Some(Extraction {
        output_path,
        features_written: written,
    }))
}

/// Resolves the path geogrep writes extracts to. Returns `None` when
/// `$HOME` is unset, in which case `--extract` cannot run and recursive
/// searches have no extracts directory to avoid revisiting.
pub(crate) fn extracts_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join("geogrep").join("extracts"))
}

fn extract_dir() -> Result<PathBuf> {
    extracts_directory().ok_or_else(|| {
        anyhow::anyhow!("--extract: $HOME is not set; cannot locate output directory")
    })
}

/// Returns Ok(true) if the directory exists or was just created with the
/// user's consent, Ok(false) if the user declined creation.
fn ensure_extract_dir(dir: &Path) -> Result<bool> {
    if dir.is_dir() {
        return Ok(true);
    }
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        bail!(
            "--extract: output directory {} does not exist; rerun interactively to confirm \
             creation",
            dir.display()
        );
    }
    eprint!("--extract: create output directory {}? [y/N] ", dir.display());
    io::stderr().flush().ok();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    if !matches!(line.trim(), "y" | "Y" | "yes" | "Yes" | "YES") {
        return Ok(false);
    }
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(true)
}

fn pick_dominant(summaries: &[LayerSummary]) -> Option<(&LayerSummary, bool)> {
    if let Some(target) = best_extractable(summaries, |s| s.is_spatial) {
        return Some((target, false));
    }
    best_extractable(summaries, |s| !s.is_spatial).map(|t| (t, true))
}

fn best_extractable(
    summaries: &[LayerSummary],
    pred: impl Fn(&LayerSummary) -> bool,
) -> Option<&LayerSummary> {
    summaries
        .iter()
        .filter(|s| !s.matched_fids.is_empty() && pred(s))
        .max_by(|a, b| {
            a.matched_fids
                .len()
                .cmp(&b.matched_fids.len())
                .then_with(|| a.score.cmp(&b.score))
                .then_with(|| b.path.cmp(&a.path))
                .then_with(|| b.layer.cmp(&a.layer))
        })
}

fn estimate_output_size(summary: &LayerSummary) -> Result<u64> {
    let file_size = std::fs::metadata(&summary.path)
        .with_context(|| format!("reading metadata for {}", summary.path.display()))?
        .len();
    let dataset = Dataset::open(&summary.path)
        .with_context(|| format!("opening {} to count features", summary.path.display()))?;
    let mut total: u64 = 0;
    for idx in 0..dataset.layer_count() {
        if let Ok(layer) = dataset.layer(idx) {
            total = total.saturating_add(layer.feature_count());
        }
    }
    if total == 0 {
        return Ok(0);
    }
    let matched = summary.matched_fids.len() as u128;
    let estimate = (matched.saturating_mul(file_size as u128)) / total as u128;
    Ok(estimate.min(u64::MAX as u128) as u64)
}

fn confirm_large_extract(summary: &LayerSummary, estimate: u64) -> Result<bool> {
    let stdin = io::stdin();
    let estimate_mb = estimate as f64 / MB;
    if !stdin.is_terminal() {
        bail!(
            "--extract: estimated output ~{estimate_mb:.1} MB exceeds 100 MB and stdin is not a \
             terminal; rerun interactively to confirm or narrow the search first"
        );
    }
    eprint!(
        "--extract: writing matched features from {} layer {} would create roughly \
         {estimate_mb:.1} MB. Continue? [y/N] ",
        summary.path.display(),
        summary.layer,
    );
    io::stderr().flush().ok();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes" | "YES"))
}

fn output_path_for(summary: &LayerSummary, raw_query: &str, dir: &Path) -> PathBuf {
    let stem = summary
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("dataset");
    let q = sanitize_query(raw_query);
    dir.join(format!("{stem}.geogrep.{q}.{OUTPUT_EXTENSION}"))
}

fn sanitize_query(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        return "query".to_owned();
    }
    out.truncate(32);
    while out.ends_with('_') {
        out.pop();
    }
    out
}

fn write_extract(summary: &LayerSummary, output_path: &Path) -> Result<usize> {
    let input = Dataset::open(&summary.path)
        .with_context(|| format!("opening {}", summary.path.display()))?;

    let mut in_layer = input
        .layer_by_name(&summary.layer)
        .with_context(|| format!("opening layer {} in {}", summary.layer, summary.path.display()))?;

    let geom_type = in_layer.defn().geometry_type();
    let srs = in_layer.spatial_ref();

    let field_specs: Vec<(String, OGRFieldType::Type)> = in_layer
        .defn()
        .fields()
        .map(|f| (f.name(), f.field_type()))
        .collect();

    if output_path.exists() {
        std::fs::remove_file(output_path)
            .with_context(|| format!("removing existing {}", output_path.display()))?;
    }

    let driver = DriverManager::get_driver_by_name(OUTPUT_DRIVER)
        .with_context(|| format!("driver {OUTPUT_DRIVER} unavailable for create"))?;
    let mut output = driver
        .create_vector_only(output_path)
        .with_context(|| format!("creating output dataset {}", output_path.display()))?;

    let layer_name = summary.layer.clone();
    let out_layer = output
        .create_layer(LayerOptions {
            name: &layer_name,
            srs: srs.as_ref(),
            ty: geom_type,
            options: None,
        })
        .with_context(|| {
            format!(
                "creating layer {layer_name} in {}",
                output_path.display()
            )
        })?;

    let field_refs: Vec<(&str, OGRFieldType::Type)> = field_specs
        .iter()
        .map(|(name, ty)| (name.as_str(), *ty))
        .collect();
    out_layer
        .create_defn_fields(&field_refs)
        .with_context(|| format!("defining fields in {}", output_path.display()))?;

    let target_fids: HashSet<u64> = summary.matched_fids.iter().copied().collect();
    let field_count = field_specs.len();
    let mut written = 0usize;

    for in_feature in in_layer.features() {
        let Some(fid) = in_feature.fid() else { continue };
        if !target_fids.contains(&fid) {
            continue;
        }
        let mut out_feature = Feature::new(out_layer.defn())
            .context("allocating output feature")?;
        for idx in 0..field_count {
            if let Some(value) = in_feature
                .field(idx)
                .with_context(|| format!("reading field {idx} of fid {fid}"))?
            {
                out_feature
                    .set_field(idx, &value)
                    .with_context(|| format!("writing field {idx} of fid {fid}"))?;
            }
        }
        if let Some(g) = in_feature.geometry() {
            out_feature
                .set_geometry(g.clone())
                .with_context(|| format!("writing geometry of fid {fid}"))?;
        }
        out_feature
            .create(&out_layer)
            .with_context(|| format!("persisting fid {fid} to output"))?;
        written += 1;
    }

    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spatial(path: &str, layer: &str, score: u8, fids: &[u64]) -> LayerSummary {
        summary_with(path, layer, score, fids, true)
    }

    fn tabular(path: &str, layer: &str, score: u8, fids: &[u64]) -> LayerSummary {
        summary_with(path, layer, score, fids, false)
    }

    fn summary_with(
        path: &str,
        layer: &str,
        score: u8,
        fids: &[u64],
        is_spatial: bool,
    ) -> LayerSummary {
        LayerSummary {
            score,
            path: path.into(),
            layer: layer.into(),
            is_spatial,
            best: "f = v".into(),
            matched_features: fids.len(),
            exact_values: 0,
            matched_fids: fids.to_vec(),
        }
    }

    #[test]
    fn dominant_pick_prefers_more_fids_then_higher_score() {
        let s = vec![
            spatial("a.gpkg", "l", 95, &[1, 2]),
            spatial("b.gpkg", "l", 85, &[1, 2, 3, 4]),
            spatial("c.gpkg", "l", 99, &[1]),
        ];
        let (chosen, fell_back) = pick_dominant(&s).unwrap();
        assert_eq!(chosen.path.to_str().unwrap(), "b.gpkg");
        assert!(!fell_back);
    }

    #[test]
    fn dominant_skips_summaries_with_no_extractable_fids() {
        let s = vec![
            spatial("a.gpkg", "l", 100, &[]),
            spatial("b.gpkg", "l", 80, &[1]),
        ];
        let (chosen, fell_back) = pick_dominant(&s).unwrap();
        assert_eq!(chosen.path.to_str().unwrap(), "b.gpkg");
        assert!(!fell_back);
    }

    #[test]
    fn dominant_returns_none_when_only_structural_hits() {
        let s = vec![spatial("a.gpkg", "l", 100, &[])];
        assert!(pick_dominant(&s).is_none());
    }

    #[test]
    fn dominant_prefers_spatial_over_higher_fid_count_tabular() {
        let s = vec![
            tabular("companies.csv", "rows", 99, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]),
            spatial("roads.gpkg", "lines", 100, &[1, 2]),
        ];
        let (chosen, fell_back) = pick_dominant(&s).unwrap();
        assert_eq!(chosen.path.to_str().unwrap(), "roads.gpkg");
        assert!(!fell_back);
    }

    #[test]
    fn dominant_falls_back_to_tabular_when_no_spatial_matches() {
        let s = vec![
            tabular("companies.csv", "rows", 99, &[1, 2, 3]),
            spatial("roads.gpkg", "lines", 100, &[]),
        ];
        let (chosen, fell_back) = pick_dominant(&s).unwrap();
        assert_eq!(chosen.path.to_str().unwrap(), "companies.csv");
        assert!(fell_back);
    }

    #[test]
    fn output_path_always_uses_fgb_regardless_of_input_format() {
        for path in [
            "/data/export_2022.gpkg",
            "/data/roads.shp",
            "/data/places.geojson",
            "/data/norway.osm.pbf",
        ] {
            let s = spatial(path, "layer", 90, &[1]);
            let out = output_path_for(&s, "Rambergveien 41", Path::new("/home/u/geogrep/extracts"));
            assert!(
                out.to_str().unwrap().ends_with(".fgb"),
                "expected .fgb output for {path}, got {}",
                out.display()
            );
        }
    }

    #[test]
    fn output_path_uses_stem_and_query_in_filename() {
        let s = spatial("/data/export_2022.gpkg", "vegadresse", 90, &[1]);
        let path = output_path_for(&s, "Rambergveien 41", Path::new("/home/u/geogrep/extracts"));
        assert_eq!(
            path.to_str().unwrap(),
            "/home/u/geogrep/extracts/export_2022.geogrep.rambergveien_41.fgb"
        );
    }

    #[test]
    fn sanitize_query_strips_punctuation_and_lowercases() {
        assert_eq!(sanitize_query("Rambergveien 41"), "rambergveien_41");
        assert_eq!(sanitize_query("  --weird!! input!! "), "weird_input");
        assert_eq!(sanitize_query("!!!"), "query");
    }

    #[test]
    fn sanitize_query_truncates_long_input() {
        let long = "a".repeat(200);
        let s = sanitize_query(&long);
        assert!(s.len() <= 32);
    }
}
