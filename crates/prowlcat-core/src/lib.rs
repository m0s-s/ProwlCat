//! Domain types, query parsing, and scan orchestration.

pub mod config;
pub mod extract;
pub mod hashid;
pub mod mask;
pub mod query;
pub mod scan;
pub mod sorted;
pub mod unique;

pub use config::{parse_size, ProgressHandle, ScanConfig};
pub use sorted::SortedResults;
pub use prowlcat_types::{
    ExtractedValue, MatchKind, Query, QueryKind, Record, ScanResult, ScanSummary, SourceLocation,
};
pub use query::{ParseError, TargetParser, TypeHint};
pub use scan::ScanOrchestrator;
