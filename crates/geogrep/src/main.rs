use anyhow::Result;
use clap::Parser;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

mod extract;
mod matcher;
mod normalize;
mod output;
mod search;

const DEFAULT_THRESHOLD: u8 = 80;

#[derive(Debug, Parser)]
#[command(version, about = "Fuzzy-search vector GIS datasets")]
struct Cli {
    /// Search query.
    query: String,

    /// Dataset file or directory to search. Defaults to the current directory.
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Search layer names.
    #[arg(long)]
    layers: bool,

    /// Search field/column names.
    #[arg(long)]
    columns: bool,

    /// Search feature attribute values.
    #[arg(long)]
    values: bool,

    /// Minimum match score to include, from 0 to 100.
    #[arg(long, default_value_t = DEFAULT_THRESHOLD, value_parser = parse_threshold)]
    threshold: u8,

    /// Maximum number of ranked file/layer summaries to print.
    #[arg(long, value_parser = parse_limit)]
    limit: Option<usize>,

    /// Skip files larger than this many MB.
    #[arg(long = "sizelimit", value_name = "MB", value_parser = parse_sizelimit)]
    sizelimit_mb: Option<u64>,

    /// Print diagnostics for skipped files during directory searches.
    #[arg(long)]
    verbose: bool,

    /// Extract value-matched features from the dominant dataset/layer into a
    /// new file beside the input. Prompts for confirmation if the estimated
    /// output exceeds 100 MB.
    #[arg(long)]
    extract: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool> {
    let cli = Cli::parse();
    // Directory searches intentionally probe many non-datasets; keep GDAL from writing
    // directly to stderr and report failures through geogrep's own diagnostics instead.
    gdal::config::set_error_handler(|_, _, _| {});
    let paths = if cli.paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        cli.paths
    };
    let options = search::SearchOptions {
        threshold: cli.threshold,
        scopes: search::SearchScopes::from_flags(cli.layers, cli.columns, cli.values),
        size_limit_bytes: cli.sizelimit_mb.map(mb_to_bytes),
        verbose: cli.verbose,
        progress: std::io::stderr().is_terminal(),
    };
    let search_result = search::search_paths(&paths, &cli.query, options)?;
    let mut summaries = search_result.summaries;
    output::rank_summaries(&mut summaries);

    output::emit_scan_summary(&search_result.stats);

    let limit = cli.limit.unwrap_or(summaries.len()).min(summaries.len());
    for summary in &summaries[..limit] {
        output::emit_layer_summary(summary);
    }

    if cli.extract {
        if let Some(extraction) = extract::extract_dominant(&summaries, &cli.query)? {
            eprintln!(
                "--extract: wrote {} features to {}",
                extraction.features_written,
                extraction.output_path.display(),
            );
        }
    }

    Ok(!summaries.is_empty())
}

fn parse_threshold(raw: &str) -> std::result::Result<u8, String> {
    let threshold = raw
        .parse::<u8>()
        .map_err(|_| "threshold must be an integer from 0 to 100".to_owned())?;
    if threshold <= 100 {
        Ok(threshold)
    } else {
        Err("threshold must be an integer from 0 to 100".to_owned())
    }
}

fn parse_limit(raw: &str) -> std::result::Result<usize, String> {
    let limit = raw
        .parse::<usize>()
        .map_err(|_| "limit must be a positive integer".to_owned())?;
    if limit > 0 {
        Ok(limit)
    } else {
        Err("limit must be a positive integer".to_owned())
    }
}

fn parse_sizelimit(raw: &str) -> std::result::Result<u64, String> {
    let limit = raw
        .parse::<u64>()
        .map_err(|_| "sizelimit must be a positive integer number of MB".to_owned())?;
    if limit > 0 {
        Ok(limit)
    } else {
        Err("sizelimit must be a positive integer number of MB".to_owned())
    }
}

fn mb_to_bytes(mb: u64) -> u64 {
    mb.saturating_mul(1024).saturating_mul(1024)
}
