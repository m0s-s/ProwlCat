use std::net::IpAddr;
use std::sync::LazyLock;

use prowlcat_types::{ExtractedValue, MatchKind};
use ipnet::IpNet;
use regex::Regex;

/// Reference IPv4 matcher kept for differential testing of [`scan_ipv4`].
#[cfg(test)]
static RE_IPV4: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)",
    )
    .unwrap()
});

static RE_IPV6: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([0-9A-Fa-f]{0,4}:){2,7}[0-9A-Fa-f]{0,4}").unwrap()
});

/// Parse one dotted-quad octet starting at `i`: 1-3 digits, <= 255, no leading zero
/// (matching what `IpAddr::from_str` accepts). Returns (value, index after the digits).
#[inline]
fn octet(b: &[u8], i: usize) -> Option<(u8, usize)> {
    let mut j = i;
    let mut v: u32 = 0;
    while j < b.len() && b[j].is_ascii_digit() {
        v = v * 10 + (b[j] - b'0') as u32;
        j += 1;
        if j - i > 3 {
            return None;
        }
    }
    let len = j - i;
    if len == 0 || v > 255 || (len > 1 && b[i] == b'0') {
        return None;
    }
    Some((v as u8, j))
}

/// Scan for IPv4 addresses on digit boundaries, calling `f(start, end, ip)`; stop when `f` returns true.
fn scan_ipv4(line: &str, f: &mut impl FnMut(usize, usize, IpAddr) -> bool) -> bool {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // `i` is the start of a digit run, so the left boundary is satisfied.
        let start = i;
        let mut pos = i;
        let mut oct = [0u8; 4];
        let mut ok = true;
        for (n, slot) in oct.iter_mut().enumerate() {
            match octet(b, pos) {
                Some((v, next)) => {
                    *slot = v;
                    pos = next;
                }
                None => {
                    ok = false;
                    break;
                }
            }
            if n < 3 {
                if pos < b.len() && b[pos] == b'.' {
                    pos += 1;
                } else {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            // right boundary: the last octet already consumed its whole digit run
            if f(start, pos, IpAddr::from(oct)) {
                return true;
            }
            i = pos;
        } else {
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    false
}

fn scan_ipv6(line: &str, f: &mut impl FnMut(usize, usize, IpAddr) -> bool) -> bool {
    if line.bytes().filter(|&c| c == b':').take(2).count() < 2 {
        return false;
    }
    for m in RE_IPV6.find_iter(line) {
        let (start, end) = (m.start(), m.end());
        let before_ok = start == 0 || !line.as_bytes()[start - 1].is_ascii_hexdigit();
        let after_ok = end >= line.len() || !line.as_bytes()[end].is_ascii_hexdigit();
        if !before_ok || !after_ok {
            continue;
        }
        if let Ok(ip) = m.as_str().parse::<IpAddr>() {
            if f(start, end, ip) {
                return true;
            }
        }
    }
    false
}

/// Visit IP addresses in `line` (IPv4 first, then IPv6); stops early when `f` returns true.
fn scan_ips(line: &str, mut f: impl FnMut(usize, usize, IpAddr) -> bool) {
    if line.as_bytes().contains(&b'.') && scan_ipv4(line, &mut f) {
        return;
    }
    scan_ipv6(line, &mut f);
}

#[cfg(test)]
pub fn extract_ips(line: &str) -> Vec<(usize, usize, IpAddr)> {
    let mut found = Vec::new();
    scan_ips(line, |s, e, ip| {
        found.push((s, e, ip));
        false
    });
    found
}

/// First IP in `line` that lies inside `network`. Only the network's address family is scanned.
pub fn match_ip_in_network(
    line: &str,
    network: &IpNet,
) -> Option<(MatchKind, (usize, usize), Vec<ExtractedValue>)> {
    let mut hit = None;
    let mut visit = |start: usize, end: usize, ip: IpAddr| {
        if network.contains(&ip) {
            hit = Some((start, end, ip));
            true
        } else {
            false
        }
    };
    match network {
        IpNet::V4(_) => {
            if line.as_bytes().contains(&b'.') {
                scan_ipv4(line, &mut visit);
            }
        }
        IpNet::V6(_) => {
            scan_ipv6(line, &mut visit);
        }
    }
    hit.map(|(start, end, ip)| {
        (MatchKind::IpInNetwork, (start, end), vec![ExtractedValue::Ip(ip.to_string())])
    })
}

/// Text every IPv4 address inside `network` must contain (its fixed leading octets), used as a
/// cheap per-line prefilter. `None` when the prefix covers less than one whole octet or for IPv6.
pub fn required_literal(network: &IpNet) -> Option<String> {
    match network {
        IpNet::V4(n) => {
            let octets = n.network().octets();
            let k = (n.prefix_len() / 8) as usize;
            if k == 0 {
                return None;
            }
            let mut s = octets[..k.min(4)].iter().map(u8::to_string).collect::<Vec<_>>().join(".");
            if k < 4 {
                s.push('.');
            }
            Some(s)
        }
        IpNet::V6(_) => None,
    }
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

    let mut hit = None;
    scan_ips(line, |start, end, ip| {
        if network.contains(&ip) {
            hit = Some((start, end, ip));
            true
        } else {
            false
        }
    });
    hit.map(|(start, end, ip)| {
        (MatchKind::IpInNetwork, (start, end), vec![ExtractedValue::Ip(ip.to_string())])
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ipv4_boundary_ok(line: &str, start: usize, end: usize) -> bool {
        let b = line.as_bytes();
        (start == 0 || !b[start - 1].is_ascii_digit()) && (end >= b.len() || !b[end].is_ascii_digit())
    }

    fn regex_ipv4(line: &str) -> Vec<(usize, usize, IpAddr)> {
        RE_IPV4
            .find_iter(line)
            .filter(|m| ipv4_boundary_ok(line, m.start(), m.end()))
            .filter_map(|m| m.as_str().parse::<IpAddr>().ok().map(|ip| (m.start(), m.end(), ip)))
            .collect()
    }

    fn fast_ipv4(line: &str) -> Vec<(usize, usize, IpAddr)> {
        let mut v = Vec::new();
        scan_ipv4(line, &mut |s, e, ip| {
            v.push((s, e, ip));
            false
        });
        v
    }

    #[test]
    fn ipv4_scanner_matches_regex_reference() {
        let cases = [
            "10.0.0.1", "1.2.3.4.5", "1234.5.6.7", "300.1.1.1", "01.2.3.4", "1.2.3.456",
            "1.2.3.2555", "x1.2.3.4y", "a 192.168.0.1 b 10.20.30.40", "1.2.3", "...", "1.2.3.4.5.6.7.8",
            "255.255.255.255", "256.1.1.1", "0.0.0.0", "1..2.3.4", "9.9.9.9:80", "ip=8.8.8.8,1.1.1.1",
            "007.1.1.1", "1.2.3.x 4.5.6.7", "12.34.56.78.90",
        ];
        for c in cases {
            assert_eq!(fast_ipv4(c), regex_ipv4(c), "mismatch on {c:?}");
        }
        // Deterministic pseudo-random fuzz over a small alphabet.
        let alphabet = b"0123456789..:ax ";
        let mut x: u64 = 0x1234_5678_9abc_def1;
        for _ in 0..20_000 {
            let len = {
                x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                (x % 24) as usize
            };
            let mut line = String::new();
            for _ in 0..len {
                x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                line.push(alphabet[(x % alphabet.len() as u64) as usize] as char);
            }
            // The regex drops a valid address that overlaps a boundary-rejected candidate
            // (e.g. "0599.5.7.85.2a"); the scanner finds it. So require a superset, not equality.
            let fast = fast_ipv4(&line);
            for hit in regex_ipv4(&line) {
                assert!(fast.contains(&hit), "scanner missed {hit:?} in {line:?}");
            }
        }
    }

    #[test]
    fn cidr_membership_and_literal() {
        let net: IpNet = "10.0.0.0/8".parse().unwrap();
        let (k, span, _) = match_cidr_line("login from 10.23.1.5 ok", &net, "10.0.0.0/8").unwrap();
        assert_eq!(k, MatchKind::IpInNetwork);
        assert_eq!(span, (11, 20));
        assert!(match_cidr_line("login from 11.23.1.5 ok", &net, "10.0.0.0/8").is_none());
        let (k, _, _) = match_cidr_line("range 10.0.0.0/8 blocked", &net, "10.0.0.0/8").unwrap();
        assert_eq!(k, MatchKind::CidrLiteral);
    }

    #[test]
    fn required_literal_prefixes() {
        let lit = |c: &str| required_literal(&c.parse::<IpNet>().unwrap());
        assert_eq!(lit("10.0.0.0/8").as_deref(), Some("10."));
        assert_eq!(lit("172.16.0.0/12").as_deref(), Some("172."));
        assert_eq!(lit("192.168.4.0/24").as_deref(), Some("192.168.4."));
        assert_eq!(lit("10.1.2.3/32").as_deref(), Some("10.1.2.3"));
        assert_eq!(lit("128.0.0.0/4"), None);
        assert_eq!(lit("0.0.0.0/0"), None);
        assert_eq!(lit("2001:db8::/32"), None);
    }

    #[test]
    fn only_matching_family_is_scanned() {
        let v4: IpNet = "10.0.0.0/8".parse().unwrap();
        let v6: IpNet = "2001:db8::/32".parse().unwrap();
        let line = "a 10.1.2.3 b 2001:db8::1 c";
        assert_eq!(format!("{:?}", match_ip_in_network(line, &v4).unwrap().2), r#"[Ip("10.1.2.3")]"#);
        assert_eq!(format!("{:?}", match_ip_in_network(line, &v6).unwrap().2), r#"[Ip("2001:db8::1")]"#);
        assert!(match_ip_in_network("no addresses here", &v4).is_none());
    }

    #[test]
    fn ipv6_still_extracted() {
        let ips = extract_ips("addr 2001:db8::1 end");
        assert_eq!(ips.len(), 1);
    }
}
