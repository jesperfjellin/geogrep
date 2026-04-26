# geogrep

Fuzzy grep for geospatial data.

## Requirements

- Rust 1.85 or newer.
- GDAL development files available to `pkg-config`.

On Ubuntu/Debian:

```bash
sudo apt install libgdal-dev pkg-config
```

The repo includes `.cargo/config.toml` with a repo-local `PKG_CONFIG_PATH` for common Unix install locations.

## Build

```bash
cargo check
cargo test -p geogrep
```

## Run From Source

```bash
cargo run -p geogrep -- "New York" tests/data
cargo run -p geogrep -- --limit 10 --sizelimit 200 "Paris"
```

If no path is supplied, `geogrep` searches the current directory recursively.

## Install The CLI

Install the short binary name:

```bash
cargo install --path crates/geogrep --bin gg --locked --force
```

Make sure Cargo's bin directory is on `PATH`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

Then run:

```bash
gg "New York"
gg --limit 10 --sizelimit 200 "Berlin"
gg --layers "cities" ~/data
```

## Flags

Scope (combine freely; default searches all three):

- `--layers` — layer names
- `--columns` — field names
- `--values` — feature attribute values

Other:

- `--threshold <0-100>` — minimum fuzzy score (default 80)
- `--limit <n>` — cap on printed summaries
- `--sizelimit <MB>` — skip files larger than this
- `--verbose` — print diagnostics for skipped or unreadable files
- `--extract` — write matched features to `~/geogrep/extracts/` (prompts on first use and for outputs over 100 MB)
- `--timings` — per-dataset open/scan time breakdown, sorted by total time
