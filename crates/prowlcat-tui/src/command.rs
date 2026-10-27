//! What the prompt understands: plain targets and `/commands`.

use std::path::PathBuf;

use prowlcat_core::TypeHint;

/// `(name, one-line help)`, in the order shown by the palette.
pub const COMMANDS: &[(&str, &str)] = &[
    ("search", "find a target in the corpus"),
    ("index", "build, use or inspect the Tantivy index"),
    ("stats", "corpus statistics, no search"),
    ("path", "change corpus folder, files or globs"),
    ("type", "force the target type (auto, email, domain, ip, cidr, text)"),
    ("export", "write the last results to .json .csv .ndjson"),
    ("passwords", "candidate passwords for encrypted zips"),
    ("threads", "parallel workers"),
    ("max", "max matches per target (0 = unlimited)"),
    ("history", "clear the list of recent searches"),
    ("help", "keys and tips"),
    ("quit", "leave ProwlCat"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Run a search for these raw targets, optionally over a different corpus.
    Search { targets: Vec<String>, paths: Vec<PathBuf> },
    /// Open the corpus screen; `Stats` also refreshes the statistics.
    Corpus { refresh: bool },
    IndexBuild(PathBuf),
    IndexUse(PathBuf),
    IndexOff,
    SetPaths(Vec<PathBuf>),
    SetType(TypeHint),
    Export(Option<PathBuf>),
    Passwords(PathBuf),
    Threads(usize),
    MaxMatches(u64),
    ClearHistory,
    Help,
    Quit,
}

/// Commands whose name starts with `prefix` (no leading slash).
pub fn suggestions(prefix: &str) -> Vec<(&'static str, &'static str)> {
    let prefix = prefix.to_ascii_lowercase();
    COMMANDS.iter().copied().filter(|(name, _)| name.starts_with(&prefix)).collect()
}

pub fn hint_name(h: TypeHint) -> &'static str {
    match h {
        TypeHint::Auto => "auto",
        TypeHint::Email => "email",
        TypeHint::Domain => "domain",
        TypeHint::Ip => "ip",
        TypeHint::Cidr => "cidr",
        TypeHint::Text => "text",
    }
}

pub fn parse_hint(s: &str) -> Option<TypeHint> {
    Some(match s.to_ascii_lowercase().as_str() {
        "auto" => TypeHint::Auto,
        "email" => TypeHint::Email,
        "domain" => TypeHint::Domain,
        "ip" => TypeHint::Ip,
        "cidr" => TypeHint::Cidr,
        "text" => TypeHint::Text,
        _ => return None,
    })
}

/// Parse one line typed at the prompt.
pub fn parse(line: &str, hint: TypeHint) -> Result<Command, String> {
    let line = line.trim();
    if line.is_empty() {
        return Err("type a target, or / for commands".into());
    }
    let Some(rest) = line.strip_prefix('/') else {
        return parse_search(line, hint);
    };

    let (name, args) = match rest.split_once(char::is_whitespace) {
        Some((n, a)) => (n, a.trim()),
        None => (rest, ""),
    };
    let name = name.to_ascii_lowercase();
    // An unambiguous prefix is enough: `/sea` is `/search`.
    let name = match suggestions(&name).as_slice() {
        [(only, _)] => (*only).to_string(),
        _ if COMMANDS.iter().any(|(n, _)| *n == name) => name,
        [] => return Err(format!("unknown command /{name}; try /help")),
        many => {
            let names: Vec<_> = many.iter().map(|(n, _)| format!("/{n}")).collect();
            return Err(format!("ambiguous: {}", names.join(", ")));
        }
    };

    match name.as_str() {
        "search" => {
            if args.is_empty() {
                Err("usage: /search <target>...".into())
            } else {
                parse_search(args, hint)
            }
        }
        "stats" => Ok(Command::Corpus { refresh: true }),
        "index" => parse_index(args),
        "path" => {
            let paths = split_paths(args);
            if paths.is_empty() {
                Err("usage: /path <dir|file|glob>...".into())
            } else {
                Ok(Command::SetPaths(paths))
            }
        }
        "type" => parse_hint(args).map(Command::SetType).ok_or_else(|| "usage: /type auto|email|domain|ip|cidr|text".to_string()),
        "export" => Ok(Command::Export((!args.is_empty()).then(|| PathBuf::from(unquote(args))))),
        "passwords" => {
            if args.is_empty() {
                Err("usage: /passwords <file with one password per line>".into())
            } else {
                Ok(Command::Passwords(PathBuf::from(unquote(args))))
            }
        }
        "threads" => args
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .map(Command::Threads)
            .ok_or_else(|| "usage: /threads <n>, with n of at least 1".to_string()),
        "max" => args.parse::<u64>().map(Command::MaxMatches).map_err(|_| "usage: /max <n>, 0 for unlimited".to_string()),
        "history" => match args {
            "clear" => Ok(Command::ClearHistory),
            _ => Err("usage: /history clear".into()),
        },
        "help" => Ok(Command::Help),
        "quit" => Ok(Command::Quit),
        other => Err(format!("unknown command /{other}")),
    }
}

fn parse_index(args: &str) -> Result<Command, String> {
    let (sub, rest) = match args.split_once(char::is_whitespace) {
        Some((s, r)) => (s, r.trim()),
        None => (args, ""),
    };
    match (sub, rest) {
        ("", _) => Ok(Command::Corpus { refresh: false }),
        ("build", "") => Err("usage: /index build <output dir>".into()),
        ("build", dir) => Ok(Command::IndexBuild(PathBuf::from(unquote(dir)))),
        ("use", "") => Err("usage: /index use <index dir>".into()),
        ("use", dir) => Ok(Command::IndexUse(PathBuf::from(unquote(dir)))),
        ("off", _) => Ok(Command::IndexOff),
        _ => Err("usage: /index [build <dir> | use <dir> | off]".into()),
    }
}

/// Split a plain line into targets and `@path` tokens. Text searches keep the whole phrase.
fn parse_search(line: &str, hint: TypeHint) -> Result<Command, String> {
    let mut paths = Vec::new();
    let mut words = Vec::new();
    for token in line.split_whitespace() {
        match token.strip_prefix('@') {
            Some("") => return Err("`@` must be followed by a path, e.g. @~/breaches".into()),
            Some(p) => paths.push(PathBuf::from(expand_home(&unquote(p)))),
            None => words.push(token),
        }
    }
    if words.is_empty() {
        return Err("no target given".into());
    }
    let targets = if hint == TypeHint::Text {
        vec![words.join(" ")]
    } else {
        words.into_iter().map(str::to_string).collect()
    };
    Ok(Command::Search { targets, paths })
}

pub fn split_paths(args: &str) -> Vec<PathBuf> {
    args.split_whitespace().map(|p| PathBuf::from(expand_home(&unquote(p)))).collect()
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    for q in ['"', '\''] {
        if let Some(inner) = s.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
            return inner.to_string();
        }
    }
    s.to_string()
}

/// Expand a leading `~` to the home directory.
pub fn expand_home(p: &str) -> String {
    if p == "~" || p.starts_with("~/") || p.starts_with("~\\") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return format!("{}{}", home.to_string_lossy(), &p[1..]);
        }
    }
    p.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(targets: &[&str], paths: &[&str]) -> Command {
        Command::Search {
            targets: targets.iter().map(|s| s.to_string()).collect(),
            paths: paths.iter().map(PathBuf::from).collect(),
        }
    }

    #[test]
    fn plain_input_is_a_search() {
        assert_eq!(parse("acme.corp", TypeHint::Auto), Ok(search(&["acme.corp"], &[])));
        assert_eq!(
            parse("a@x.com b@y.com", TypeHint::Auto),
            Ok(search(&["a@x.com", "b@y.com"], &[]))
        );
    }

    #[test]
    fn text_type_keeps_the_phrase_together() {
        assert_eq!(parse("john smith", TypeHint::Text), Ok(search(&["john smith"], &[])));
    }

    #[test]
    fn at_tokens_select_the_corpus() {
        assert_eq!(parse("acme.corp @/data/leaks", TypeHint::Auto), Ok(search(&["acme.corp"], &["/data/leaks"])));
        assert!(parse("@/data", TypeHint::Auto).is_err(), "a path alone is not a target");
        assert!(parse("x @", TypeHint::Auto).is_err());
    }

    #[test]
    fn commands_accept_unambiguous_prefixes() {
        assert_eq!(parse("/sea acme.corp", TypeHint::Auto), Ok(search(&["acme.corp"], &[])));
        assert_eq!(parse("/q", TypeHint::Auto), Ok(Command::Quit));
        assert_eq!(parse("/st", TypeHint::Auto), Ok(Command::Corpus { refresh: true }));
        assert!(parse("/p", TypeHint::Auto).unwrap_err().contains("ambiguous"));
        assert!(parse("/nope", TypeHint::Auto).unwrap_err().contains("unknown"));
    }

    #[test]
    fn index_subcommands() {
        assert_eq!(parse("/index", TypeHint::Auto), Ok(Command::Corpus { refresh: false }));
        assert_eq!(parse("/index build out.idx", TypeHint::Auto), Ok(Command::IndexBuild("out.idx".into())));
        assert_eq!(parse("/index use \"my idx\"", TypeHint::Auto), Ok(Command::IndexUse("my idx".into())));
        assert_eq!(parse("/index off", TypeHint::Auto), Ok(Command::IndexOff));
        assert!(parse("/index build", TypeHint::Auto).is_err());
        assert!(parse("/index wat", TypeHint::Auto).is_err());
    }

    #[test]
    fn settings_validate_their_arguments() {
        assert_eq!(parse("/type email", TypeHint::Auto), Ok(Command::SetType(TypeHint::Email)));
        assert!(parse("/type nope", TypeHint::Auto).is_err());
        assert_eq!(parse("/threads 8", TypeHint::Auto), Ok(Command::Threads(8)));
        assert!(parse("/threads 0", TypeHint::Auto).is_err());
        assert_eq!(parse("/max 0", TypeHint::Auto), Ok(Command::MaxMatches(0)));
        assert!(parse("/max -1", TypeHint::Auto).is_err());
        assert_eq!(parse("/export", TypeHint::Auto), Ok(Command::Export(None)));
        assert_eq!(parse("/export out.csv", TypeHint::Auto), Ok(Command::Export(Some("out.csv".into()))));
        assert_eq!(parse("/history clear", TypeHint::Auto), Ok(Command::ClearHistory));
    }

    #[test]
    fn empty_input_asks_for_a_target() {
        assert!(parse("   ", TypeHint::Auto).is_err());
    }

    #[test]
    fn suggestions_filter_by_prefix() {
        let names: Vec<_> = suggestions("s").into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["search", "stats"]);
        assert_eq!(suggestions("").len(), COMMANDS.len());
        assert!(suggestions("zzz").is_empty());
    }

    #[test]
    fn home_is_expanded() {
        std::env::set_var("HOME", "/home/me");
        assert_eq!(expand_home("~/x"), "/home/me/x");
        assert_eq!(expand_home("/abs"), "/abs");
    }
}
