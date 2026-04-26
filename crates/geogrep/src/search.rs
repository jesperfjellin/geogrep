use crate::extract;
use crate::matcher::{Query, score};
use crate::output;
use anyhow::{Context, Result, bail};
use gdal::Dataset;
use gdal::vector::{FieldValue, LayerAccess, OGRwkbGeometryType};
use ignore::WalkBuilder;
use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PROGRESS_DRAW_INTERVAL: Duration = Duration::from_millis(250);
const LARGE_DATASET_NOTICE_BYTES: u64 = 1024 * 1024 * 1024;
type SharedProgress = Arc<Mutex<Progress>>;

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
    /// Sum of file sizes for datasets that GDAL successfully opened (and that
    /// had at least one layer). Counts only the primary file path opened —
    /// for shapefiles, this is the `.shp`, not summed sidecars — to stay
    /// consistent with the size reported to `--sizelimit`.
    pub dataset_bytes: u64,
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
    let progress = Arc::new(Mutex::new(Progress::new(options.progress)));

    for path in paths {
        if path.is_dir() {
            results.extend(search_directory(
                path,
                &query,
                options,
                Arc::clone(&progress),
            ));
        } else if path.is_file() {
            match search_file_with_query(path, &query, options, &progress) {
                Ok(matches) => results.extend(matches),
                Err(err) => {
                    finish_progress(&progress);
                    return Err(err);
                }
            }
        } else {
            finish_progress(&progress);
            bail!(
                "path does not exist or is not searchable: {}",
                path.display()
            );
        }
    }

    let stats = finish_progress(&progress);
    Ok(SearchResult {
        summaries: results,
        stats,
    })
}

fn search_directory(
    path: &Path,
    query: &Query,
    options: SearchOptions,
    progress: SharedProgress,
) -> Vec<output::LayerSummary> {
    let extracts_dir_canon = extract::extracts_directory().and_then(|p| p.canonicalize().ok());
    let mut walker = WalkBuilder::new(path);
    walker.standard_filters(false);
    walker.filter_entry(move |entry| {
        !is_hidden_dir(entry) && !is_geogrep_extracts_dir(entry, extracts_dir_canon.as_deref())
    });

    walker
        .build()
        .par_bridge()
        .filter_map(|entry| {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    if options.verbose {
                        with_progress(&progress, |progress| progress.clear_line());
                        eprintln!("skipping walk entry: {err}");
                    }
                    return None;
                }
            };
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return None;
            }

            match search_file_with_query(entry.path(), query, options, &progress) {
                Ok(matches) => Some(matches),
                Err(err) if options.verbose => {
                    with_progress(&progress, |progress| progress.clear_line());
                    eprintln!("skipping {}: {err:#}", entry.path().display());
                    None
                }
                Err(_) => None,
            }
        })
        .flatten()
        .collect()
}

fn search_file_with_query(
    path: &Path,
    query: &Query,
    options: SearchOptions,
    progress: &SharedProgress,
) -> Result<Vec<output::LayerSummary>> {
    with_progress(progress, |progress| progress.record_file(path));
    if should_skip_vector_probe(path) {
        if options.verbose {
            with_progress(progress, |progress| progress.clear_line());
            eprintln!(
                "skipping {}: non-vector file extension",
                path.display()
            );
        }
        return Ok(Vec::new());
    }

    let file_size = file_size_bytes(path)?;
    if file_exceeds_size_limit(file_size, options.size_limit_bytes) {
        if options.verbose {
            with_progress(progress, |progress| progress.clear_line());
            eprintln!("skipping {}: file exceeds --sizelimit", path.display());
        }
        return Ok(Vec::new());
    }

    if should_skip_json_by_content(path) {
        if options.verbose {
            with_progress(progress, |progress| progress.clear_line());
            eprintln!(
                "skipping {}: .json without GeoJSON/TopoJSON markers",
                path.display()
            );
        }
        return Ok(Vec::new());
    }

    if file_size >= LARGE_DATASET_NOTICE_BYTES {
        with_progress(progress, |progress| {
            progress.record_large_dataset_open(file_size, path)
        });
    }
    let dataset = Dataset::open(path).with_context(|| format!("opening {}", path.display()))?;
    if file_size >= LARGE_DATASET_NOTICE_BYTES {
        with_progress(progress, |progress| {
            progress.record_large_dataset_scan(file_size, path)
        });
    }
    let mut results = Vec::new();
    let layer_count = dataset.layer_count();
    if layer_count == 0 {
        with_progress(progress, |progress| {
            progress.clear_large_dataset_status(path)
        });
        return Ok(results);
    }

    with_progress(progress, |progress| progress.record_dataset(path, file_size));

    for idx in 0..layer_count {
        let mut layer = dataset
            .layer(idx)
            .with_context(|| format!("reading layer {idx} of {}", path.display()))?;
        let layer_name = layer.name();
        let is_spatial = layer.defn().geometry_type() != OGRwkbGeometryType::wkbNone;
        let mut summary = LayerSummary::new(path, &layer_name, is_spatial);

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
                    with_progress(progress, |progress| {
                        progress.record_features(feature_progress, path)
                    });
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
                with_progress(progress, |progress| {
                    progress.record_features(feature_progress, path)
                });
            }
        }

        if let Some(result) = summary.into_result() {
            results.push(result);
        }
    }
    with_progress(progress, |progress| {
        progress.record_matching_layers(results.len(), path);
        progress.clear_large_dataset_status(path);
    });
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

fn should_skip_vector_probe(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };

    // `.zip` is intentionally absent so zipped shapefiles still probe via GDAL.
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "h5" | "hdf" | "hdf5" | "he5" | "nc" | "nc4"
        | "tif" | "tiff" | "png" | "jpg" | "jpeg" | "gif" | "bmp"
        | "ico" | "webp" | "heic" | "heif" | "jp2" | "j2k"
        | "mp3" | "wav" | "flac" | "ogg" | "oga" | "m4a" | "opus" | "aac" | "wma"
        | "mp4" | "m4v" | "mov" | "avi" | "mkv" | "webm" | "wmv"
        | "flv" | "mpg" | "mpeg" | "3gp"
        | "tar" | "gz" | "tgz" | "bz2" | "tbz2" | "xz" | "txz"
        | "lz" | "lzma" | "zst" | "7z" | "rar"
        | "deb" | "rpm" | "dmg" | "iso" | "cab" | "apk" | "aab"
        | "rs" | "py" | "pyc" | "pyo" | "pyd" | "pyi"
        | "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx"
        | "go" | "c" | "cpp" | "cc" | "cxx" | "cs"
        | "h" | "hpp" | "hxx"
        | "java" | "kt" | "kts" | "scala" | "swift"
        | "rb" | "php" | "pl" | "pm"
        | "sh" | "bash" | "zsh" | "fish" | "lua"
        | "o" | "a" | "so" | "dylib" | "dll" | "exe" | "obj" | "pdb"
        | "class" | "jar" | "war" | "ear"
        | "md" | "rst" | "tex" | "bib"
        | "html" | "htm" | "css" | "scss" | "sass" | "less"
        | "doc" | "docx" | "ppt" | "pptx"
        | "ttf" | "otf" | "woff" | "woff2" | "eot"
        | "yaml" | "yml" | "toml" | "lock" | "log"
    )
}

/// Cheap content sniff for `.json` files. node_modules-heavy walks have
/// hundreds of thousands of plain JSON files (package.json, tsconfig.json,
/// lockfiles, etc.) that GDAL would otherwise probe driver-by-driver. We
/// can rule them out by reading the first 8 KiB and checking for any
/// GeoJSON or TopoJSON quoted-string marker.
fn should_skip_json_by_content(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    if !ext.eq_ignore_ascii_case("json") {
        return false;
    }

    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 8192];
    let Ok(n) = file.read(&mut buf) else {
        return false;
    };
    !looks_like_geojson_head(&buf[..n])
}

fn looks_like_geojson_head(head: &[u8]) -> bool {
    const SIGNATURES: [&[u8]; 7] = [
        b"\"coordinates\"",
        b"\"features\"",
        b"\"geometries\"",
        b"\"Feature\"",
        b"\"FeatureCollection\"",
        b"\"Topology\"",
        b"\"arcs\"",
    ];
    SIGNATURES.iter().any(|sig| contains_subseq(head, sig))
}

fn contains_subseq(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
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

/// True when `entry` is the geogrep extracts directory encountered during a
/// recursive walk (depth > 0). Lets explicit `gg "..." ~/geogrep/extracts`
/// invocations still descend, since those start the walk with depth == 0.
fn is_geogrep_extracts_dir(entry: &ignore::DirEntry, skip_canon: Option<&Path>) -> bool {
    if entry.depth() == 0 {
        return false;
    }
    let Some(skip) = skip_canon else {
        return false;
    };
    if !entry.file_type().is_some_and(|ft| ft.is_dir()) {
        return false;
    }
    if entry.file_name() != std::ffi::OsStr::new("extracts") {
        return false;
    }
    entry.path().canonicalize().is_ok_and(|c| c == skip)
}

struct Progress {
    enabled: bool,
    stats: SearchStats,
    large_dataset_status: Option<(PathBuf, String)>,
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

    fn record_dataset(&mut self, path: &Path, file_size: u64) {
        self.stats.datasets_found += 1;
        self.stats.dataset_bytes = self.stats.dataset_bytes.saturating_add(file_size);
        self.draw_throttled(path);
    }

    fn record_large_dataset_open(&mut self, file_size: u64, path: &Path) {
        self.large_dataset_status = Some((
            path.to_path_buf(),
            format_large_dataset_status("Opening", file_size),
        ));
        self.draw(path);
    }

    fn record_large_dataset_scan(&mut self, file_size: u64, path: &Path) {
        self.large_dataset_status = Some((
            path.to_path_buf(),
            format_large_dataset_status("Scanning", file_size),
        ));
        self.draw(path);
    }

    fn clear_large_dataset_status(&mut self, path: &Path) {
        let should_clear = self
            .large_dataset_status
            .as_ref()
            .is_some_and(|(status_path, _)| status_path == path);
        if should_clear {
            self.large_dataset_status = None;
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
            "Datasets found: {} ({}). Total files checked: {}.",
            self.stats.datasets_found,
            output::format_byte_size(self.stats.dataset_bytes),
            self.stats.files_checked
        );
        let line = match &self.large_dataset_status {
            Some((_, status)) => format!("{line} {status}"),
            None => line,
        };
        let line_len = line.chars().count();
        eprint!("\r\x1b[2K{line}");
        let _ = io::stderr().flush();
        self.line_len = line_len;
    }
}

fn with_progress<R>(progress: &SharedProgress, f: impl FnOnce(&mut Progress) -> R) -> R {
    let mut progress = progress.lock().unwrap_or_else(|err| err.into_inner());
    f(&mut progress)
}

fn finish_progress(progress: &SharedProgress) -> SearchStats {
    with_progress(progress, |progress| {
        progress.finish();
        progress.stats()
    })
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
    fn skips_hdf5_and_netcdf_containers_before_gdal_probe() {
        assert!(should_skip_vector_probe(Path::new("data/foo.h5")));
        assert!(should_skip_vector_probe(Path::new("data/foo.HDF5")));
        assert!(should_skip_vector_probe(Path::new("data/foo.nc")));
        assert!(!should_skip_vector_probe(Path::new("data/foo.gpkg")));
        assert!(!should_skip_vector_probe(Path::new("data/foo.fgb")));
    }

    #[test]
    fn skips_common_non_vector_extensions() {
        for path in [
            "a.png", "a.JPG", "a.tif", "a.tiff", "a.webp",
            "a.mp3", "a.wav", "a.mp4", "a.mkv",
            "a.tar.gz", "a.7z", "a.rar", "a.iso",
            "a.rs", "a.py", "a.js", "a.tsx", "a.go", "a.cpp", "a.h", "a.swift",
            "a.so", "a.dylib", "a.dll", "a.exe", "a.class", "a.jar",
            "a.md", "a.html", "a.css",
            "a.doc", "a.docx", "a.ppt",
            "a.ttf", "a.woff2",
            "a.yaml", "a.yml", "Cargo.lock", "app.log", "Cargo.toml",
        ] {
            assert!(
                should_skip_vector_probe(Path::new(path)),
                "should skip {path}"
            );
        }
    }

    #[test]
    fn does_not_skip_vector_or_ambiguous_extensions() {
        for ext in [
            "gpkg", "shp", "geojson", "json", "fgb", "gml", "kml", "kmz",
            "gpx", "csv", "tsv", "vrt", "sqlite", "db", "tab", "mid",
            "mif", "dxf", "dwg", "osm", "pbf", "dbf", "parquet", "topojson",
            "ndjson", "geojsonl", "zip", "xml", "pdf", "xls", "xlsx", "ods",
            "svg",
        ] {
            let path = format!("data/sample.{ext}");
            assert!(
                !should_skip_vector_probe(Path::new(&path)),
                "should not skip {ext}"
            );
        }
        assert!(!should_skip_vector_probe(Path::new("data/no_extension")));
    }

    #[test]
    fn looks_like_geojson_head_detects_feature_collection() {
        let head = br#"{"type":"FeatureCollection","features":[]}"#;
        assert!(looks_like_geojson_head(head));
    }

    #[test]
    fn looks_like_geojson_head_detects_pretty_printed_collection() {
        let head = br#"{
  "type": "FeatureCollection",
  "features": [
    {"type": "Feature", "geometry": {"type": "Point", "coordinates": [1.0, 2.0]}}
  ]
}"#;
        assert!(looks_like_geojson_head(head));
    }

    #[test]
    fn looks_like_geojson_head_detects_geometry_only() {
        let head = br#"{"type":"Point","coordinates":[1,2]}"#;
        assert!(looks_like_geojson_head(head));
    }

    #[test]
    fn looks_like_geojson_head_detects_topojson() {
        let head = br#"{"type":"Topology","arcs":[],"objects":{}}"#;
        assert!(looks_like_geojson_head(head));
    }

    #[test]
    fn looks_like_geojson_head_rejects_typical_dev_json() {
        let package_json = br#"{"name":"myapp","version":"1.0.0","dependencies":{"react":"^18.0.0"}}"#;
        assert!(!looks_like_geojson_head(package_json));

        let tsconfig = br#"{"compilerOptions":{"target":"es2020"},"exclude":["node_modules"]}"#;
        assert!(!looks_like_geojson_head(tsconfig));

        let renovate = br#"{"$schema":"https://docs.renovatebot.com/renovate-schema.json","extends":["config:base"]}"#;
        assert!(!looks_like_geojson_head(renovate));
    }

    #[test]
    fn looks_like_geojson_head_rejects_empty_or_short_inputs() {
        assert!(!looks_like_geojson_head(b""));
        assert!(!looks_like_geojson_head(b"{}"));
        assert!(!looks_like_geojson_head(b"null"));
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
    is_spatial: bool,
    best: Option<Hit>,
    matched_fids: HashSet<u64>,
    anonymous_feature_matches: usize,
    exact_values: usize,
}

impl LayerSummary {
    fn new(path: &Path, layer: &str, is_spatial: bool) -> Self {
        Self {
            path: path.to_path_buf(),
            layer: layer.to_owned(),
            is_spatial,
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
        let mut matched_fids: Vec<u64> = self.matched_fids.into_iter().collect();
        matched_fids.sort_unstable();
        Some(output::LayerSummary {
            score: best.score,
            path: self.path,
            layer: self.layer,
            is_spatial: self.is_spatial,
            best: best.label,
            matched_features: matched_fids.len() + self.anonymous_feature_matches,
            exact_values: self.exact_values,
            matched_fids,
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
