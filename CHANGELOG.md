# Changelog

## 0.2.0

- Interactive terminal UI (new `prowlcat-tui` crate, built on ratatui): home screen with recent searches and a command palette, live scan view with streaming hits, results explorer with filter, export and copy, and a corpus/index screen. It opens when `prowlcat` is run with no subcommand in a terminal.
- Night Eyes theme (amber on black) with graceful fallbacks: 256 colours, 16 colours, `NO_COLOR`, and an ASCII-only mode.
- Fang, the animated mascot: half-block pixel art that idles, prowls while a scan runs, pounces when it finds something and sulks when it does not. `--no-animation` / `PROWLCAT_NO_ANIM=1` hold a still frame.
- `--ui auto|tui|cli` (env `PROWLCAT_UI`) and `prowlcat tui` choose the interface. `search`, `index` and `stats` are unchanged, and `--ui cli` keeps even a bare `prowlcat` on the classic behaviour.
- The interface remembers the last corpus and recent searches in the user state directory (`--no-history`, `PROWLCAT_NO_HISTORY=1`, `/history clear`).
- Core: `ProgressHandle` gains `cancel` (stop a scan) and `live` (stream matches as files finish). Both default to off; scans without them behave as before.

## 0.1.0

- Email, domain, IP, CIDR and plain-text search over plaintext, `.gz`, `.zst`, `.tar`, `.tar.gz` and `.zip` files.
- Parallel scanning, memory-mapped large files, byte-based progress bar.
- Encrypted zip support with user-supplied candidate passwords (`--password`, `--password-file`).
- Terminal, JSON, NDJSON and CSV output.
- Optional Tantivy index: `prowlcat index` and `prowlcat search --index`.
- Criterion benchmarks (`cargo bench -p prowlcat-core`).
