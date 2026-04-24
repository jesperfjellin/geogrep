use crate::matcher::{Query, score};
use crate::output;
use anyhow::{Context, Result, bail};
use gdal::Dataset;
use gdal::vector::{FieldValue, LayerAccess};
use ignore::WalkBuilder;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SearchOptions {
    pub threshold: u8,
    pub scopes: SearchScopes,
    pub verbose: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SearchScopes {
    pub layers: bool,
    pub columns: bool,
    pub values: bool,
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
) -> Result<Vec<output::LayerSummary>> {
    let query = Query::new(raw_query);
    let mut results = Vec::new();

    for path in paths {
        if path.is_dir() {
            results.extend(search_directory(path, &query, options));
        } else if path.is_file() {
            results.extend(search_file_with_query(path, &query, options)?);
        } else {
            bail!(
                "path does not exist or is not searchable: {}",
                path.display()
            );
        }
    }

    Ok(results)
}

fn search_directory(
    path: &Path,
    query: &Query,
    options: SearchOptions,
) -> Vec<output::LayerSummary> {
    let mut results = Vec::new();
    let mut walker = WalkBuilder::new(path);
    walker.standard_filters(false);

    for entry in walker.build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                if options.verbose {
                    eprintln!("skipping walk entry: {err}");
                }
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        match search_file_with_query(entry.path(), query, options) {
            Ok(matches) => results.extend(matches),
            Err(err) if options.verbose => {
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
) -> Result<Vec<output::LayerSummary>> {
    let dataset = Dataset::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut results = Vec::new();

    for idx in 0..dataset.layer_count() {
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
            for feature in layer.features() {
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
        }

        if let Some(result) = summary.into_result() {
            results.push(result);
        }
    }
    Ok(results)
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
