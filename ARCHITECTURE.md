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
| `memmap2` / `memchr` / `bstr` | fast byte scanning |
| `aho-corasick` | multi-target pre-filter |
| `regex` / `ipnet` | IP extraction and CIDR |
| `flate2` / `zstd` / `tar` / `zip` | compressed streams and archives |
| `serde` / `csv` | export formats |
| `tantivy` | optional index |

---

## 3. High-level architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      ProwlCat CLI                           │
│  search │ index │ stats                                     │
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
│  Terminal │ JSON │ NDJSON │ CSV                             │
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
├── LICENSE
├── assets/ascii-art.txt
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
└── tests/fixtures/         # synthetic test data only
```

---

## 5. CLI

### `prowlcat search`

```bash
prowlcat search [OPTIONS] <TARGETS>...
prowlcat search [OPTIONS] -f <FILE> -p <PATH>...
```

| Flag | Description |
|------|-------------|
| `-p, --path` | File, directory, or glob (repeatable) |
| `-f, --targets-file` | One target per line (`#` comments ignored) |
| `-t, --type` | `auto`, `email`, `domain`, `ip`, `cidr`, `text` |
| `--loose` | Plain-text substring search (alias for `-t text`) |
| `-r, --recursive` | Recurse directories (default: true) |
| `-j, --threads` | Worker threads (default: CPU count) |
| `-m, --max-matches` | Cap matches per target (0 = unlimited) |
| `-o, --output` | Export to `.json`, `.csv`, `.ndjson` |
| `--password` | Candidate password for encrypted zip members (repeatable; visible in process list) |
| `--password-file` | File with one candidate password per line |
| `--no-progress` | Hide banner |

**Exit codes:** `0` = matches found, `1` = no matches, `2` = error.

### `prowlcat stats`

Corpus statistics without running a search.

### `prowlcat index`

```bash
prowlcat index -p ./corpus -o ./corpus.idx      # [-r] [-j N] [--password … | --password-file …]
prowlcat search bob@example.com --index ./corpus.idx
```

The output directory must not exist or be empty. See §9.

---

## 6. Scan strategy for large files

1. **No line-count pre-scan** — progress counts raw on-disk bytes consumed (compressed bytes for `.gz`/`.zst`/archives) against total file size, shown as a bar with throughput and ETA on an interactive stderr. Disabled by `--no-progress` or when stderr is not a terminal.
2. **8 MiB reusable buffer** for plaintext; mmap at or above `mmap_threshold_mb` (M2).
3. **One Rayon task per file**; sequential scan inside each file.
4. **Bounded memory for results** (`ScanOrchestrator::run_sorted`, `prowlcat-core/src/sorted.rs`). Matches are not kept as `ScanResult`s: each worker hands over compact hits (no repeated path or query text) in batches of about 2 MiB. Once the collector holds more than `result_memory_mb` (256 by default, `PROWLCAT_RESULT_MEM_MB`), it sorts the batch and writes it to an anonymous temporary file, a *run*. Every hit carries a key `(query rank, path rank, line, file index, sequence)` that reproduces the order of the old "collect, then stable sort by query, path, line". Reading back is a k-way merge of the in-memory tail and the runs, so RAM is the budget plus a small buffer per run, whatever the match count; the limit becomes free disk space. A file that fails after handing over part of its hits has them ignored, as a failed file always contributed nothing.
5. **Streaming output.** The writers in `prowlcat-output` take iterators, so compact JSON, NDJSON, CSV and the table are written one result at a time. The interface keeps the first 250,000 matches for the results screen and the complete `SortedResults` for export. The live view's channel is bounded and drops hits when the screen falls behind; the scan's own counter stays exact.

Not covered yet: a single line with no newline is read whole into memory, and `IndexSearcher::search` still builds its result list in memory.

---

### Benchmarks

`cargo bench -p prowlcat-core` runs criterion benchmarks over a synthetic corpus (default 32 MiB, override with `PROWLCAT_BENCH_MB`). Groups: `scan` (plain stream vs mmap, gzip, zstd, email/CIDR/text matchers, 1000-target Aho-Corasick) and `parallel` (1/2/4/8 threads over 8 shard files). Throughput is reported against uncompressed size.

---

## 7. Compressed files

| Format | Status |
|--------|--------|
| `.gz` | yes (`flate2`) |
| `.zst` | yes (`zstd`, M2) |
| `.tar`, `.tar.gz`/`.tgz` | yes (`tar`, M2) — each member scanned, reported as `archive_entry` |
| `.zip` | yes (`zip`, M2) — encrypted (ZipCrypto/AES) members opened with `--password` / `--password-file`; skipped with a warning if no candidate works |

Auto-detect via extension, falling back to magic bytes (gzip `1f 8b`, zstd `28 b5 2f fd`, zip `50 4b 03 04`). No decompression to disk.

---

## 8. Matching logic

### Email / domain / IP

- Aho-Corasick pre-filter (case-insensitive)
- Post-filter with token boundaries
- Domain: `@domain`, `://domain`, optional subdomains

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

## 9. Optional index (M4)

Tantivy-based index (`crates/prowlcat-index`) for repeated queries on the same corpus. Build with `prowlcat index`, query with `search --index <dir>` (replaces `-p`).

- **What is indexed:** only lines containing an email, dotted domain or IP. Each such line is stored verbatim with its path, archive entry, line number and byte offset. Tokens are lowercased; every parent-domain suffix of a host is indexed, and IPs go in a Tantivy IP field so CIDR queries are range queries.
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

---

## 11. Known limitations (v1)

- No custom regex queries
- Password-protected archives: encrypted zip only (you supply candidates; no cracking). Encrypted 7z/rar unsupported
- UTF-8 primary encoding; Latin-1 best-effort fallback
- Not a replacement for SQL/analytics databases
