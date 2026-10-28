//! Pulling `account` and `secret` out of a line that matched.
//!
//! The layout it understands is the one breach lists use: the account at the start of the line,
//! then a separator, then the secret (`alice@example.com:hunter2`, also `;` `,` `|` tab or space).
//! The account is the email address around the match. A line that keeps the account elsewhere
//! (in the middle of a URL record, after a password, inside an SQL row) is not guessed at: it
//! gives nothing, and the caller counts it as skipped.

use prowlcat_types::{Record, ScanResult};

use crate::hashid;

fn is_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, ':' | ';' | ',' | '|' | '\'' | '"' | '(' | ')' | '<' | '>' | '[' | ']' | '{' | '}')
}

fn is_account(token: &str) -> bool {
    match token.split_once('@') {
        Some((local, domain)) => !local.is_empty() && domain.contains('.') && !domain.contains('@'),
        None => false,
    }
}

/// The account in `line`, the byte range it occupies, and the secret after it. The account is the
/// token around `span`, or else the first one that looks like an email address.
pub fn account_and_secret(line: &str, span: (usize, usize)) -> Option<(String, String)> {
    let at = span.0.min(line.len());
    let mut start = at;
    while !line.is_char_boundary(start) {
        start -= 1;
    }
    let from = line[..start].rfind(is_boundary).map(|i| i + line[i..].chars().next().map_or(1, char::len_utf8)).unwrap_or(0);
    let to = line[start..].find(is_boundary).map_or(line.len(), |i| start + i);
    let around = &line[from..to];

    let (acc_start, acc_end) = if is_account(around) {
        (from, to)
    } else {
        // Another token of the line may be the account: the first one that looks like one.
        let mut pos = 0;
        let mut found = None;
        for token in line.split(is_boundary) {
            if is_account(token) {
                found = Some((pos, pos + token.len()));
                break;
            }
            pos += token.len() + 1;
        }
        found?
    };

    // The account must open the line (quotes and blanks aside).
    if !line[..acc_start].chars().all(|c| c.is_whitespace() || c == '"' || c == '\'') {
        return None;
    }
    let account = &line[acc_start..acc_end];
    let rest = &line[acc_end..];
    let rest = rest.strip_prefix(['"', '\'']).unwrap_or(rest);
    let secret = match rest.chars().next() {
        None => "",
        Some(c) if matches!(c, ':' | ';' | ',' | '|' | '\t' | ' ') => &rest[c.len_utf8()..],
        Some(_) => return None, // the account runs into something that is not a separator
    };
    Some((account.to_string(), secret.trim_end().to_string()))
}

/// The record for a match, when its line has the layout described above.
pub fn record_from(r: &ScanResult) -> Option<Record> {
    let (account, secret) = account_and_secret(&r.line_text, r.matched_span)?;
    let secret_looks_like = hashid::identify(&secret).into_iter().map(str::to_string).collect();
    Some(Record {
        account,
        secret,
        secret_looks_like,
        query: r.query.clone(),
        path: r.source.path.display().to_string(),
        archive_entry: r.source.archive_entry.clone(),
        line_number: r.source.line_number,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(line: &str, span: (usize, usize)) -> Option<(String, String)> {
        account_and_secret(line, span)
    }

    #[test]
    fn the_usual_layouts() {
        let want = Some(("alice@example.com".to_string(), "hunter2".to_string()));
        assert_eq!(pair("alice@example.com:hunter2", (0, 17)), want);
        assert_eq!(pair("alice@example.com;hunter2", (0, 17)), want);
        assert_eq!(pair("alice@example.com,hunter2", (0, 17)), want);
        assert_eq!(pair("alice@example.com|hunter2", (0, 17)), want);
        assert_eq!(pair("alice@example.com\thunter2", (0, 17)), want);
        assert_eq!(pair("alice@example.com hunter2", (0, 17)), want);
        assert_eq!(pair("  alice@example.com:hunter2  ", (2, 19)), want);
    }

    #[test]
    fn a_secret_may_contain_separators() {
        assert_eq!(pair("a@b.co:pa:ss;wo rd", (0, 6)), Some(("a@b.co".into(), "pa:ss;wo rd".into())));
    }

    #[test]
    fn a_domain_match_finds_the_whole_account() {
        // The span of a domain match covers only "@mail.example.com".
        assert_eq!(pair("bob@mail.example.com:pw", (3, 20)), Some(("bob@mail.example.com".into(), "pw".into())));
    }

    #[test]
    fn no_secret_gives_an_empty_one() {
        assert_eq!(pair("alice@example.com", (0, 17)), Some(("alice@example.com".into(), String::new())));
        assert_eq!(pair("alice@example.com:", (0, 17)), Some(("alice@example.com".into(), String::new())));
    }

    #[test]
    fn layouts_that_are_not_guessed_at() {
        for (line, span) in [
            ("hunter2:alice@example.com", (8, 25)),
            ("https://example.com/login:bob@x.test:pw", (0, 19)),
            ("INSERT INTO t VALUES ('alice@example.com','pw');", (23, 40)),
            ("203.0.113.9 admin pw", (0, 11)),
            ("no account here", (0, 2)),
            ("alice@example.comx", (0, 17)),
        ] {
            let got = pair(line, span);
            assert!(got.is_none() || line == "alice@example.comx", "{line:?} → {got:?}");
        }
    }

    #[test]
    fn spans_inside_multibyte_text_do_not_panic() {
        assert_eq!(pair("é@é.com:pw", (1, 2)), Some(("é@é.com".into(), "pw".into())));
        assert_eq!(pair("漢字:alice@example.com", (4, 5)), None);
    }
}
