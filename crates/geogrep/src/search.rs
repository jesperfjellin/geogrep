use crate::matcher::{Query, score};
use crate::output;
use anyhow::{Context, Result};
use gdal::Dataset;
use gdal::vector::{FieldValue, LayerAccess};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::Path;

pub fn search_file(path: &Path, raw_query: &str, threshold: u8) -> Result<()> {
    let query = Query::new(raw_query);
    let dataset = Dataset::open(path).with_context(|| format!("opening {}", path.display()))?;

    for idx in 0..dataset.layer_count() {
        let mut layer = dataset
            .layer(idx)
            .with_context(|| format!("reading layer {idx} of {}", path.display()))?;
        let layer_name = layer.name();
        let mut summary = LayerSummary::new(path, &layer_name);

        let s = score(&query, &layer_name);
        if s >= threshold {
            summary.record_structural_hit(Hit::layer(s, &layer_name));
        }

        let field_names: Vec<String> = layer.defn().fields().map(|f| f.name()).collect();

        for name in &field_names {
            let s = score(&query, name);
            if s >= threshold {
                summary.record_structural_hit(Hit::field(s, name));
            }
        }

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
                if s >= threshold {
                    feature_matched = true;
                    let is_exact = query.is_exact_match(&text);
                    summary.record_value_hit(Hit::value(s, &name, &text, is_exact), is_exact);
                }
            }
            if feature_matched {
                summary.record_feature_match(fid);
            }
        }

        if let Some(result) = summary.into_result() {
            output::emit_layer_summary(&result);
        }
    }
    Ok(())
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

struct LayerSummary<'a> {
    path: &'a Path,
    layer: String,
    best: Option<Hit>,
    matched_fids: HashSet<u64>,
    anonymous_feature_matches: usize,
    exact_values: usize,
}

impl<'a> LayerSummary<'a> {
    fn new(path: &'a Path, layer: &str) -> Self {
        Self {
            path,
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

    fn into_result(self) -> Option<output::LayerSummary<'a>> {
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
