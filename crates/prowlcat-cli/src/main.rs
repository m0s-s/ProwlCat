mod commands;

use std::path::PathBuf;
use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use prowlcat_core::{ProgressHandle, ScanConfig, ScanOrchestrator, TargetParser, TypeHint};
use prowlcat_output::{infer_format, print_results, write_output, OutputFormat};

#[derive(Parser)]
#[command(
    name = "prowlcat",
    version,
    about = "ProwlCat — local-first breach corpus search engine",
    long_about = "ProwlCat: high-performance local search across plaintext and compressed breach datasets."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(short, long, global = true)]
    quiet: bool,

    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Commands {
    /// Search breach files for emails, domains, IPs, CIDR ranges, or plain text
    Search(SearchArgs),
    /// Build a Tantivy index of a corpus for fast repeated searches
    Index(IndexArgs),
    /// Show corpus statistics without searching
    Stats(StatsArgs),
}

#[derive(Parser)]
struct SearchArgs {
    /// Targets: email, domain, IP, CIDR, or plain text
    #[arg(value_name = "TARGET")]
    targets: Vec<String>,

    /// File with one target per line
    #[arg(short = 'f', long)]
    targets_file: Option<PathBuf>,

    /// Files, directories, or globs to scan
    #[arg(short = 'p', long = "path", required_unless_present = "index")]
    paths: Vec<PathBuf>,

    /// Search a prebuilt index (see `prowlcat index`) instead of scanning files; text/--loose targets are unsupported
    #[arg(long, conflicts_with = "paths")]
    index: Option<PathBuf>,

    /// Recurse into subdirectories
    #[arg(short = 'r', long, default_value_t = true)]
    recursive: bool,

    /// Force target type: auto, email, domain, ip, cidr, text
    #[arg(short = 't', long = "type", default_value = "auto")]
    target_type: String,

    /// Plain-text substring search (case-insensitive, no type inference)
    #[arg(long)]
    loose: bool,

    /// Parallel worker threads
    #[arg(short = 'j', long)]
    threads: Option<usize>,

    /// Max matches per target (0 = unlimited)
    #[arg(short = 'm', long, default_value_t = 0)]
    max_matches: u64,

    /// Export results to file (.json, .csv, .ndjson)
    #[arg(short = 'o', long)]
    output: Option<PathBuf>,

    /// Output format override
    #[arg(long)]
    format: Option<String>,

    /// Password for encrypted zip archives (repeatable; visible in process list, prefer --password-file)
    #[arg(long = "password")]
    passwords: Vec<String>,

    /// File with one candidate archive password per line
    #[arg(long)]
    password_file: Option<PathBuf>,

    /// Disable progress output
    #[arg(long)]
    no_progress: bool,
}

#[derive(Parser)]
struct IndexArgs {
    /// Files, directories, or globs to index
    #[arg(short = 'p', long = "path", required = true)]
    paths: Vec<PathBuf>,

    /// Directory to create the index in (must not exist or be empty)
    #[arg(short = 'o', long, required = true)]
    output: PathBuf,

    /// Recurse into subdirectories
    #[arg(short = 'r', long, default_value_t = true)]
    recursive: bool,

    /// Parallel worker threads
    #[arg(short = 'j', long)]
    threads: Option<usize>,

    /// Password for encrypted zip archives (repeatable)
    #[arg(long = "password")]
    passwords: Vec<String>,

    /// File with one candidate archive password per line
    #[arg(long)]
    password_file: Option<PathBuf>,
}

#[derive(Parser)]
struct StatsArgs {
    #[arg(short = 'p', long = "path", required = true)]
    paths: Vec<PathBuf>,

    #[arg(short = 'r', long, default_value_t = true)]
    recursive: bool,
}

fn main() -> ExitCode {
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
    init_tracing(cli.verbose, cli.quiet);

    match cli.command {
        Commands::Search(args) => run_search(args),
        Commands::Index(args) => run_index(args),
        Commands::Stats(args) => commands::stats::run(args.paths, args.recursive),
    }
}

fn run_search(args: SearchArgs) -> Result<ExitCode> {
    if !args.no_progress {
        print_banner();
    }

    let hint = if args.loose {
        TypeHint::Text
    } else {
        parse_type_hint(&args.target_type)?
    };
    let parser = TargetParser::new(hint);

    let mut raw_targets = args.targets;
    if let Some(file) = args.targets_file {
        let content = std::fs::read_to_string(&file)
            .with_context(|| format!("read targets file {}", file.display()))?;
        raw_targets.extend(
            content
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_string),
        );
    }

    if raw_targets.is_empty() {
        anyhow::bail!("no targets provided (positional args or --targets-file)");
    }

    let queries = parser
        .parse_many(raw_targets)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let passwords = load_passwords(args.passwords, args.password_file.as_deref())?;

    let mut config = ScanConfig {
        passwords,
        paths: args.paths,
        recursive: args.recursive,
        max_matches_per_query: args.max_matches,
        ..ScanConfig::default()
    };
    if let Some(t) = args.threads {
        config.threads = t;
    }

    // Progress bar only on an interactive stderr, so piped/exported output stays clean.
    let show_progress = !args.no_progress && std::io::stderr().is_terminal();
    let progress = show_progress.then(ProgressHandle::default);
    config.progress = progress.clone();

    let start = Instant::now();
    let (mut results, mut summary) = if let Some(dir) = &args.index {
        let searcher = prowlcat_index::IndexSearcher::open(dir)?;
        searcher.search(&queries, config.include_subdomains, config.max_matches_per_query)?
    } else {
        let orchestrator = ScanOrchestrator::new(config, queries);
        match &progress {
            Some(handle) => run_with_progress(&orchestrator, handle)?,
            None => orchestrator.run()?,
        }
    };
    summary.elapsed_ms = start.elapsed().as_millis() as u64;

    // Stable sort for readable output
    results.sort_by(|a, b| {
        a.query
            .cmp(&b.query)
            .then(a.source.path.cmp(&b.source.path))
            .then(a.source.line_number.cmp(&b.source.line_number))
    });

    if let Some(path) = args.output {
        let format = args
            .format
            .as_deref()
            .map(parse_output_format)
            .transpose()?
            .unwrap_or_else(|| infer_format(&path));
        write_output(&path, format, &results, &summary)?;
    } else {
        print_results(&results, &summary);
    }

    Ok(if summary.matches > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
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
        recursive: args.recursive,
        threads: args.threads.unwrap_or(defaults.threads),
        mmap_threshold_mb: defaults.mmap_threshold_mb,
        passwords: load_passwords(args.passwords, args.password_file.as_deref())?,
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
) -> Result<(Vec<prowlcat_core::ScanResult>, prowlcat_core::ScanSummary)> {
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
        let result = orchestrator.run();
        finished.store(true, Ordering::Relaxed);
        result
    });
    bar.finish_and_clear();
    result
}

fn init_tracing(verbose: u8, quiet: bool) {
    let level = if quiet {
        "warn"
    } else if verbose > 0 {
        "debug"
    } else {
        "info"
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(format!("{level},tantivy=warn"))))
        .with_target(false)
        .try_init();
}

fn parse_type_hint(s: &str) -> Result<TypeHint> {
    match s.to_ascii_lowercase().as_str() {
        "auto" => Ok(TypeHint::Auto),
        "email" => Ok(TypeHint::Email),
        "domain" => Ok(TypeHint::Domain),
        "ip" => Ok(TypeHint::Ip),
        "cidr" => Ok(TypeHint::Cidr),
        "text" => Ok(TypeHint::Text),
        other => anyhow::bail!("unknown target type: {other}"),
    }
}

fn parse_output_format(s: &str) -> Result<OutputFormat> {
    match s.to_ascii_lowercase().as_str() {
        "table" => Ok(OutputFormat::Table),
        "json" => Ok(OutputFormat::Json),
        "ndjson" | "jsonl" => Ok(OutputFormat::Ndjson),
        "csv" => Ok(OutputFormat::Csv),
        other => anyhow::bail!("unknown output format: {other}"),
    }
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
