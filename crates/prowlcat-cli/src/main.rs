mod commands;

use std::path::PathBuf;
use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use tracing_subscriber::EnvFilter;

use prowlcat_core::{ProgressHandle, ScanConfig, ScanOrchestrator, SortedResults, TargetParser, TypeHint};
use prowlcat_output::{infer_format, print_results_iter, write_output_iter, OutputFormat};
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

    #[arg(short, long, global = true)]
    quiet: bool,

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
}

#[derive(Parser)]
struct TuiArgs {
    /// Targets to search straight away; leave empty to start at the prompt
    #[arg(value_name = "TARGET")]
    targets: Vec<String>,

    /// Files, directories, or globs to scan (remembered from the last session when omitted)
    #[arg(short = 'p', long = "path")]
    paths: Vec<PathBuf>,

    /// Search a prebuilt index instead of scanning files
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

    /// Password for encrypted zip archives (repeatable; visible in process list, prefer --password-file)
    #[arg(long = "password")]
    passwords: Vec<String>,

    /// File with one candidate archive password per line
    #[arg(long)]
    password_file: Option<PathBuf>,

    /// Hold a still frame instead of animating Fang (also PROWLCAT_NO_ANIM=1)
    #[arg(long)]
    no_animation: bool,

    /// Skip the intro
    #[arg(long)]
    no_splash: bool,

    /// Do not remember the corpus or recent searches (also PROWLCAT_NO_HISTORY=1)
    #[arg(long)]
    no_history: bool,
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
                Some(Commands::Stats(args)) => commands::stats::run(args.paths, args.recursive),
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
            recursive: args.recursive,
            threads: args.threads,
            passwords: load_passwords(args.passwords, args.password_file.as_deref())?,
            start: Start::IndexBuild(args.output),
            ..TuiOptions::default()
        },
        Some(Commands::Stats(args)) => {
            TuiOptions { paths: args.paths, recursive: args.recursive, start: Start::Stats, ..TuiOptions::default() }
        }
    };
    prowlcat_tui::run(options)?;
    Ok(ExitCode::SUCCESS)
}

fn tui_from_args(args: TuiArgs) -> Result<TuiOptions> {
    let hint = if args.loose { TypeHint::Text } else { parse_type_hint(&args.target_type)? };
    let start = if args.targets.is_empty() { Start::Home } else { Start::Search };
    let defaults = TuiOptions::default();
    Ok(TuiOptions {
        targets: args.targets,
        hint,
        paths: args.paths,
        index: args.index,
        recursive: args.recursive,
        threads: args.threads,
        max_matches: args.max_matches,
        passwords: load_passwords(args.passwords, args.password_file.as_deref())?,
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
    let hint = if args.loose { TypeHint::Text } else { parse_type_hint(&args.target_type)? };
    let mut targets = args.targets;
    if let Some(file) = &args.targets_file {
        targets.extend(read_targets(file)?);
    }
    let start = if targets.is_empty() { Start::Home } else { Start::Search };
    Ok(TuiOptions {
        targets,
        hint,
        paths: args.paths,
        index: args.index,
        recursive: args.recursive,
        threads: args.threads,
        max_matches: args.max_matches,
        passwords: load_passwords(args.passwords, args.password_file.as_deref())?,
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
        raw_targets.extend(read_targets(&file)?);
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

    if let Some(path) = args.output {
        let format = args
            .format
            .as_deref()
            .map(parse_output_format)
            .transpose()?
            .unwrap_or_else(|| infer_format(&path));
        write_output_iter(&path, format, found.iter(), found.summary())?;
    } else {
        print_results_iter(found.iter(), found.summary())?;
    }

    Ok(if found.summary().matches > 0 {
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
