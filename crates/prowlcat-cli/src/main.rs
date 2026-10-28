mod commands;

use std::path::PathBuf;
use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use tracing_subscriber::EnvFilter;

use prowlcat_core::{ProgressHandle, ScanConfig, ScanOrchestrator, SortedResults, TargetParser, TypeHint};
use prowlcat_output::{infer_format, print_results_iter, write_output_iter, write_stdout_iter, OutputFormat};
use prowlcat_tui::{Start, TuiOptions};

#[derive(Parser)]
#[command(
    name = "prowlcat",
    version,
    about = "ProwlCat — local-first breach corpus search engine",
    long_about = "ProwlCat: high-performance local search across plaintext and compressed breach datasets.\n\n\
Run `prowlcat` on its own in a terminal for the interactive interface. The subcommands below are \
the scripting interface and behave the same whether or not a terminal is attached; `--ui cli` \
(or PROWLCAT_UI=cli) pins the classic behaviour everywhere."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Log only warnings and errors
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Log more detail
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Interface to use: `auto` opens the interactive UI when run with no subcommand in a
    /// terminal; `cli` never does; `tui` always does (for any subcommand, prefilled from its options)
    #[arg(long, global = true, value_enum, default_value_t = UiMode::Auto, env = "PROWLCAT_UI", value_name = "MODE")]
    ui: UiMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum UiMode {
    Auto,
    Tui,
    Cli,
}

#[derive(Subcommand)]
enum Commands {
    /// Search breach files for emails, domains, IPs, CIDR ranges, or plain text
    Search(SearchArgs),
    /// Build a Tantivy index of a corpus for fast repeated searches
    Index(IndexArgs),
    /// Show corpus statistics without searching
    Stats(StatsArgs),
    /// Open the interactive interface (the default when run with no subcommand in a terminal)
    Tui(TuiArgs),
    /// Tell what kind of password hash a string looks like (it identifies, it does not crack)
    Hashid(HashidArgs),
}

/// What `-t/--type` can force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum TargetType {
    Auto,
    Email,
    Domain,
    Ip,
    Cidr,
    Text,
}

impl From<TargetType> for TypeHint {
    fn from(t: TargetType) -> Self {
        match t {
            TargetType::Auto => TypeHint::Auto,
            TargetType::Email => TypeHint::Email,
            TargetType::Domain => TypeHint::Domain,
            TargetType::Ip => TypeHint::Ip,
            TargetType::Cidr => TypeHint::Cidr,
            TargetType::Text => TypeHint::Text,
        }
    }
}

/// Formats for `--format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Table,
    Json,
    #[value(alias = "jsonl")]
    Ndjson,
    Csv,
}

impl From<Format> for OutputFormat {
    fn from(f: Format) -> Self {
        match f {
            Format::Table => OutputFormat::Table,
            Format::Json => OutputFormat::Json,
            Format::Ndjson => OutputFormat::Ndjson,
            Format::Csv => OutputFormat::Csv,
        }
    }
}

// Options shared by several subcommands are defined once, so they read the same everywhere.

/// Whether to descend into subdirectories.
#[derive(Args)]
#[command(next_help_heading = "Input")]
struct Walk {
    /// Descend into subdirectories (this is the default; accepted so existing scripts keep working)
    #[arg(short = 'r', long)]
    recursive: bool,

    /// Do not descend into subdirectories
    #[arg(long, conflicts_with = "recursive")]
    no_recursive: bool,
}

impl Walk {
    fn on(&self) -> bool {
        !self.no_recursive
    }
}

/// Passwords for encrypted zip archives.
#[derive(Args)]
#[command(next_help_heading = "Encrypted archives")]
struct Archives {
    /// Password to try on encrypted zip archives (repeatable; visible in the process list, prefer --password-file)
    #[arg(long = "password", value_name = "PASSWORD")]
    passwords: Vec<String>,

    /// File with one candidate password per line
    #[arg(long, value_name = "FILE")]
    password_file: Option<PathBuf>,
}

impl Archives {
    fn load(&self) -> Result<Vec<String>> {
        load_passwords(self.passwords.clone(), self.password_file.as_deref())
    }
}

/// Parallelism.
#[derive(Args)]
#[command(next_help_heading = "Limits and speed")]
struct Workers {
    /// Parallel worker threads (default: all cores)
    #[arg(short = 'j', long, value_name = "N")]
    threads: Option<usize>,
}

/// How very long lines are read and shown.
#[derive(Args)]
#[command(next_help_heading = "Limits and speed")]
struct Lines {
    /// Search lines longer than SIZE in overlapping pieces, so memory stays bounded however long a
    /// line is (for example 64K or 1M; at least 4K; 0 = whole lines). A match is reported with the
    /// piece it lies in. Env: PROWLCAT_MAX_LINE_BYTES
    #[arg(long, value_name = "SIZE", value_parser = parse_size_arg)]
    max_line_bytes: Option<usize>,

    /// Report only SIZE bytes of context on each side of a match in a long line, with `…` where it
    /// goes on (for example 60; 0 = whole lines). Lines that fit are unchanged. Env: PROWLCAT_SNIPPET
    #[arg(long, value_name = "SIZE", value_parser = parse_size_arg)]
    snippet: Option<usize>,
}

impl Lines {
    /// The scan reads these from the environment, which is how the interactive interface gets
    /// them too, so the flags are handed over the same way.
    fn export_to_env(&self) {
        if let Some(n) = self.max_line_bytes {
            std::env::set_var("PROWLCAT_MAX_LINE_BYTES", n.to_string());
        }
        if let Some(n) = self.snippet {
            std::env::set_var("PROWLCAT_SNIPPET", n.to_string());
        }
    }
}

/// What kind of thing the targets are.
#[derive(Args)]
#[command(next_help_heading = "Matching")]
struct Kind {
    /// Force the target type (default: guess from the target)
    #[arg(short = 't', long = "type", value_enum, ignore_case = true, default_value_t = TargetType::Auto, value_name = "TYPE")]
    target_type: TargetType,

    /// Plain-text, case-insensitive substring search with no type rules (same as `-t text`)
    #[arg(long)]
    loose: bool,
}

impl Kind {
    fn hint(&self) -> TypeHint {
        if self.loose {
            TypeHint::Text
        } else {
            self.target_type.into()
        }
    }
}

#[derive(Parser)]
struct TuiArgs {
    /// Targets to search straight away; leave empty to start at the prompt
    #[arg(value_name = "TARGET")]
    targets: Vec<String>,

    /// Files, directories, or globs to scan (remembered from the last session when omitted)
    #[arg(short = 'p', long = "path", value_name = "PATH", help_heading = "Input")]
    paths: Vec<PathBuf>,

    /// Search a prebuilt index instead of scanning files
    #[arg(long, value_name = "DIR", conflicts_with = "paths", help_heading = "Input")]
    index: Option<PathBuf>,

    #[command(flatten)]
    walk: Walk,

    #[command(flatten)]
    kind: Kind,

    /// Maximum matches per target (0 = unlimited)
    #[arg(short = 'm', long, value_name = "N", default_value_t = 0, help_heading = "Limits and speed")]
    max_matches: u64,

    #[command(flatten)]
    workers: Workers,

    #[command(flatten)]
    lines: Lines,

    #[command(flatten)]
    archives: Archives,

    /// Hold a still frame instead of animating Fang (also PROWLCAT_NO_ANIM=1)
    #[arg(long, help_heading = "Interface")]
    no_animation: bool,

    /// Skip the intro
    #[arg(long, help_heading = "Interface")]
    no_splash: bool,

    /// Do not remember the corpus or recent searches (also PROWLCAT_NO_HISTORY=1)
    #[arg(long, help_heading = "Interface")]
    no_history: bool,
}

#[derive(Parser)]
struct SearchArgs {
    /// Targets: email, domain, IP, CIDR, or plain text
    #[arg(value_name = "TARGET")]
    targets: Vec<String>,

    /// File with one target per line (blank lines and `#` comments are ignored)
    #[arg(short = 'f', long, value_name = "FILE", help_heading = "Input")]
    targets_file: Option<PathBuf>,

    /// Files, directories, or globs to scan (repeatable)
    #[arg(short = 'p', long = "path", value_name = "PATH", required_unless_present = "index", help_heading = "Input")]
    paths: Vec<PathBuf>,

    /// Search a prebuilt index (see `prowlcat index`) instead of scanning files; text targets are not supported
    #[arg(long, value_name = "DIR", conflicts_with = "paths", help_heading = "Input")]
    index: Option<PathBuf>,

    #[command(flatten)]
    walk: Walk,

    #[command(flatten)]
    kind: Kind,

    /// Maximum matches per target (0 = unlimited)
    #[arg(short = 'm', long, value_name = "N", default_value_t = 0, help_heading = "Limits and speed")]
    max_matches: u64,

    #[command(flatten)]
    workers: Workers,

    #[command(flatten)]
    lines: Lines,

    /// Write the results to FILE; the extension picks the format (.json, .ndjson, .csv). `-` means
    /// standard output (NDJSON unless --format says otherwise)
    #[arg(short = 'o', long, value_name = "FILE", help_heading = "Output")]
    output: Option<PathBuf>,

    /// Output format. With -o it overrides the extension; without -o the results go to standard
    /// output in this format (default: a readable table)
    #[arg(long, value_enum, ignore_case = true, value_name = "FORMAT", help_heading = "Output")]
    format: Option<Format>,

    /// Disable the banner and the progress bar
    #[arg(long, help_heading = "Output")]
    no_progress: bool,

    /// Print `account:secret` for each matching line instead of the whole line. It expects the
    /// layout of combo lists, the account first and then the secret (`alice@example.com:hunter2`);
    /// lines laid out differently are counted and skipped, never guessed at. Goes to standard output
    /// unless -o names a file; --format json|ndjson|csv gives fields (and what a hash-looking
    /// secret can be)
    #[arg(long, help_heading = "Extraction")]
    extract: bool,

    /// With --extract: each account and secret once (accounts compared ignoring case), sorted
    #[arg(long, requires = "extract", help_heading = "Extraction")]
    unique: bool,

    /// Hide secrets: what follows a match is shown as `***` (with --extract, the secret is).
    /// Made for `account:secret` lines; a line that keeps the password before the account is not
    /// protected
    #[arg(long, help_heading = "Extraction")]
    mask: bool,

    #[command(flatten)]
    archives: Archives,
}

#[derive(Parser)]
struct IndexArgs {
    /// Files, directories, or globs to index (repeatable)
    #[arg(short = 'p', long = "path", value_name = "PATH", required = true, help_heading = "Input")]
    paths: Vec<PathBuf>,

    /// Directory to create the index in (must not exist or be empty)
    #[arg(short = 'o', long, value_name = "DIR", required = true, help_heading = "Output")]
    output: PathBuf,

    #[command(flatten)]
    walk: Walk,

    #[command(flatten)]
    workers: Workers,

    #[command(flatten)]
    archives: Archives,
}

#[derive(Parser)]
struct HashidArgs {
    /// Hashes to identify; with none, they are read from standard input, one per line
    #[arg(value_name = "HASH")]
    hashes: Vec<String>,
}

#[derive(Parser)]
struct StatsArgs {
    /// Files, directories, or globs to count (repeatable)
    #[arg(short = 'p', long = "path", value_name = "PATH", required = true, help_heading = "Input")]
    paths: Vec<PathBuf>,

    #[command(flatten)]
    walk: Walk,

    /// Also open every file, without searching it, and report which ones a search could not read
    /// (encrypted without a password that fits, a RAR with no program to read it, damaged, or not
    /// the format its name says)
    #[arg(long, help_heading = "Coverage")]
    check: bool,

    #[command(flatten)]
    archives: Archives,
}

fn main() -> ExitCode {
    // Scans open many files at once (inputs and, for big result sets, temporary files); the
    // usual soft limit of 1024 is far below what the system allows.
    let _ = rlimit::increase_nofile_limit(u64::MAX);
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let kind = match &cli.command {
        None => CommandKind::None,
        Some(Commands::Tui(_)) => CommandKind::Tui,
        Some(_) => CommandKind::Other,
    };

    match resolve_launch(cli.ui, kind, prowlcat_tui::is_interactive())? {
        Launch::NoSubcommand => Ok(missing_subcommand()),
        Launch::Tui => {
            init_tui_logging(cli.verbose, cli.quiet);
            run_tui(cli.command)
        }
        Launch::Classic => {
            init_tracing(cli.verbose, cli.quiet);
            match cli.command {
                Some(Commands::Search(args)) => run_search(args),
                Some(Commands::Index(args)) => run_index(args),
                Some(Commands::Stats(args)) => {
                    let passwords = args.archives.load()?;
                    commands::stats::run(args.paths, args.walk.on(), args.check, &passwords)
                }
                Some(Commands::Hashid(args)) => commands::hashid::run(args.hashes),
                Some(Commands::Tui(_)) | None => bail!("internal error: no classic command to run"),
            }
        }
    }
}

/// What was typed after `prowlcat`, as far as choosing an interface is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandKind {
    None,
    Tui,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Launch {
    /// The scripting interface: exactly what `prowlcat` has always done.
    Classic,
    /// The interactive interface.
    Tui,
    /// No subcommand and no reason to open the interface: report it like before.
    NoSubcommand,
}

/// Decide which interface to run.
///
/// The interactive UI is the default only for a bare `prowlcat` in a terminal. Every subcommand
/// keeps its classic behaviour unless `--ui tui` asks otherwise, and `--ui cli` (or the
/// `PROWLCAT_UI=cli` environment variable) pins the classic behaviour everywhere.
fn resolve_launch(ui: UiMode, command: CommandKind, interactive: bool) -> Result<Launch> {
    Ok(match (command, ui) {
        (CommandKind::Tui, UiMode::Cli) => bail!("`prowlcat tui` opens the interactive interface, which conflicts with `--ui cli`"),
        (CommandKind::Tui, _) => Launch::Tui,
        (CommandKind::Other, UiMode::Tui) => Launch::Tui,
        (CommandKind::Other, _) => Launch::Classic,
        (CommandKind::None, UiMode::Tui) => Launch::Tui,
        (CommandKind::None, UiMode::Auto) if interactive => Launch::Tui,
        (CommandKind::None, _) => Launch::NoSubcommand,
    })
}

/// Print clap's own "requires a subcommand" error and exit with its usual status.
fn missing_subcommand() -> ExitCode {
    let _ = Cli::command().subcommand_required(true).get_matches();
    ExitCode::from(2)
}

fn run_tui(command: Option<Commands>) -> Result<ExitCode> {
    let options = match command {
        None => TuiOptions::default(),
        Some(Commands::Tui(args)) => tui_from_args(args)?,
        Some(Commands::Search(args)) => tui_from_search(args)?,
        Some(Commands::Index(args)) => TuiOptions {
            paths: args.paths,
            recursive: args.walk.on(),
            threads: args.workers.threads,
            passwords: args.archives.load()?,
            start: Start::IndexBuild(args.output),
            ..TuiOptions::default()
        },
        Some(Commands::Stats(args)) => {
            TuiOptions { paths: args.paths, recursive: args.walk.on(), start: Start::Stats, ..TuiOptions::default() }
        }
        Some(Commands::Hashid(_)) => TuiOptions::default(),
    };
    prowlcat_tui::run(options)?;
    Ok(ExitCode::SUCCESS)
}

fn tui_from_args(args: TuiArgs) -> Result<TuiOptions> {
    args.lines.export_to_env();
    let start = if args.targets.is_empty() { Start::Home } else { Start::Search };
    let defaults = TuiOptions::default();
    Ok(TuiOptions {
        targets: args.targets,
        hint: args.kind.hint(),
        paths: args.paths,
        index: args.index,
        recursive: args.walk.on(),
        threads: args.workers.threads,
        max_matches: args.max_matches,
        passwords: args.archives.load()?,
        start,
        animate: defaults.animate && !args.no_animation,
        splash: !args.no_splash,
        persist: defaults.persist && !args.no_history,
    })
}

/// `prowlcat search ... --ui tui`: open the interface and run that search.
fn tui_from_search(args: SearchArgs) -> Result<TuiOptions> {
    if args.output.is_some() || args.format.is_some() {
        bail!("--output and --format write a file without opening the interface; use `--ui cli` for that, or press `e` on the results to export");
    }
    args.lines.export_to_env();
    let mut targets = args.targets;
    if let Some(file) = &args.targets_file {
        targets.extend(read_targets(file)?);
    }
    let start = if targets.is_empty() { Start::Home } else { Start::Search };
    Ok(TuiOptions {
        targets,
        hint: args.kind.hint(),
        paths: args.paths,
        index: args.index,
        recursive: args.walk.on(),
        threads: args.workers.threads,
        max_matches: args.max_matches,
        passwords: args.archives.load()?,
        start,
        ..TuiOptions::default()
    })
}

/// Targets from a file: one per line, blank lines and `#` comments ignored.
fn read_targets(file: &std::path::Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(file).with_context(|| format!("read targets file {}", file.display()))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect())
}

/// Log lines would be drawn over the interface, so the interactive mode stays silent unless a
/// log file is named (`PROWLCAT_LOG_FILE=/tmp/prowlcat.log`).
fn init_tui_logging(verbose: u8, quiet: bool) {
    let Some(path) = std::env::var_os("PROWLCAT_LOG_FILE") else { return };
    let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else { return };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(format!("{},tantivy=warn", log_level(verbose, quiet)))))
        .with_writer(std::sync::Mutex::new(file))
        .with_ansi(false)
        .with_target(false)
        .try_init();
}

fn log_level(verbose: u8, quiet: bool) -> &'static str {
    if quiet {
        "warn"
    } else if verbose > 0 {
        "debug"
    } else {
        "info"
    }
}

fn run_search(args: SearchArgs) -> Result<ExitCode> {
    // Data on standard output (`-o -`, or `--format json|ndjson|csv` without `-o`) must not be
    // preceded by the banner.
    let data_on_stdout = match (&args.output, args.format) {
        (Some(path), _) => path.as_os_str() == "-",
        (None, _) if args.extract => true,
        (None, format) => format.is_some_and(|f| f != Format::Table),
    };
    if !args.no_progress && !data_on_stdout {
        print_banner();
    }

    let parser = TargetParser::new(args.kind.hint());

    let mut raw_targets = args.targets;
    if let Some(file) = args.targets_file {
        raw_targets.extend(read_targets(&file)?);
    }

    if raw_targets.is_empty() {
        anyhow::bail!("no targets provided (positional args or --targets-file)");
    }

    let queries = parser
        .parse_many(raw_targets)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let passwords = args.archives.load()?;

    let mut config = ScanConfig {
        passwords,
        paths: args.paths,
        recursive: args.walk.on(),
        max_matches_per_query: args.max_matches,
        ..ScanConfig::default()
    };
    if let Some(t) = args.workers.threads {
        config.threads = t;
    }
    if let Some(n) = args.lines.max_line_bytes {
        config.max_line_bytes = n;
    }
    if let Some(n) = args.lines.snippet {
        config.snippet_bytes = n;
    }

    // Progress bar only on an interactive stderr, so piped/exported output stays clean.
    let show_progress = !args.no_progress && std::io::stderr().is_terminal();
    let progress = show_progress.then(ProgressHandle::default);
    config.progress = progress.clone();

    let start = Instant::now();
    // Results come back already in the stable, readable order (query, path, line). Large result
    // sets wait in temporary files instead of RAM, and are written out one at a time.
    let mut found = if let Some(dir) = &args.index {
        let searcher = prowlcat_index::IndexSearcher::open(dir)?;
        let (results, summary) = searcher.search(&queries, config.include_subdomains, config.max_matches_per_query)?;
        SortedResults::from_vec(results, summary)
    } else {
        let orchestrator = ScanOrchestrator::new(config, queries);
        match &progress {
            Some(handle) => run_with_progress(&orchestrator, handle)?,
            None => orchestrator.run_sorted()?,
        }
    };
    found.summary_mut().elapsed_ms = start.elapsed().as_millis() as u64;

    if args.extract {
        let budget = usize::try_from(ScanConfig::default().result_memory_mb.saturating_mul(1 << 20)).unwrap_or(usize::MAX);
        return write_extract(&found, args.output.as_deref(), args.format, args.unique, args.mask, budget);
    }

    // With --mask every result is handed on with what follows its match hidden.
    let mask = args.mask;
    let results = found.iter().map(move |r| {
        r.map(|mut r| {
            if mask {
                r.line_text = prowlcat_core::mask::mask_after_match(&r.line_text, r.matched_span);
            }
            r
        })
    });
    let format: Option<OutputFormat> = args.format.map(Into::into);
    match args.output {
        Some(path) if path.as_os_str() == "-" => {
            write_stdout_iter(format.unwrap_or(OutputFormat::Ndjson), results, found.summary())?
        }
        Some(path) => {
            let format = format.unwrap_or_else(|| infer_format(&path));
            write_output_iter(&path, format, results, found.summary())?
        }
        None => match format {
            Some(format) => write_stdout_iter(format, results, found.summary())?,
            None => print_results_iter(results, found.summary())?,
        },
    }

    Ok(if found.summary().matches > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// `--extract`: write `account:secret` for the matching lines whose layout allows it.
fn write_extract(
    found: &SortedResults,
    output: Option<&std::path::Path>,
    format: Option<Format>,
    unique: bool,
    mask: bool,
    budget: usize,
) -> Result<ExitCode> {
    use std::cell::Cell;

    let (matches, skipped) = (Cell::new(0u64), Cell::new(0u64));
    let records = found.iter().filter_map(|r| match r {
        Ok(r) => {
            matches.set(matches.get() + 1);
            let record = prowlcat_core::extract::record_from(&r);
            if record.is_none() {
                skipped.set(skipped.get() + 1);
            }
            record.map(Ok)
        }
        Err(e) => Some(Err(e)),
    });

    let to_stdout = output.is_none_or(|p| p.as_os_str() == "-");
    let format: OutputFormat = match (format, output) {
        (Some(f), _) => f.into(),
        (None, Some(p)) if !to_stdout => match p.extension().and_then(|e| e.to_str()) {
            Some("json") => OutputFormat::Json,
            Some("ndjson") | Some("jsonl") => OutputFormat::Ndjson,
            Some("csv") => OutputFormat::Csv,
            _ => OutputFormat::Table,
        },
        _ => OutputFormat::Table,
    };

    let records: Box<dyn Iterator<Item = Result<prowlcat_core::Record>> + '_> = if unique {
        let mut u = prowlcat_core::unique::Unique::new(budget);
        for r in records {
            u.push(r?)?;
        }
        Box::new(u.finish()?)
    } else {
        Box::new(records)
    };

    let written = if to_stdout {
        let result = prowlcat_output::write_records_to(std::io::stdout().lock(), format, mask, records);
        match result {
            Err(e) if e.chain().any(|c| c.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)) => 0,
            other => other?,
        }
    } else {
        let path = output.expect("a file when not standard output");
        prowlcat_output::write_records_to(std::fs::File::create(path)?, format, mask, records)?
    };

    eprintln!(
        "{written} {} from {} matching lines ({} skipped: not laid out as account then secret)",
        if unique { "unique records" } else { "records" },
        matches.get(),
        skipped.get(),
    );
    Ok(if written > 0 { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

fn load_passwords(mut passwords: Vec<String>, file: Option<&std::path::Path>) -> Result<Vec<String>> {
    if let Some(file) = file {
        let content = std::fs::read_to_string(file)
            .with_context(|| format!("read password file {}", file.display()))?;
        passwords.extend(
            content
                .lines()
                .map(|l| l.trim_end_matches(char::from(13)))
                .filter(|l| !l.is_empty())
                .map(str::to_string),
        );
    }
    Ok(passwords)
}

fn run_index(args: IndexArgs) -> Result<ExitCode> {
    let defaults = ScanConfig::default();
    let cfg = prowlcat_index::IndexBuildConfig {
        paths: args.paths,
        recursive: args.walk.on(),
        threads: args.workers.threads.unwrap_or(defaults.threads),
        mmap_threshold_mb: defaults.mmap_threshold_mb,
        passwords: args.archives.load()?,
    };
    let start = Instant::now();
    let meta = prowlcat_index::build_index(&cfg, &args.output)?;
    eprintln!(
        "indexed {} lines from {} files ({} bytes) into {} in {:.1}s ({} files failed)",
        meta.docs,
        meta.files,
        meta.bytes,
        args.output.display(),
        start.elapsed().as_secs_f64(),
        meta.errors,
    );
    Ok(ExitCode::SUCCESS)
}

/// Run the scan while a ticker thread renders a byte-based progress bar on stderr.
fn run_with_progress(
    orchestrator: &ScanOrchestrator,
    handle: &ProgressHandle,
) -> Result<SortedResults> {
    use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

    let bar = ProgressBar::with_draw_target(Some(0), ProgressDrawTarget::stderr_with_hz(10));
    bar.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan} [{elapsed_precise}] {bar:40.cyan/blue} {bytes}/{total_bytes} ({binary_bytes_per_sec}, eta {eta}) {msg}",
        )
        .expect("valid progress template")
        .progress_chars("=> "),
    );
    bar.set_message("enumerating...");

    let finished = AtomicBool::new(false);
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !finished.load(Ordering::Relaxed) {
                let total = handle.total.load(Ordering::Relaxed);
                if total > 0 {
                    bar.set_length(total);
                    bar.set_message(format!(
                        "{} files, {} matches",
                        handle.files_done.load(Ordering::Relaxed),
                        handle.matches.load(Ordering::Relaxed)
                    ));
                }
                bar.set_position(handle.done.load(Ordering::Relaxed));
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let result = orchestrator.run_sorted();
        finished.store(true, Ordering::Relaxed);
        result
    });
    bar.finish_and_clear();
    result
}

fn init_tracing(verbose: u8, quiet: bool) {
    let level = log_level(verbose, quiet);
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(format!("{level},tantivy=warn"))))
        .with_target(false)
        .try_init();
}

fn parse_size_arg(s: &str) -> Result<usize, String> {
    prowlcat_core::parse_size(s).ok_or_else(|| format!("`{s}` is not a size: use a number of bytes, or 64K, 1M, 1G"))
}

const BANNER: &str = r"
      (\
       \ \
       / /            |\\
      / /    .-`````-. / ^`-.
      \ \   /         \_/{|} `o
       \ \ /  .---.   \\ _ ,--'
        \ \/  /     \,  \( `^^^
         \   \/\     (\  )
          \   ) \     ) \ \
           ) /__ \__  ) (\ \___
          (___)))__))(__))(__)))
  ProwlCat — local breach search
";

fn print_banner() {
    println!("{BANNER}");
}

#[cfg(test)]
mod tests {
    use super::*;

    use CommandKind::{None as Bare, Other, Tui as TuiCmd};
    use UiMode::{Auto, Cli as ForceCli, Tui as ForceTui};

    fn launch(ui: UiMode, command: CommandKind, interactive: bool) -> Launch {
        resolve_launch(ui, command, interactive).unwrap()
    }

    #[test]
    fn bare_command_opens_the_interface_only_in_a_terminal() {
        assert_eq!(launch(Auto, Bare, true), Launch::Tui);
        assert_eq!(launch(Auto, Bare, false), Launch::NoSubcommand, "pipes and scripts get the classic error");
    }

    #[test]
    fn ui_cli_pins_the_classic_behaviour_everywhere() {
        assert_eq!(launch(ForceCli, Bare, true), Launch::NoSubcommand);
        assert_eq!(launch(ForceCli, Bare, false), Launch::NoSubcommand);
        assert_eq!(launch(ForceCli, Other, true), Launch::Classic);
    }

    #[test]
    fn subcommands_stay_classic_even_in_a_terminal() {
        assert_eq!(launch(Auto, Other, true), Launch::Classic);
        assert_eq!(launch(Auto, Other, false), Launch::Classic);
    }

    #[test]
    fn ui_tui_forces_the_interface() {
        for interactive in [true, false] {
            assert_eq!(launch(ForceTui, Bare, interactive), Launch::Tui);
            assert_eq!(launch(ForceTui, Other, interactive), Launch::Tui);
            assert_eq!(launch(ForceTui, TuiCmd, interactive), Launch::Tui);
        }
    }

    #[test]
    fn the_tui_subcommand_always_means_the_interface() {
        assert_eq!(launch(Auto, TuiCmd, false), Launch::Tui);
        assert!(resolve_launch(ForceCli, TuiCmd, true).is_err(), "contradictory flags are an error, not a guess");
    }

    #[test]
    fn search_options_carry_over_to_the_interface() {
        let cli = Cli::try_parse_from([
            "prowlcat", "search", "acme.corp", "-p", "/data", "-t", "domain", "-j", "3", "-m", "10", "--ui", "tui",
        ])
        .unwrap();
        assert_eq!(cli.ui, UiMode::Tui);
        let Some(Commands::Search(args)) = cli.command else { panic!("expected search") };
        let opts = tui_from_search(args).unwrap();
        assert_eq!(opts.targets, vec!["acme.corp".to_string()]);
        assert_eq!(opts.paths, vec![PathBuf::from("/data")]);
        assert_eq!(opts.hint, TypeHint::Domain);
        assert_eq!(opts.threads, Some(3));
        assert_eq!(opts.max_matches, 10);
        assert_eq!(opts.start, Start::Search);
    }

    #[test]
    fn a_search_without_targets_opens_at_the_prompt() {
        let cli = Cli::try_parse_from(["prowlcat", "search", "-p", "/data", "--ui", "tui"]).unwrap();
        let Some(Commands::Search(args)) = cli.command else { panic!() };
        assert_eq!(tui_from_search(args).unwrap().start, Start::Home);
    }

    #[test]
    fn export_flags_cannot_be_silently_dropped() {
        let cli = Cli::try_parse_from(["prowlcat", "search", "x.com", "-p", "/d", "-o", "out.json"]).unwrap();
        let Some(Commands::Search(args)) = cli.command else { panic!() };
        let err = tui_from_search(args).unwrap_err().to_string();
        assert!(err.contains("--output"), "{err}");
    }

    #[test]
    fn tui_subcommand_options() {
        let cli = Cli::try_parse_from([
            "prowlcat", "tui", "a@b.com", "c@d.com", "-p", "/d", "--type", "email", "--no-animation", "--no-splash", "--no-history",
        ])
        .unwrap();
        let Some(Commands::Tui(args)) = cli.command else { panic!() };
        let opts = tui_from_args(args).unwrap();
        assert_eq!(opts.targets.len(), 2);
        assert_eq!(opts.hint, TypeHint::Email);
        assert!(!opts.animate && !opts.splash && !opts.persist);
        assert_eq!(opts.start, Start::Search);

        let cli = Cli::try_parse_from(["prowlcat", "tui"]).unwrap();
        let Some(Commands::Tui(args)) = cli.command else { panic!() };
        assert_eq!(tui_from_args(args).unwrap().start, Start::Home);
    }

    #[test]
    fn the_same_options_mean_the_same_in_every_subcommand() {
        let parse = |args: &[&str]| Cli::try_parse_from(["prowlcat"].iter().chain(args)).unwrap();
        // Search, tui, index and stats all take -p/-r/--no-recursive; the first three also -j and the passwords.
        for cmd in ["search", "tui", "index", "stats"] {
            let mut a = vec![cmd, "-p", "/d", "-r"];
            if cmd == "index" {
                a.extend(["-o", "/idx"]);
            }
            parse(&a);
            let mut a = vec![cmd, "-p", "/d", "--no-recursive"];
            if cmd == "index" {
                a.extend(["-o", "/idx"]);
            }
            parse(&a);
        }
        for cmd in ["search", "tui", "index"] {
            let mut a = vec![cmd, "-p", "/d", "-j", "2", "--password", "x", "--password-file", "/pw"];
            if cmd == "index" {
                a.extend(["-o", "/idx"]);
            }
            parse(&a);
        }
        for cmd in ["search", "tui"] {
            parse(&[cmd, "x", "-p", "/d", "-t", "domain", "-m", "5", "--max-line-bytes", "64K", "--snippet", "60"]);
        }
        assert!(Cli::try_parse_from(["prowlcat", "stats", "-p", "/d", "-r", "--no-recursive"]).is_err());
    }

    #[test]
    fn recursion_is_on_unless_turned_off() {
        let cli = Cli::try_parse_from(["prowlcat", "stats", "-p", "/d"]).unwrap();
        let Some(Commands::Stats(a)) = cli.command else { panic!() };
        assert!(a.walk.on());
        let cli = Cli::try_parse_from(["prowlcat", "stats", "-p", "/d", "--no-recursive"]).unwrap();
        let Some(Commands::Stats(a)) = cli.command else { panic!() };
        assert!(!a.walk.on());
    }

    #[test]
    fn line_limits_accept_sizes() {
        let cli = Cli::try_parse_from(["prowlcat", "tui", "--max-line-bytes", "1M", "--snippet", "60"]).unwrap();
        let Some(Commands::Tui(a)) = cli.command else { panic!() };
        assert_eq!((a.lines.max_line_bytes, a.lines.snippet), (Some(1 << 20), Some(60)));
        assert!(Cli::try_parse_from(["prowlcat", "tui", "--snippet", "lots"]).is_err());
    }

    #[test]
    fn loose_means_text() {
        let cli = Cli::try_parse_from(["prowlcat", "tui", "--loose"]).unwrap();
        let Some(Commands::Tui(args)) = cli.command else { panic!() };
        assert_eq!(tui_from_args(args).unwrap().hint, TypeHint::Text);
    }

    #[test]
    fn unknown_ui_mode_is_rejected_by_clap() {
        assert!(Cli::try_parse_from(["prowlcat", "--ui", "fancy", "stats", "-p", "/d"]).is_err());
    }

    #[test]
    fn the_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
