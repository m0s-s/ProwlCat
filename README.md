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
- Interactive terminal UI (the default when you run `prowlcat` in a terminal): live hits, a results explorer, an index screen, and **Fang**, the animated mascot
- A scripting interface that never changes: every subcommand behaves the same with or without a terminal

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

## Interactive mode

Run `prowlcat` on its own in a terminal:

```bash
prowlcat                          # opens the interface; the corpus is remembered between runs
prowlcat tui acme.corp -p ~/breaches   # same, and start searching right away
```

Type a target (email, domain, IP, CIDR) and press enter. Fang prowls while the scan runs, hits stream in live, and the results open in an explorer when it finishes. Type `/` for the command list.

| Key | Where | Does |
|-----|-------|------|
| `/` then a name | home | commands: `/path`, `/index`, `/stats`, `/export`, `/type`, `/passwords`, `/threads`, `/max`, `/history clear`, `/help`, `/quit` |
| `@dir` after a target | home | search that folder, and remember it |
| `tab` | home | complete a `/command` or an `@path` |
| `up` / `down` | home | recent searches |
| `esc` | scan | stop the search |
| `up/down`, `tab` | results | move, switch between files and lines |
| `/`, `e`, `y` | results | filter, export (`.json` `.csv` `.ndjson`), copy the line |
| `?` | anywhere | cheat sheet |
| `ctrl+c` | anywhere | quit |

The interface uses the Night Eyes theme (amber on black) and adapts to the terminal: truecolor, 256 or 16 colours, `NO_COLOR`, and an ASCII-only mode for non-UTF-8 locales. Terminals that are too short swap the mascot for a pair of eyes.

### Choosing the interface

The command line is the scripting interface and is untouched. The interactive UI only opens when you ask for it, or when you run `prowlcat` with nothing after it in a terminal.

| You run | In a terminal | Piped, redirected, in a script |
|---------|---------------|--------------------------------|
| `prowlcat` | interactive UI | usage error, as always |
| `prowlcat search …`, `index`, `stats` | classic, unchanged | classic, unchanged |
| `prowlcat tui [TARGET]… [-p DIR]` | interactive UI | error: needs a terminal |
| `prowlcat --ui cli …` or `PROWLCAT_UI=cli` | classic everywhere, even bare | classic |
| `prowlcat --ui tui search …` | interactive UI, prefilled from the options | error: needs a terminal |

`--ui` takes `auto` (default), `tui` or `cli`. Put `PROWLCAT_UI=cli` in a script's environment to guarantee the classic behaviour whatever its caller does. Exit codes, output formats and flags of `search`, `index` and `stats` are the same in every mode.

### Environment

| Variable | Effect |
|----------|--------|
| `PROWLCAT_UI` | `auto`, `tui` or `cli`; same as `--ui` |
| `PROWLCAT_NO_ANIM=1` | hold a still frame instead of animating Fang (also `prowlcat tui --no-animation`) |
| `PROWLCAT_NO_HISTORY=1` | do not remember the corpus or recent searches (also `--no-history`) |
| `PROWLCAT_STATE_DIR` | where that memory is kept (default: the per-user state directory) |
| `PROWLCAT_COLOR` | `truecolor`, `256`, `16` or `none`; overrides detection |
| `NO_COLOR` | no colours at all |
| `PROWLCAT_ASCII=1` | plain ASCII glyphs and mascot |
| `PROWLCAT_RESULT_MEM_MB` | RAM used to hold matches before they are parked in temporary files (default 256); affects memory and speed, never the results |
| `PROWLCAT_TMP_DIR` | directory for the temporary files that hold matches past the memory budget; by default the system temp directory, or `/var/tmp` / `~/.cache/prowlcat` when that is a RAM-backed tmpfs |
| `PROWLCAT_LOG_FILE` | write log lines to this file while the interface is open (they would otherwise draw over it) |

The remembered state is a small JSON file with the last corpus path, index path and your recent targets. Targets can be sensitive: turn it off with `--no-history`, or clear it with `/history clear`.

Pressing `esc` during a scan stops it: files not yet started are skipped and the file being read stops matching within a few hundred lines. A very large single file may still be read to its end in the background.

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
