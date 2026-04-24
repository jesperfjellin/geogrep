use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;

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

    /// Minimum match score to include, from 0 to 100.
    #[arg(long, default_value_t = DEFAULT_THRESHOLD, value_parser = parse_threshold)]
    threshold: u8,

    /// Maximum number of ranked file/layer summaries to print.
    #[arg(long, value_parser = parse_limit)]
    limit: Option<usize>,

    /// Search layer names.
    #[arg(long)]
    layers: bool,

    /// Search field/column names.
    #[arg(long)]
    columns: bool,

    /// Search feature attribute values.
    #[arg(long)]
    values: bool,

    /// Print diagnostics for skipped files during directory searches.
    #[arg(long)]
    verbose: bool,
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
        verbose: cli.verbose,
    };
    let mut summaries = search::search_paths(&paths, &cli.query, options)?;
    output::rank_summaries(&mut summaries);

    let limit = cli.limit.unwrap_or(summaries.len()).min(summaries.len());
    for summary in &summaries[..limit] {
        output::emit_layer_summary(summary);
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
