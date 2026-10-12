//! Domain types, query parsing, and scan orchestration.

pub mod config;
pub mod query;
pub mod scan;

pub use config::{ProgressHandle, ScanConfig};
pub use prowlcat_types::{
    ExtractedValue, MatchKind, Query, QueryKind, ScanResult, ScanSummary, SourceLocation,
};
pub use query::{ParseError, TargetParser, TypeHint};
pub use scan::ScanOrchestrator;
