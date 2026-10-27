# Changelog

## 0.2.2

- Docs: the README is rewritten with screenshots of the interface, the full command-line reference, output samples and a section on large searches.
- Fix: in the interface's help sheet the key column ran into the descriptions (`@dirsearch…`); the column and the box are wider.
- Fix: domain searches now match subdomains and URLs (`example.com` finds `b@mail.example.com`, `https://example.com/x`) as `include_subdomains` intends; before, only `@example.com` was found. Searches can return more matches than in 0.2.1.
- Memory: the temporary files that hold matches past the budget are no longer created in a RAM-backed `/tmp` (tmpfs, the default on Arch/CachyOS and others), where a large search filled the RAM anyway. They go to `/var/tmp` or `~/.cache/prowlcat`; `PROWLCAT_TMP_DIR` chooses the directory.
- Memory/speed: those temporary files use a compact binary encoding instead of JSON lines. They are about 3.7 times smaller and a heavy search spends noticeably less time on them. Results and their order are unchanged.
- Memory: spilled runs share a few large temporary files (1 GiB each) instead of one file per run, and the command line raises the open-file limit at start. A search with a huge result set no longer runs out of file descriptors (the usual soft limit is 1024).

## 0.2.1

- Memory: searches with a very large number of matches no longer fill the RAM. Matches are kept in a compact form and, past a budget (256 MiB, `PROWLCAT_RESULT_MEM_MB`), sorted and parked in temporary files that are merged back in order. Results and their order are unchanged; NDJSON, CSV and the table are byte for byte identical to 0.2.0. All formats are now written one result at a time.
- JSON export (`-o file.json`) is now compact, on one line, instead of indented. The content is the same; pipe it through `jq .` to read it. Object keys follow the field order (as in NDJSON) rather than alphabetical order.
- The interface shows the first 250,000 matches and still counts, and exports, all of them.
- Core: `ScanOrchestrator::run_sorted` and `SortedResults` (bounded memory, repeatable iteration). `ProgressHandle::live` is now a bounded `SyncSender` and drops hits when the receiver falls behind. `prowlcat-output` gains `write_output_iter` and `print_results_iter`.

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
