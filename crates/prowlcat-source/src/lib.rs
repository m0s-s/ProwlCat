//! Source enumeration: files, directories, globs, compression detection.

mod entry;
mod walker;

pub use entry::{CompressionKind, SourceEntry};
pub use walker::SourceEnumerator;
