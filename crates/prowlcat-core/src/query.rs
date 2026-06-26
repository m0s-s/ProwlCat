use std::net::IpAddr;
use std::str::FromStr;

use prowlcat_types::{Query, QueryKind};
use ipnet::IpNet;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("empty target")]
    Empty,
    #[error("invalid IP address: {0}")]
    InvalidIp(String),
    #[error("invalid CIDR: {0}")]
    InvalidCidr(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeHint {
    Auto,
    Email,
    Domain,
    Ip,
    Cidr,
    Text,
}

pub struct TargetParser {
    hint: TypeHint,
}

impl TargetParser {
    pub fn new(hint: TypeHint) -> Self {
        Self { hint }
    }

    pub fn parse(&self, raw: &str) -> Result<Query, ParseError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ParseError::Empty);
        }

        let kind = match self.hint {
            TypeHint::Email => QueryKind::Email,
            TypeHint::Domain => QueryKind::Domain,
            TypeHint::Ip => QueryKind::Ip,
            TypeHint::Cidr => QueryKind::Cidr,
            TypeHint::Text => QueryKind::Text,
            TypeHint::Auto => infer_kind(trimmed),
        };

        match kind {
            QueryKind::Text => Ok(Query {
                raw: trimmed.to_string(),
                normalized: trimmed.to_ascii_lowercase(),
                kind,
                network: None,
                address: None,
            }),
            QueryKind::Email => Ok(Query {
                raw: trimmed.to_string(),
                normalized: trimmed.to_ascii_lowercase(),
                kind,
                network: None,
                address: None,
            }),
            QueryKind::Domain => Ok(Query {
                raw: trimmed.to_string(),
                normalized: trimmed.to_ascii_lowercase().trim_start_matches('.').to_string(),
                kind,
                network: None,
                address: None,
            }),
            QueryKind::Ip => {
                let addr = IpAddr::from_str(trimmed)
                    .map_err(|_| ParseError::InvalidIp(trimmed.to_string()))?;
                Ok(Query {
                    raw: trimmed.to_string(),
                    normalized: addr.to_string(),
                    kind,
                    network: None,
                    address: Some(addr),
                })
            }
            QueryKind::Cidr => {
                let net = IpNet::from_str(trimmed)
                    .map_err(|_| ParseError::InvalidCidr(trimmed.to_string()))?;
                Ok(Query {
                    raw: trimmed.to_string(),
                    normalized: net.to_string(),
                    kind,
                    network: Some(net),
                    address: None,
                })
            }
        }
    }

    pub fn parse_many<I, S>(&self, inputs: I) -> Result<Vec<Query>, ParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        inputs.into_iter().map(|s| self.parse(s.as_ref())).collect()
    }
}

fn infer_kind(s: &str) -> QueryKind {
    if s.contains('/') {
        if IpNet::from_str(s).is_ok() {
            return QueryKind::Cidr;
        }
    }
    if IpAddr::from_str(s).is_ok() {
        return QueryKind::Ip;
    }
    if s.contains('@') {
        return QueryKind::Email;
    }
    QueryKind::Domain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_detect_cidr() {
        let q = TargetParser::new(TypeHint::Auto).parse("10.0.0.0/8").unwrap();
        assert_eq!(q.kind, QueryKind::Cidr);
    }

    #[test]
    fn auto_detect_email() {
        let q = TargetParser::new(TypeHint::Auto)
            .parse("User@Example.COM")
            .unwrap();
        assert_eq!(q.kind, QueryKind::Email);
        assert_eq!(q.normalized, "user@example.com");
    }

    #[test]
    fn text_query_preserves_phrase() {
        let q = TargetParser::new(TypeHint::Text)
            .parse("Winter2024!")
            .unwrap();
        assert_eq!(q.kind, QueryKind::Text);
        assert_eq!(q.normalized, "winter2024!");
    }
}
