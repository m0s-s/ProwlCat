//! Match engines for email, domain, IP, CIDR, and plain-text queries.

mod cidr;
mod engine;

pub use engine::{LineHit, MatchEngine};
