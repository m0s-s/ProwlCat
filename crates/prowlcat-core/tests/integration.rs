//! Integration smoke tests against bundled fixtures.

use std::path::PathBuf;

use prowlcat_core::{ScanConfig, ScanOrchestrator, TargetParser, TypeHint};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

#[test]
fn search_email_in_fixture() {
    let parser = TargetParser::new(TypeHint::Auto);
    let queries = parser.parse_many(["user@example.com"]).unwrap();

    let config = ScanConfig {
        paths: vec![fixture_path("sample_breach.txt")],
        recursive: false,
        threads: 2,
        ..ScanConfig::default()
    };

    let (results, summary) = ScanOrchestrator::new(config, queries).run().unwrap();
    assert!(summary.matches >= 1, "expected at least one email match");
    assert!(results.iter().any(|r| r.query.contains("user@example.com")));
}

#[test]
fn search_cidr_literal_and_membership() {
    let parser = TargetParser::new(TypeHint::Cidr);
    let queries = parser.parse_many(["10.0.0.0/8"]).unwrap();

    let config = ScanConfig {
        paths: vec![fixture_path("sample_breach.txt")],
        recursive: false,
        threads: 2,
        ..ScanConfig::default()
    };

    let (results, _) = ScanOrchestrator::new(config, queries).run().unwrap();
    use prowlcat_types::MatchKind;
    assert!(
        results.iter().any(|r| r.match_kind == MatchKind::CidrLiteral),
        "expected literal CIDR match"
    );
    assert!(
        results.iter().any(|r| r.match_kind == MatchKind::IpInNetwork),
        "expected IP-in-network match for 10.23.1.5"
    );
}

#[test]
fn search_domain_excludes_false_positive() {
    let parser = TargetParser::new(TypeHint::Domain);
    let queries = parser.parse_many(["acme.example"]).unwrap();

    let config = ScanConfig {
        paths: vec![fixture_path("sample_breach.txt")],
        recursive: false,
        threads: 2,
        ..ScanConfig::default()
    };

    let (results, _) = ScanOrchestrator::new(config, queries).run().unwrap();
    assert!(!results.is_empty());
    assert!(
        results
            .iter()
            .all(|r| !r.line_text.starts_with("notacme.example")),
        "acme.example must not match notacme.example line"
    );
}

#[test]
fn search_plain_text_substring() {
    let parser = TargetParser::new(TypeHint::Text);
    let queries = parser.parse_many(["Winter2024"]).unwrap();

    let config = ScanConfig {
        paths: vec![fixture_path("sample_breach.txt")],
        recursive: false,
        threads: 2,
        ..ScanConfig::default()
    };

    let (results, summary) = ScanOrchestrator::new(config, queries).run().unwrap();
    assert_eq!(summary.matches, 1);
    assert!(results[0].line_text.contains("Winter2024"));
}
