# ProwlCat — Architecture

**ProwlCat** is a high-performance, local-first search engine for breach datasets (single files, compressed archives, recursive directories). Scope is deliberately limited to on-disk corpora: no external APIs, no remote OSINT.

---

## 1. Goals and constraints

| Goal | Architectural implication |
|------|---------------------------|
| Terabyte-scale data | Streaming, mmap, file-level parallelism |
| Repeated queries | Optional Tantivy index |
| Professional CLI UX | Explicit subcommands, progress, structured output |
| Extensibility | Modular crates, pluggable matchers and decoders |
| Correct IP/CIDR matching | Dedicated parser + literal + membership strategies |

---

## 2. Technology choice: Rust

| Criterion | Rust (ProwlCat) | Typical Python scripts |
|-----------|-----------------|------------------------|
| Throughput | SIMD, zero-copy, mmap | GIL, per-line string decode |
| Parallelism | Rayon work-stealing | multiprocessing overhead |
| Multi-pattern search | Aho-Corasick | nested target × line loops |
| Compression | streaming decoders | gzip line-by-line |
| Distribution | static binary | pip + runtime |

Reference: [ripgrep](https://github.com/BurntSushi/ripgrep) demonstrates Rust streaming + per-file parallelism at multi-TB scale.

### Core crates

| Crate | Role |
|-------|------|
| `clap` | CLI |
| `rayon` | parallel file workers |
| `walkdir` / `glob` | directory traversal |
| `memmap2` / `memchr` | fast byte scanning |
| `aho-corasick` | multi-target pre-filter |
| `regex` / `ipnet` | IP extraction and CIDR |
| `flate2` / `zstd` / `bzip2` / `liblzma` / `tar` / `zip` / `sevenz-rust2` | compressed streams and archives |
| `rusqlite` (SQLite bundled) / `bson` / `serde_json` | databases and dumps read record by record |
| `bincode` | compact encoding of the temporary files that hold results and records past the memory budget |
| `rlimit` | raises the open-file limit at start |
| `serde` / `csv` | export formats |
| `tantivy` | optional index |

---

## 3. High-level architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      ProwlCat CLI                           │
│  search │ index │ stats │ hashid │ tui                      │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                    Query Layer                              │
│  TargetParser → QueryKind (Email|Domain|Ip|Cidr|Text|Auto)  │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                 Scan Orchestrator                           │
│  FileEnumerator → per-file SearchJob → MatchSink            │
└──────────────────────────┬──────────────────────────────────┘
                           │
     ┌─────────────────────┼─────────────────────┐
     │                     │                     │
┌────▼────┐         ┌──────▼──────┐       ┌──────▼──────┐
│ Source  │         │  Decoder    │       │  Matcher    │
│ Walker  │         │  Pipeline   │       │  Engine     │
└─────────┘         └─────────────┘       └─────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                    Result Model                             │
│  ScanResult { query, kind, location, context, extracted }   │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                    Output Layer                             │
│  Table │ JSON │ NDJSON │ CSV │ extracted records            │
└─────────────────────────────────────────────────────────────┘
```

**Principles:** streaming first, parallelism per file, fail-soft on unreadable files, optional index for repeat queries.

---

## 4. Repository layout

```
prowlcat/
├── Cargo.toml
├── README.md
├── ARCHITECTURE.md
├── CHANGELOG.md
├── LICENSE
├── THIRD-PARTY-NOTICES.md    # licenses of what the binary includes (generated)
├── deny.toml                 # license policy, checked in CI and on release
├── assets/screenshots/       # README images, regenerated for every release
├── scripts/                  # release-check.sh, screenshots.sh, gen-third-party.sh
├── .github/workflows/        # CI, and Release (verify → build → publish)
├── crates/
│   ├── prowlcat-cli/       # binary `prowlcat`
│   ├── prowlcat-core/      # orchestration
│   ├── prowlcat-types/     # shared types
│   ├── prowlcat-source/    # file enumeration
│   ├── prowlcat-decode/    # streaming readers
│   ├── prowlcat-match/     # matching engines
│   ├── prowlcat-output/    # formatters
│   ├── prowlcat-index/     # optional Tantivy index
│   └── prowlcat-tui/       # interactive UI: Night Eyes theme, Fang the mascot
└── tests/fixtures/         # synthetic test data only (the end-to-end suite builds its own corpus)
```

---

## 5. CLI

Every subcommand is a thin layer over the same crates.

| Command | Does | Exit codes |
|---------|------|------------|
| `search` | find targets in files, or in an index (`--index`); `--extract`, `--unique`, `--mask` change what is written | `0` matches, `1` none, `2` error |
| `index` | build a Tantivy index (§9) | `0`, `2` error |
| `stats` | count the corpus by kind; `--check` also opens every file and says which ones a search could not read | `0`, `2` error |
| `hashid` | name the kinds of password hash a string can be (identification only) | `0`, `1` if one is unknown |
| `tui` | the interactive interface (§9b) | `0`, `2` error |

The complete option reference is the README and `prowlcat <command> --help`. Options that several commands share (`-p`, `-r/--no-recursive`, `-j`, the passwords, `--formats`, `--max-line-bytes`, `--snippet`, `-t/--loose`) are defined once in `prowlcat-cli/src/main.rs` (`Walk`, `Archives`, `Workers`, `Lines`, `Reading`, `Kind`) and grouped in the help by what they do (Input, Matching, Limits and speed, Output, Extraction, Encrypted archives); a unit test checks that each reads and is spelled the same in every command that has it. `-t` and `--format` take fixed sets of values, checked by the parser.

---

## 6. Scan strategy for large files

1. **No line-count pre-scan** — progress counts raw on-disk bytes consumed (compressed bytes for `.gz`/`.zst`/archives) against total file size, shown as a bar with throughput and ETA on an interactive stderr. Disabled by `--no-progress` or when stderr is not a terminal.
2. **8 MiB reusable buffer** for plaintext; mmap at or above `mmap_threshold_mb` (M2).
3. **One Rayon task per file**; sequential scan inside each file.
4. **Bounded memory for results** (`ScanOrchestrator::run_sorted`, `prowlcat-core/src/sorted.rs`). Matches are not kept as `ScanResult`s: each worker hands over compact hits (no repeated path or query text) in batches of about 2 MiB. Once the collector holds more than `result_memory_mb` (256 by default, `PROWLCAT_RESULT_MEM_MB`), it sorts the batch and writes it to an anonymous temporary file, a *run*. Every hit carries a key `(query rank, path rank, line, file index, sequence)` that reproduces the order of the old "collect, then stable sort by query, path, line". Reading back is a k-way merge of the in-memory tail and the runs, so RAM is the budget plus a small buffer per run, whatever the match count; the limit becomes free disk space. A file that fails after handing over part of its hits has them ignored, as a failed file always contributed nothing.
5. **Streaming output.** The writers in `prowlcat-output` take iterators, so compact JSON, NDJSON, CSV and the table are written one result at a time. The interface keeps the first 250,000 matches for the results screen and the complete `SortedResults` for export. The live view's channel is bounded and drops hits when the screen falls behind; the scan's own counter stays exact.

6. **Very long lines** (`--max-line-bytes`, `--snippet`; off by default). `prowlcat-decode/src/plain.rs` hands a line longer than the cap over in pieces of at most the cap, each cut after a separator (blank, tab, `,` `;` `(` `)` quotes, `|`, which never occur inside an email, host or IP) and repeating up to 256 bytes of the previous piece, so a match across a cut is found whole; the callback gets `skip`, the number of leading bytes the previous piece already held, and the scan drops a match that lies wholly inside them. Streaming and mmap cut at the same places. `--snippet` only changes what is reported: `scan_entry` takes the window around the match before it copies the line.
7. **Spill files** (`sorted.rs`). Runs are encoded with bincode and appended to a few shared temporary files (a new one every 1 GiB) so a huge search holds a few dozen descriptors; they go to `PROWLCAT_TMP_DIR`, else the system temp directory unless that is a RAM-backed tmpfs (then `/var/tmp`, then `~/.cache/prowlcat`). If a run cannot be written the scan stops with an error instead of filling memory.

Not covered yet: `IndexSearcher::search` still builds its result list in memory.

---

### Benchmarks

`cargo bench -p prowlcat-core` runs criterion benchmarks over a synthetic corpus (default 32 MiB, override with `PROWLCAT_BENCH_MB`). Groups: `scan` (plain stream vs mmap, gzip, zstd, email/CIDR/text matchers, 1000-target Aho-Corasick) and `parallel` (1/2/4/8 threads over 8 shard files). Throughput is reported against uncompressed size.

---

## 7. Compressed files, archives, databases

Detection (`prowlcat-source/src/entry.rs`) is by name first, then by the first bytes, so a file called `dump.bin` is recognised too. `prowlcat-decode` turns every kind into the same thing, lines handed to a callback `(entry, line number, offset, skip, line)`.

| Format | How it is read |
|--------|----------------|
| plain text | 8 MiB buffer, or mmap from `mmap_threshold_mb` |
| `.gz`, `.zst`, `.bz2`, `.xz` | streamed; bzip2 and xz of several streams are read to the end |
| `.tar`, `.tar.gz`, `.tar.bz2`, `.tar.xz`, `.tar.zst` | each member is a source, reported as `archive_entry` |
| `.zip`, `.7z` | each member is a source; encrypted ones are opened with `--password` / `--password-file`, tried in order (a 7z that fails before delivering anything moves on to the next candidate) |
| `.rar` | **through a program, not a library** (`rar.rs`): see below |
| SQLite database | read-only; every row of every table is a line (tab-separated values, blobs as hex), the table is the entry |
| `.bson` | every document is a line of relaxed extended JSON |
| `.json` that is one array | every element is a line, parsed one at a time (a visitor over `serde_json`'s sequence) |

`--formats text` (`PROWLCAT_FORMATS=text`) maps the last three to plain files. Nothing is decompressed to disk. A source that cannot be opened is a failed file (counted, logged, `warnings: N files failed`), never a silent "no match".

**RAR.** RAR's decoder cannot be included under this project's license, so ProwlCat contains none. It runs 7-Zip (`7zz`, then `7z`, from `PATH`; `PROWLCAT_RAR_TOOL` picks one, and is the only way `unrar` is ever used) as a separate process: it lists the archive, then streams every member in one pass and cuts the stream at the listed sizes, so members keep their names and nothing touches the disk. The program is never run through a shell and the archive path follows `--`; the child is killed if the scan is abandoned. `rar-rs` (BSD-2-Clause, clean-room) was evaluated and not used yet: it decodes a whole member in memory, which would defeat the bounded-memory design.

**`stats --check`** (`prowlcat_decode::probe`) opens each file as its kind says and reads its start: encrypted without a fitting password, a RAR with no program, damaged, or not what its name says. Damage further inside a file is not found.

---

## 8. Matching logic

### Email / domain / IP

- Aho-Corasick pre-filter (case-insensitive)
- Post-filter with token boundaries
- Domain: the name after `@` (`user@example.com`, `user@mail.example.com`), as a subdomain suffix (`.example.com`), or after `://` (with or without `www.`); the match must not run into a longer label, so `example.community` and `notexample.com` do not count. With subdomains off (a library setting, on by default) only `@domain`, `@www.domain`, `://domain` and `://www.domain` match

### CIDR (intelligent behavior)

1. **CidrLiteral** — CIDR string appears in the line
2. **IpInNetwork** — extract IPs from line, test `network.contains(ip)`

### Plain text (`text` / `--loose`)

- Case-insensitive substring
- No type inference or word-boundary rules
- Example: `prowlcat search Winter2024 --loose -p ./breaches`

**Performance notes:** IPv4 extraction is a hand-written digit scanner (no regex), IPv6 is only scanned for IPv6 networks, and IPv4 networks with at least one fixed octet (e.g. `10.0.0.0/8` -> `10.`) first reject lines lacking that literal via `memmem`. Together this made CIDR scans roughly 3-4x faster on the synthetic benchmark.

### Auto-detection order

1. Valid CIDR → `Cidr`
2. Valid IP → `Ip`
3. Contains `@` → `Email`
4. Otherwise → `Domain`

---

## 8b. Extraction, masking, hash identification

All three work on matches that already exist and never change which lines match.

- `prowlcat-core/src/extract.rs` takes a match and its line and returns `account` and `secret`, only for the layout of combo lists (the account first, then a separator, then the secret). Any other layout (URL records, password first, SQL rows) returns nothing and is counted as skipped, not guessed at.
- `unique.rs` drops duplicate records (account compared ignoring case, secret exactly, first occurrence kept) with an external sort: sorted runs in temporary files, then a k-way merge, so memory stays near the budget.
- `mask.rs` replaces what follows a match by `***` (best effort, for `account:secret`); with `--extract` the secret itself is replaced.
- `hashid.rs` lists what a string can be from its prefix, length and alphabet. It identifies; nothing in ProwlCat decrypts or cracks.

---

## 9. Optional index (M4)

Tantivy-based index (`crates/prowlcat-index`) for repeated queries on the same corpus. Build with `prowlcat index`, query with `search --index <dir>` (replaces `-p`).

- **What is indexed:** only lines containing an email, dotted domain or IP. Rows of SQLite databases, BSON documents and JSON array elements are lines like any other (`--formats text` indexes their plain bytes instead). Each such line is stored verbatim with its path, archive entry, line number and byte offset. Tokens are lowercased; every parent-domain suffix of a host is indexed, and IPs go in a Tantivy IP field so CIDR queries are range queries.
- **How a search runs:** the index nominates candidate lines, then the normal `MatchEngine` re-checks them. Spans, match kinds and extracted values are therefore identical to a full scan of the indexed lines.
- **Limits:** plain-text / `--loose` targets are rejected (term indexes cannot do substring search). Matches that are not token-bounded (e.g. an IP glued to letters) can be found by a scan but not by the index. The index copies the matching lines, so its size scales with the number of such lines. It is a snapshot: rebuild it after the corpus changes.

---

## 9b. Interactive interface (M6)

`crates/prowlcat-tui` is a second front end over the same crates: it calls `ScanOrchestrator`, `IndexSearcher` and `build_index` exactly as the CLI does, so results are identical in both modes.

**Choosing a front end** (`prowlcat-cli`, `resolve_launch`). The TUI is the default only for a bare `prowlcat` with a terminal on stdin and stdout. Every subcommand stays classic unless `--ui tui` is given, `--ui cli` / `PROWLCAT_UI=cli` pins the classic behaviour everywhere (a bare `prowlcat` then prints clap's usual "requires a subcommand" error), and `prowlcat tui` always means the interface. The interface refuses to start without a terminal, so a pipe never receives escape sequences.

**Structure.**

| Module | Role |
|--------|------|
| `app` | all state and behaviour: screens, prompt, commands, searches, results view. Draws nothing, so it is tested without a terminal |
| `ui/*` | pure drawing from `&App` (home, scan, results, corpus, overlays, splash) |
| `worker` | background threads for search, statistics and index builds; answers carry a job id so cancelled jobs are ignored |
| `mascot`, `mascot_data` | Fang: pixel grids (generated by `tools/gen_mascot.py` from the master drawing) rendered as half blocks; the pose is a pure function of `(mood, tick)` |
| `theme` | Night Eyes palette, colour-depth mapping, ASCII glyph set |
| `history` | remembered corpus and recent searches |
| `options` | what the interface starts with (targets, paths, limits, start screen), built from the command line |
| `clipboard` | copying the selected line (terminal escape sequence) |
| `command`, `input` | the prompt: `/commands`, `@paths`, a line editor |

**Live data.** `ProgressHandle` gained two opt-in fields: `live` (a channel that receives each match once its file is scanned) and `cancel` (polled every 1024 lines). A scan without them runs the same code as before.

**Animation.** One 100 ms tick drives everything. Fang blinks and breathes when idle, pulses and creeps while a scan runs, flashes on each hit, pounces on success and goes dim on no match; the pounce is a vertical shift of whole pixels, which half blocks make sub-cell smooth. `PROWLCAT_NO_ANIM=1` pins a still frame.

**Degradation.** Truecolor, 256 and 16 colours, no colour (`NO_COLOR`), and ASCII-only locales each have a rendering path; terminals under 50x12 get a message instead of a broken layout. Matched text is stripped of control characters before it is drawn, so a hostile line in a corpus cannot inject escape sequences.

---

## 10. Roadmap

| Milestone | Scope |
|-----------|-------|
| M0 | CLI, plaintext, email/domain/ip/cidr/text |
| M1 | gzip, parallelism, JSON/CSV export |
| M2 | zstd, tar.gz, zip, mmap — **done** |
| M3 | progress bar, benchmarks — **done** (`cargo bench -p prowlcat-core`) |
| M4 | Tantivy index — **done** (`prowlcat index`, `search --index`) |
| M5 | release binaries, docs — **done** (`.github/workflows`, README, CHANGELOG) |
| M6 | interactive UI with the Night Eyes theme and the Fang mascot — **done** (`prowlcat`, `prowlcat tui`, `--ui`) |
| M7 | end-to-end suite, release gate, license policy — **done** (`crates/prowlcat-cli/tests/e2e.rs`, `scripts/release-check.sh`, `deny.toml`) |
| M8 | bounded memory for huge result sets and very long lines — **done** |
| M9 | bz2, xz, 7z, rar (via 7-Zip), SQLite, BSON, JSON arrays, `stats --check` — **done** |
| M10 | `--extract`, `--unique`, `--mask`, `hashid` — **done** |

---

## 11. Known limitations (v1)

- No custom regex queries
- Password-protected archives: zip, 7z and rar (you supply candidates; no cracking). Encrypted RAR is not covered by tests. SQLCipher databases are not supported
- `.rar` needs 7-Zip installed; a SQLite database inside an archive is not read as a database
- Text is read as UTF-8; bytes that are not valid UTF-8 are replaced by U+FFFD, there is no Latin-1 or other decoding
- Not a replacement for SQL/analytics databases
