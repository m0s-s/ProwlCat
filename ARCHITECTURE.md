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
│                    Result Model                           │
│  ScanResult { query, kind, location, context, extracted } │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                    Output Layer                           │
│  Terminal │ JSON │ NDJSON │ CSV                             │
└───────────────────────────────────────────────────────────┘
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
│   └── prowlcat-output/    # formatters
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
4. **Bounded match channel** for streaming NDJSON export.

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

## 10. Roadmap

| Milestone | Scope |
|-----------|-------|
| M0 | CLI, plaintext, email/domain/ip/cidr/text |
| M1 | gzip, parallelism, JSON/CSV export |
| M2 | zstd, tar.gz, zip, mmap — **done** |
| M3 | progress bar, benchmarks — **done** (`cargo bench -p prowlcat-core`) |
| M4 | Tantivy index — **done** (`prowlcat index`, `search --index`) |
| M5 | release binaries, docs |

---

## 11. Known limitations (v1)

- No custom regex queries
- Password-protected archives: encrypted zip only (you supply candidates; no cracking). Encrypted 7z/rar unsupported
- UTF-8 primary encoding; Latin-1 best-effort fallback
- Not a replacement for SQL/analytics databases
