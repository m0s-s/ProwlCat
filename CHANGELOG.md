# Changelog

## 0.1.0

- Email, domain, IP, CIDR and plain-text search over plaintext, `.gz`, `.zst`, `.tar`, `.tar.gz` and `.zip` files.
- Parallel scanning, memory-mapped large files, byte-based progress bar.
- Encrypted zip support with user-supplied candidate passwords (`--password`, `--password-file`).
- Terminal, JSON, NDJSON and CSV output.
- Optional Tantivy index: `prowlcat index` and `prowlcat search --index`.
- Criterion benchmarks (`cargo bench -p prowlcat-core`).
