use aho_corasick::AhoCorasick;
use anyhow::Result;

use prowlcat_types::{ExtractedValue, MatchKind, Query, QueryKind};

use memchr::memmem::Finder;

use crate::cidr::{match_cidr_line, match_domain_in_line, match_ip_in_network, required_literal};

#[derive(Debug, Clone)]
pub struct LineHit {
    pub query_index: usize,
    pub match_kind: MatchKind,
    pub span: (usize, usize),
    pub extracted: Vec<ExtractedValue>,
}

pub struct MatchEngine {
    queries: Vec<Query>,
    ac: AhoCorasick,
    /// Maps Aho-Corasick pattern id → query index
    pattern_to_query: Vec<usize>,
    include_subdomains: bool,
    /// Per query: literal an IP must contain to be inside that CIDR (cheap line prefilter).
    cidr_prefilters: Vec<Option<Finder<'static>>>,
}

impl MatchEngine {
    pub fn build(queries: &[Query], include_subdomains: bool) -> Result<Self> {
        let mut patterns = Vec::new();
        let mut pattern_to_query = Vec::new();

        for (qi, q) in queries.iter().enumerate() {
            let bytes = q.normalized.as_bytes().to_vec();
            patterns.push(bytes);
            pattern_to_query.push(qi);
        }

        let ac = AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .build(&patterns)?;

        let cidr_prefilters = queries
            .iter()
            .map(|q| {
                q.network
                    .as_ref()
                    .and_then(required_literal)
                    .map(|lit| Finder::new(lit.as_bytes()).into_owned())
            })
            .collect();

        Ok(Self {
            queries: queries.to_vec(),
            cidr_prefilters,
            ac,
            pattern_to_query,
            include_subdomains,
        })
    }

    pub fn query_count(&self) -> usize {
        self.queries.len()
    }

    pub fn query_raw(&self, index: usize) -> &str {
        &self.queries[index].raw
    }

    pub fn query_kind(&self, index: usize) -> QueryKind {
        self.queries[index].kind
    }

    pub fn match_line(&self, line: &[u8]) -> Vec<LineHit> {
        let text = String::from_utf8_lossy(line);
        let mut hits = Vec::new();

        for mat in self.ac.find_iter(line) {
            let qi = self.pattern_to_query[mat.pattern().as_usize()];
            if let Some(hit) = self.match_query_at(qi, &text, mat.start()) {
                hits.push(hit);
            }
        }

        // CIDR membership: IPs in a subnet won't trigger the literal CIDR pattern.
        if line.iter().any(|b| b.is_ascii_digit()) {
            for (qi, q) in self.queries.iter().enumerate() {
                if q.kind != QueryKind::Cidr {
                    continue;
                }
                if hits.iter().any(|h| h.query_index == qi) {
                    continue;
                }
                if let Some(net) = &q.network {
                    if let Some(f) = &self.cidr_prefilters[qi] {
                        if f.find(line).is_none() {
                            continue;
                        }
                    }
                    // The literal-CIDR case was already reported via Aho-Corasick above.
                    if let Some((kind, span, extracted)) = match_ip_in_network(&text, net) {
                        hits.push(LineHit {
                            query_index: qi,
                            match_kind: kind,
                            span,
                            extracted,
                        });
                    }
                }
            }
        }

        hits
    }

    fn match_query_at(&self, qi: usize, text: &str, hint_start: usize) -> Option<LineHit> {
        let q = &self.queries[qi];

        match q.kind {
            QueryKind::Email => {
                let (start, end) = find_token_span(text, hint_start, &q.normalized)?;
                Some(LineHit {
                    query_index: qi,
                    match_kind: MatchKind::Exact,
                    span: (start, end),
                    extracted: vec![ExtractedValue::Email(q.raw.clone())],
                })
            }
            QueryKind::Domain => {
                let (start, end) = match_domain_in_line(&text, &q.normalized, self.include_subdomains)?;
                Some(LineHit {
                    query_index: qi,
                    match_kind: MatchKind::Exact,
                    span: (start, end),
                    extracted: vec![ExtractedValue::Domain(q.normalized.clone())],
                })
            }
            QueryKind::Ip => {
                let (start, end) = find_token_span(text, hint_start, &q.normalized)?;
                Some(LineHit {
                    query_index: qi,
                    match_kind: MatchKind::Exact,
                    span: (start, end),
                    extracted: vec![ExtractedValue::Ip(q.normalized.clone())],
                })
            }
            QueryKind::Cidr => {
                let net = q.network.as_ref()?;
                let (kind, span, extracted) = match_cidr_line(text, net, &q.normalized)?;
                Some(LineHit {
                    query_index: qi,
                    match_kind: kind,
                    span,
                    extracted,
                })
            }
            QueryKind::Text => {
                let needle_len = q.normalized.len();
                let end = (hint_start + needle_len).min(text.len());
                Some(LineHit {
                    query_index: qi,
                    match_kind: MatchKind::Exact,
                    span: (hint_start, end),
                    extracted: vec![ExtractedValue::Text(q.raw.clone())],
                })
            }
        }
    }
}

fn find_token_span(text: &str, hint_start: usize, needle: &str) -> Option<(usize, usize)> {
    let lower = text.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    let start = lower.find(&needle)?;
    let end = start + needle.len();

    let before_ok = start == 0 || !is_token_char(lower.as_bytes()[start - 1]);
    let after_ok = end >= lower.len() || !is_token_char(lower.as_bytes()[end]);

    if before_ok && after_ok {
        Some((start, end))
    } else if hint_start <= text.len() {
        // fallback: accept AC hit position
        Some((hint_start, (hint_start + needle.len()).min(text.len())))
    } else {
        None
    }
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_' || b == b'@'
}
