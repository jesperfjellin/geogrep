use crate::matcher::{Query, score};
use crate::output;
use anyhow::{Context, Result, bail};
use gdal::Dataset;
use gdal::vector::{FieldValue, LayerAccess};
use ignore::WalkBuilder;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const PROGRESS_DRAW_INTERVAL: Duration = Duration::from_millis(250);
const LARGE_DATASET_NOTICE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SearchOptions {
    pub threshold: u8,
    pub scopes: SearchScopes,
    pub size_limit_bytes: Option<u64>,
    pub verbose: bool,
    pub progress: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SearchScopes {
    pub layers: bool,
    pub columns: bool,
    pub values: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SearchResult {
    pub summaries: Vec<output::LayerSummary>,
    pub stats: SearchStats,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct SearchStats {
    pub datasets_found: usize,
    pub files_checked: usize,
}

impl SearchScopes {
    pub fn from_flags(layers: bool, columns: bool, values: bool) -> Self {
        if layers || columns || values {
            Self {
                layers,
                columns,
                values,
            }
        } else {
            Self::all()
        }
    }

    fn all() -> Self {
        Self {
            layers: true,
            columns: true,
            values: true,
        }
    }
}

pub fn search_paths(
    paths: &[PathBuf],
    raw_query: &str,
    options: SearchOptions,
) -> Result<SearchResult> {
    let query = Query::new(raw_query);
    let mut results = Vec::new();
    let mut progress = Progress::new(options.progress);

    for path in paths {
        if path.is_dir() {
            results.extend(search_directory(path, &query, options, &mut progress));
        } else if path.is_file() {
            results.extend(search_file_with_query(
                path,
                &query,
                options,
                &mut progress,
            )?);
        } else {
            bail!(
                "path does not exist or is not searchable: {}",
                path.display()
            );
        }
    }

    progress.finish();
    Ok(SearchResult {
        summaries: results,
        stats: progress.stats(),
    })
}

fn search_directory(
    path: &Path,
    query: &Query,
    options: SearchOptions,
    progress: &mut Progress,
) -> Vec<output::LayerSummary> {
    let mut results = Vec::new();
    let mut walker = WalkBuilder::new(path);
    walker.standard_filters(false);
    walker.filter_entry(|entry| !is_hidden_dir(entry));

    for entry in walker.build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                if options.verbose {
                    progress.clear_line();
                    eprintln!("skipping walk entry: {err}");
                }
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        match search_file_with_query(entry.path(), query, options, progress) {
            Ok(matches) => results.extend(matches),
            Err(err) if options.verbose => {
                progress.clear_line();
                eprintln!("skipping {}: {err:#}", entry.path().display());
            }
            Err(_) => {}
        }
    }

    results
}

fn search_file_with_query(
    path: &Path,
    query: &Query,
    options: SearchOptions,
    progress: &mut Progress,
) -> Result<Vec<output::LayerSummary>> {
    progress.record_file(path);
    let file_size = file_size_bytes(path)?;
    if file_exceeds_size_limit(file_size, options.size_limit_bytes) {
        if options.verbose {
            progress.clear_line();
            eprintln!("skipping {}: file exceeds --sizelimit", path.display());
        }
        return Ok(Vec::new());
    }

    if file_size >= LARGE_DATASET_NOTICE_BYTES {
        progress.record_large_dataset_open(file_size, path);
    }
    let dataset = Dataset::open(path).with_context(|| format!("opening {}", path.display()))?;
    if file_size >= LARGE_DATASET_NOTICE_BYTES {
        progress.record_large_dataset_scan(file_size, path);
    }
    let mut results = Vec::new();
    let layer_count = dataset.layer_count();
    if layer_count == 0 {
        progress.clear_large_dataset_status(path);
        return Ok(results);
    }

    progress.record_dataset(path);

    for idx in 0..layer_count {
        let mut layer = dataset
            .layer(idx)
            .with_context(|| format!("reading layer {idx} of {}", path.display()))?;
        let layer_name = layer.name();
        let mut summary = LayerSummary::new(path, &layer_name);

        if options.scopes.layers {
            let s = score(&query, &layer_name);
            if s >= options.threshold {
                summary.record_structural_hit(Hit::layer(s, &layer_name));
            }
        }

        if options.scopes.columns {
            for field in layer.defn().fields() {
                let name = field.name();
                let s = score(&query, &name);
                if s >= options.threshold {
                    summary.record_structural_hit(Hit::field(s, &name));
                }
            }
        }

        if options.scopes.values {
            let mut feature_progress = 0;
            for feature in layer.features() {
                feature_progress += 1;
                if feature_progress >= 1000 {
                    progress.record_features(feature_progress, path);
                    feature_progress = 0;
                }

                let fid = feature.fid();
                let mut feature_matched = false;
                for (name, value) in feature.fields() {
                    let Some(fv) = value else { continue };
                    let text = match field_value_to_text(&fv) {
                        Some(t) if !t.is_empty() => t,
                        _ => continue,
                    };
                    let s = score(&query, &text);
                    if s >= options.threshold {
                        feature_matched = true;
                        let is_exact = query.is_exact_match(&text);
                        summary.record_value_hit(Hit::value(s, &name, &text, is_exact), is_exact);
                    }
                }
                if feature_matched {
                    summary.record_feature_match(fid);
                }
            }
            if feature_progress > 0 {
                progress.record_features(feature_progress, path);
            }
        }

        if let Some(result) = summary.into_result() {
            results.push(result);
        }
    }
    progress.record_matching_layers(results.len(), path);
    progress.clear_large_dataset_status(path);
    Ok(results)
}

fn file_size_bytes(path: &Path) -> Result<u64> {
    Ok(path
        .metadata()
        .with_context(|| format!("reading metadata for {}", path.display()))?
        .len())
}

fn file_exceeds_size_limit(file_size_bytes: u64, size_limit_bytes: Option<u64>) -> bool {
    let Some(limit) = size_limit_bytes else {
        return false;
    };
    file_size_bytes > limit
}

fn field_value_to_text(fv: &FieldValue) -> Option<String> {
    use FieldValue::*;
    match fv {
        StringValue(s) => Some(s.clone()),
        IntegerValue(i) => Some(i.to_string()),
        Integer64Value(i) => Some(i.to_string()),
        RealValue(f) => Some(f.to_string()),
        DateValue(d) => Some(d.to_string()),
        DateTimeValue(d) => Some(d.to_string()),
        _ => None,
    }
}

fn is_hidden_dir(entry: &ignore::DirEntry) -> bool {
    entry.depth() > 0
        && entry.file_type().is_some_and(|ft| ft.is_dir())
        && entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with('.'))
}

struct Progress {
    enabled: bool,
    stats: SearchStats,
    large_dataset_status: Option<String>,
    last_draw: Instant,
    line_len: usize,
    finished: bool,
}

impl Progress {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            stats: SearchStats::default(),
            large_dataset_status: None,
            last_draw: Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
            line_len: 0,
            finished: false,
        }
    }

    fn record_file(&mut self, path: &Path) {
        self.stats.files_checked += 1;
        self.draw_throttled(path);
    }

    fn record_dataset(&mut self, path: &Path) {
        self.stats.datasets_found += 1;
        self.draw_throttled(path);
    }

    fn record_large_dataset_open(&mut self, file_size: u64, path: &Path) {
        self.large_dataset_status = Some(format_large_dataset_status("Opening", file_size));
        self.draw(path);
    }

    fn record_large_dataset_scan(&mut self, file_size: u64, path: &Path) {
        self.large_dataset_status = Some(format_large_dataset_status("Scanning", file_size));
        self.draw(path);
    }

    fn clear_large_dataset_status(&mut self, path: &Path) {
        if self.large_dataset_status.take().is_some() {
            self.draw(path);
        }
    }

    fn record_features(&mut self, count: usize, path: &Path) {
        let _ = count;
        self.draw_throttled(path);
    }

    fn record_matching_layers(&mut self, count: usize, path: &Path) {
        let _ = count;
        self.draw_throttled(path);
    }

    fn finish(&mut self) {
        self.finished = true;
        self.clear_line();
    }

    fn stats(&self) -> SearchStats {
        self.stats
    }

    fn clear_line(&mut self) {
        if !self.enabled || self.line_len == 0 {
            return;
        }
        eprint!("\r\x1b[2K");
        let _ = io::stderr().flush();
        self.line_len = 0;
    }

    fn draw_throttled(&mut self, path: &Path) {
        if self.last_draw.elapsed() >= PROGRESS_DRAW_INTERVAL {
            self.draw(path);
        }
    }

    fn draw(&mut self, path: &Path) {
        if !self.enabled {
            return;
        }

        self.last_draw = Instant::now();
        let _ = path;
        let line = format!(
            "Datasets found: {}. Total files checked: {}",
            self.stats.datasets_found, self.stats.files_checked
        );
        let line = match &self.large_dataset_status {
            Some(status) => format!("{line}. {status}"),
            None => line,
        };
        let line_len = line.chars().count();
        eprint!("\r\x1b[2K{line}");
        let _ = io::stderr().flush();
        self.line_len = line_len;
    }
}

fn format_large_dataset_status(action: &str, file_size: u64) -> String {
    let gb = file_size as f64 / LARGE_DATASET_NOTICE_BYTES as f64;
    format!("{action} {gb:.1}GB dataset.")
}

impl Drop for Progress {
    fn drop(&mut self) {
        if !self.finished {
            self.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scope_searches_all_surfaces() {
        assert_eq!(
            SearchScopes::from_flags(false, false, false),
            SearchScopes {
                layers: true,
                columns: true,
                values: true
            }
        );
    }

    #[test]
    fn explicit_scope_flags_restrict_search_surfaces() {
        assert_eq!(
            SearchScopes::from_flags(false, true, false),
            SearchScopes {
                layers: false,
                columns: true,
                values: false
            }
        );
    }

    #[test]
    fn size_limit_skips_files_above_limit() {
        assert!(file_exceeds_size_limit(2, Some(1)));
    }

    #[test]
    fn missing_size_limit_does_not_skip_files() {
        assert!(!file_exceeds_size_limit(2, None));
    }

    #[test]
    fn formats_large_dataset_status_in_gb() {
        let one_and_a_half_gb = LARGE_DATASET_NOTICE_BYTES + LARGE_DATASET_NOTICE_BYTES / 2;
        assert_eq!(
            format_large_dataset_status("Opening", one_and_a_half_gb),
            "Opening 1.5GB dataset."
        );
        assert_eq!(
            format_large_dataset_status("Scanning", one_and_a_half_gb),
            "Scanning 1.5GB dataset."
        );
    }
}

struct LayerSummary {
    path: PathBuf,
    layer: String,
    best: Option<Hit>,
    matched_fids: HashSet<u64>,
    anonymous_feature_matches: usize,
    exact_values: usize,
}

impl LayerSummary {
    fn new(path: &Path, layer: &str) -> Self {
        Self {
            path: path.to_path_buf(),
            layer: layer.to_owned(),
            best: None,
            matched_fids: HashSet::new(),
            anonymous_feature_matches: 0,
            exact_values: 0,
        }
    }

    fn record_structural_hit(&mut self, hit: Hit) {
        self.record_best(hit);
    }

    fn record_value_hit(&mut self, hit: Hit, is_exact_value: bool) {
        if is_exact_value {
            self.exact_values += 1;
        }
        self.record_best(hit);
    }

    fn record_feature_match(&mut self, fid: Option<u64>) {
        if let Some(fid) = fid {
            self.matched_fids.insert(fid);
        } else {
            self.anonymous_feature_matches += 1;
        }
    }

    fn record_best(&mut self, hit: Hit) {
        if self
            .best
            .as_ref()
            .is_none_or(|best| hit.cmp_quality(best) == Ordering::Greater)
        {
            self.best = Some(hit);
        }
    }

    fn into_result(self) -> Option<output::LayerSummary> {
        let best = self.best?;
        Some(output::LayerSummary {
            score: best.score,
            path: self.path,
            layer: self.layer,
            best: best.label,
            matched_features: self.matched_fids.len() + self.anonymous_feature_matches,
            exact_values: self.exact_values,
        })
    }
}

struct Hit {
    score: u8,
    label: String,
    exact_value: bool,
    value_len: usize,
}

impl Hit {
    fn layer(score: u8, layer: &str) -> Self {
        Self {
            score,
            label: format!("layer = {layer}"),
            exact_value: false,
            value_len: layer.chars().count(),
        }
    }

    fn field(score: u8, field: &str) -> Self {
        Self {
            score,
            label: format!("field = {field}"),
            exact_value: false,
            value_len: field.chars().count(),
        }
    }

    fn value(score: u8, field: &str, value: &str, exact_value: bool) -> Self {
        Self {
            score,
            label: format!("{field} = {value}"),
            exact_value,
            value_len: value.chars().count(),
        }
    }

    fn cmp_quality(&self, other: &Self) -> Ordering {
        self.score
            .cmp(&other.score)
            .then_with(|| self.exact_value.cmp(&other.exact_value))
            .then_with(|| other.value_len.cmp(&self.value_len))
    }
}
