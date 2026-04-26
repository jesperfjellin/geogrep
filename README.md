# geogrep

Fuzzy grep for geospatial vector data and their attributes

## Install

Install with Homebrew:

```bash
brew tap jesperfjellin/geogrep
brew install geogrep
```

Verify the installed CLI:

```bash
gg --version
```

Homebrew installs the required GDAL dependency.

To update:

```bash
brew update
brew upgrade geogrep
```

## Usage

Search the current directory recursively:

```bash
gg "New York"
```

Search one or more files or directories:

```bash
gg "Main street" ~/data
gg "Paris" data/cities.gpkg data/roads.geojson
```

Limit output or skip large files:

```bash
gg --limit 10 --sizelimit 200 "Berlin"
```

Search only specific scopes:

```bash
gg --layers "roads" ~/data
gg --columns "address" ~/data
gg --values "Main street" ~/data
```

Extract matched features from the dominant dataset/layer:

```bash
gg --extract "Main street" ~/data
```

## Flags

Scope (combine freely; default searches all three):

- `--layers` - layer names
- `--columns` - field names
- `--values` - feature attribute values

Other:

- `--threshold <0-100>` - minimum fuzzy score (default 80)
- `--limit <n>` - cap on printed summaries
- `--sizelimit <MB>` - skip files larger than this
- `--verbose` - print diagnostics for skipped or unreadable files
- `--extract` - write matched features to `~/geogrep/extracts/` (prompts on first use and for outputs over 100 MB)
- `--timings` - per-dataset open/scan time breakdown, sorted by total time

## Development

Requirements for building from source:

- Rust 1.85 or newer.
- GDAL development files available to `pkg-config`.

On Ubuntu/Debian:

```bash
sudo apt install libgdal-dev pkg-config
```

Build and test:

```bash
cargo check
cargo test -p geogrep
```

Run from source:

```bash
cargo run -p geogrep -- "New York" tests/data
cargo run -p geogrep -- --limit 10 --sizelimit 200 "Paris"
```
