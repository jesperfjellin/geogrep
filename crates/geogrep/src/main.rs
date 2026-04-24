use anyhow::{Result, anyhow};
use std::path::PathBuf;

mod matcher;
mod normalize;
mod output;
mod search;

const DEFAULT_THRESHOLD: u8 = 80;

fn main() -> Result<()> {
    let mut args = std::env::args_os();
    let _bin = args.next();
    let query = args
        .next()
        .ok_or_else(|| anyhow!("usage: geogrep <query> <path>"))?
        .into_string()
        .map_err(|_| anyhow!("query is not valid UTF-8"))?;
    let path: PathBuf = args
        .next()
        .ok_or_else(|| anyhow!("usage: geogrep <query> <path>"))?
        .into();

    search::search_file(&path, &query, DEFAULT_THRESHOLD)
}
