# Changelog

## Unreleased

- New formats: `.bz2`, `.xz` and `.7z` (including password-protected 7z, with `--password` / `--password-file`), and the tarballs `.tar.bz2` (`.tbz2`), `.tar.xz` (`.txz`) and `.tar.zst`, which are now read member by member (a `.tar.zst` used to be read as one stream of tar bytes). Multi-stream bzip2 and xz files are read to the end, and a file whose name does not say what it is is recognised from its first bytes. `stats` counts the new kinds. New dependencies: `bzip2`, `liblzma`, `sevenz-rust2` (licenses checked; `deny.toml` now also allows `bzip2-1.0.6`, the libbzip2 license).
- Very long lines: `--max-line-bytes SIZE` searches a line longer than `SIZE` in overlapping pieces, cut at separators, so memory stays bounded however long a line is (a one-line SQL `INSERT` of hundreds of MB no longer needs several times its size in RAM); `--snippet BYTES` reports only the context around a match instead of the whole line. Both are off by default and then nothing changes. `PROWLCAT_MAX_LINE_BYTES` and `PROWLCAT_SNIPPET` set the defaults, and the interactive interface honours them.
- Command line: the options that several subcommands share (`-p`, `-r`, `-j`, the passwords, the line options, `-t`) are now defined once, so they read the same everywhere, and `--help` groups them (Input, Matching, Limits and speed, Output, Encrypted archives). `-t` and `--format` list their values and are checked up front (any case). `-q` and `-v` have descriptions. The `tui` subcommand takes `--max-line-bytes` and `--snippet` too. Nothing that worked before stops working.
- Command line: `--no-recursive` stays in the top level (`-r` was always on and could not be turned off; it is still accepted). `--format json|ndjson|csv` without `-o`, and `-o -`, write the results to standard output with no banner before them; before, `--format` without `-o` was ignored and a table was printed. A script that passed `--format` without `-o` and relied on the table now gets that format.
- Reliability: if the results of a search cannot be parked on disk (disk full, bad `PROWLCAT_TMP_DIR`) the search now stops with a clear error instead of filling the memory until the system kills it.
- Testing: an end-to-end suite runs the real binary on a generated corpus, and the release workflow runs it on every packaged binary; the release is created only when all platforms pass. `scripts/release-check.sh` is the gate to run before tagging.
- Licenses: `deny.toml` (checked in CI and on release) and `THIRD-PARTY-NOTICES.md`, which now ships in every package.

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
