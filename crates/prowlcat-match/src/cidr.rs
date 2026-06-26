use std::net::IpAddr;
use std::sync::LazyLock;

use prowlcat_types::{ExtractedValue, MatchKind};
use ipnet::IpNet;
use regex::Regex;

static RE_IPV4: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)",
    )
    .unwrap()
});

static RE_IPV6: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([0-9A-Fa-f]{0,4}:){2,7}[0-9A-Fa-f]{0,4}").unwrap()
});

fn ipv4_boundary_ok(line: &str, start: usize, end: usize) -> bool {
    let before_ok = start == 0 || !line.as_bytes()[start - 1].is_ascii_digit();
    let after_ok = end >= line.len() || !line.as_bytes()[end].is_ascii_digit();
    before_ok && after_ok
}

pub fn extract_ips(line: &str) -> Vec<(usize, usize, IpAddr)> {
    let mut found = Vec::new();
    for m in RE_IPV4.find_iter(line) {
        if !ipv4_boundary_ok(line, m.start(), m.end()) {
            continue;
        }
        if let Ok(ip) = m.as_str().parse::<IpAddr>() {
            found.push((m.start(), m.end(), ip));
        }
    }
    for m in RE_IPV6.find_iter(line) {
        let start = m.start();
        let end = m.end();
        let before_ok = start == 0 || !line.as_bytes()[start - 1].is_ascii_hexdigit();
        let after_ok = end >= line.len() || !line.as_bytes()[end].is_ascii_hexdigit();
        if !before_ok || !after_ok {
            continue;
        }
        if let Ok(ip) = m.as_str().parse::<IpAddr>() {
            found.push((start, end, ip));
        }
    }
    found
}

pub fn match_cidr_line(
    line: &str,
    network: &IpNet,
    cidr_literal: &str,
) -> Option<(MatchKind, (usize, usize), Vec<ExtractedValue>)> {
    if let Some(pos) = line.find(cidr_literal) {
        let end = pos + cidr_literal.len();
        return Some((
            MatchKind::CidrLiteral,
            (pos, end),
            vec![ExtractedValue::Cidr(cidr_literal.to_string())],
        ));
    }

    for (start, end, ip) in extract_ips(line) {
        if network.contains(&ip) {
            return Some((
                MatchKind::IpInNetwork,
                (start, end),
                vec![ExtractedValue::Ip(ip.to_string())],
            ));
        }
    }

    None
}

pub fn domain_boundary_ok(line: &str, start: usize, end: usize) -> bool {
    let before = start > 0 && line.as_bytes()[start - 1].is_ascii_alphanumeric();
    let after = end < line.len() && line.as_bytes()[end].is_ascii_alphanumeric();
    !before && !after
}

pub fn match_domain_in_line(
    line: &str,
    domain: &str,
    include_subdomains: bool,
) -> Option<(usize, usize)> {
    let lower = line.to_ascii_lowercase();
    let domain = domain.to_ascii_lowercase();

    let needles: Vec<String> = if include_subdomains {
        vec![
            format!("@{}", domain),
            format!(".{}", domain),
            format!("://{}", domain),
            format!("://{}", domain.trim_start_matches("www.")),
        ]
    } else {
        vec![
            format!("@{}", domain),
            format!("@www.{}", domain),
            format!("://{}", domain),
            format!("://www.{}", domain),
        ]
    };

    for needle in needles {
        if let Some(pos) = lower.find(&needle) {
            let end = pos + needle.len();
            if domain_boundary_ok(&lower, pos.saturating_sub(1), end) || needle.starts_with('@') {
                return Some((pos, end));
            }
        }
    }

    if lower == domain && domain_boundary_ok(&lower, 0, domain.len()) {
        return Some((0, domain.len()));
    }

    None
}
