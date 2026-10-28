use std::net::IpAddr;
use std::path::PathBuf;

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryKind {
    Email,
    Domain,
    Ip,
    Cidr,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    Exact,
    CidrLiteral,
    IpInNetwork,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractedValue {
    Email(String),
    Domain(String),
    Ip(String),
    Cidr(String),
    Text(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub raw: String,
    pub normalized: String,
    pub kind: QueryKind,
    pub network: Option<IpNet>,
    pub address: Option<IpAddr>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceLocation {
    pub path: PathBuf,
    pub archive_entry: Option<String>,
    pub line_number: Option<u64>,
    pub byte_offset: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResult {
    pub query: String,
    pub kind: QueryKind,
    pub match_kind: MatchKind,
    pub source: SourceLocation,
    pub line_text: String,
    pub matched_span: (usize, usize),
    pub extracted: Vec<ExtractedValue>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanSummary {
    pub targets: usize,
    pub files_scanned: usize,
    pub bytes_scanned: u64,
    pub matches: usize,
    pub elapsed_ms: u64,
    pub errors: usize,
}

/// An account and the secret that follows it on its line, taken from a match (`--extract`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub account: String,
    pub secret: String,
    /// What the secret can be if it looks like a password hash (empty when it looks like a plain password).
    pub secret_looks_like: Vec<String>,
    /// The target the line matched.
    pub query: String,
    pub path: String,
    pub archive_entry: Option<String>,
    pub line_number: Option<u64>,
}
