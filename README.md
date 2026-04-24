# geogrep

Developer notes for building and running the project locally.

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


