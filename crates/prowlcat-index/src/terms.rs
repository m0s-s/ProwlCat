use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Longest token we index; anything longer is almost certainly a hash or blob.
const MAX_TOKEN: usize = 255;

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'@' | b'+')
}

/// Extract the indexable terms of a line: lowercase tokens (emails, domains and every
/// parent-domain suffix of them) into `tokens`, and parsed IP addresses into `ips`.
///
/// Both sets are cleared first so the caller can reuse their allocations across lines.
pub(crate) fn extract_terms(line: &[u8], tokens: &mut HashSet<String>, ips: &mut HashSet<IpAddr>) {
    tokens.clear();
    ips.clear();

    let mut start = None;
    for (i, &b) in line.iter().enumerate() {
        match (is_token_byte(b), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                add_token(&line[s..i], tokens, ips);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        add_token(&line[s..], tokens, ips);
    }

    add_ipv6(line, ips);
}

fn add_token(raw: &[u8], tokens: &mut HashSet<String>, ips: &mut HashSet<IpAddr>) {
    // `raw` is pure ASCII by construction of `is_token_byte`.
    let Ok(raw) = std::str::from_utf8(raw) else { return };
    let tok = raw.trim_matches(|c: char| !c.is_ascii_alphanumeric()).to_ascii_lowercase();
    if tok.is_empty() || tok.len() > MAX_TOKEN {
        return;
    }
    if let Ok(v4) = tok.parse::<Ipv4Addr>() {
        ips.insert(IpAddr::V4(v4));
        return;
    }

    // Plain words are not searchable targets; only emails and dotted names are indexed.
    if !tok.contains('.') && !tok.contains('@') {
        return;
    }

    let host = match tok.rsplit_once('@') {
        Some((_, host)) => host,
        None => tok.as_str(),
    };
    // Every suffix after a dot, so a query for `example.com` finds `mail.example.com`.
    let mut rest = host;
    while let Some((_, tail)) = rest.split_once('.') {
        if !tail.is_empty() {
            tokens.insert(tail.to_string());
        }
        rest = tail;
    }
    if host.len() != tok.len() && !host.is_empty() {
        tokens.insert(host.to_string());
    }
    tokens.insert(tok);
}

/// IPv6 addresses contain `:`, which is a field separator everywhere else, so scan
/// maximal runs of hex digits, colons and dots separately.
fn add_ipv6(line: &[u8], ips: &mut HashSet<IpAddr>) {
    let mut start = None;
    for i in 0..=line.len() {
        let hit = line.get(i).is_some_and(|b| b.is_ascii_hexdigit() || matches!(b, b':' | b'.'));
        match (hit, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                let run = &line[s..i];
                if run.iter().filter(|&&b| b == b':').count() >= 2 {
                    if let Some(ip) = std::str::from_utf8(run)
                        .ok()
                        .and_then(|r| r.trim_matches(':').parse::<Ipv6Addr>().ok())
                    {
                        ips.insert(IpAddr::V6(ip));
                    }
                }
                start = None;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(line: &str) -> (HashSet<String>, HashSet<IpAddr>) {
        let (mut t, mut i) = (HashSet::new(), HashSet::new());
        extract_terms(line.as_bytes(), &mut t, &mut i);
        (t, i)
    }

    #[test]
    fn email_yields_full_host_and_suffixes() {
        let (t, _) = terms("Bob@Mail.Example.com:hunter2");
        for want in ["bob@mail.example.com", "mail.example.com", "example.com", "com"] {
            assert!(t.contains(want), "missing {want}: {t:?}");
        }
    }

    #[test]
    fn ips_are_extracted_not_tokenised() {
        let (t, i) = terms("10.0.0.7 login 2001:db8::1 ok");
        assert!(i.contains(&"10.0.0.7".parse().unwrap()));
        assert!(i.contains(&"2001:db8::1".parse().unwrap()));
        assert!(!t.contains("0.0.7"));
    }
}
