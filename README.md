# ProwlCat

High-performance, **local-first** breach corpus search engine written in Rust. Scan single files, `.gz` archives, and large directory trees without external APIs.

## Features

- Email, domain, IP, and CIDR search (with intelligent subnet matching)
- Plain-text substring search (`--loose` / `-t text`)
- Streaming I/O with parallel per-file workers (Rayon)
- Auto-detection of `.gz`, `.zst`, `.tar`, `.tar.gz` and `.zip` files (encrypted zips with your own candidate passwords)
- Optional Tantivy index for fast repeated searches
- Progress bar on interactive terminals
- Terminal, JSON, NDJSON, and CSV output

## Install

Download a binary for Linux, macOS or Windows from the [Releases](https://github.com/m0s-s/ProwlCat/releases) page, or build from source (Rust stable):

```bash
cargo install --git https://github.com/m0s-s/ProwlCat prowlcat-cli
```

## Quick start

```bash
cargo build --release
./target/release/prowlcat search user@example.com -p /path/to/breaches -r
```

## Examples

```bash
# Email
prowlcat search user@example.com -p ./breaches -r

# Domain
prowlcat search example.com -t domain -p ./data

# IP and CIDR
prowlcat search 203.0.113.42 -t ip -p ./logs
prowlcat search 10.0.0.0/8 -t cidr -p ./logs

# Plain text (case-insensitive substring)
prowlcat search Winter2024 -t text -p ./breaches
prowlcat search "john.smith" --loose -p ./breaches

# Targets from file, JSON export
prowlcat search -f targets.txt -p ./dumps -o results.json

# Corpus statistics (no search)
prowlcat stats -p ./breaches -r

# Encrypted zips: supply candidate passwords (no cracking)
prowlcat search user@example.com -p ./dumps --password-file passwords.txt

# Index once, search many times (no text/--loose targets)
prowlcat index -p ./breaches -o ./breaches.idx
prowlcat search user@example.com --index ./breaches.idx
```

## Exit codes

`0` matches found, `1` no matches, `2` error.

## Development

```bash
cargo test --workspace
cargo bench -p prowlcat-core --bench scan   # PROWLCAT_BENCH_MB sets corpus size
```

Pushing a `v*` tag builds and publishes release binaries via GitHub Actions.

## Search types

| Type | Flag | Behavior |
|------|------|----------|
| auto | `-t auto` (default) | Infers email, IP, CIDR, or domain |
| email | `-t email` | Email address |
| domain | `-t domain` | Domain in emails, URLs, subdomains |
| ip | `-t ip` | Single IPv4/IPv6 address |
| cidr | `-t cidr` | Literal CIDR string + IPs inside network |
| text | `-t text` or `--loose` | Substring match, no type rules |

## Documentation

- [ARCHITECTURE.md](./ARCHITECTURE.md) — design, matching logic, roadmap

## License

GNU Affero General Public License v3.0 — see [LICENSE](./LICENSE).
