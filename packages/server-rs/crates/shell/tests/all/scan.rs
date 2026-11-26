use opencode_shell::scan::{Dialect, ScanResult, scan, scan_power_shell};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    name: String,
    source: String,
    dialect: Dialect,
    input: String,
    expected: ScanResult,
}

#[derive(Deserialize)]
struct PowerShellCase {
    name: String,
    source: String,
    input: String,
    expected: ScanResult,
}

fn assert_matches(total: usize, mismatches: &[String], label: &str) {
    assert!(
        mismatches.is_empty(),
        "{} of {} {label} cases mismatch:\n{}",
        mismatches.len(),
        total,
        mismatches.join("\n"),
    );
}

#[test]
fn replays_the_scanner_corpus() {
    let corpus = std::fs::read_to_string(format!("{}/tests/corpus/scan-bash.json", env!("CARGO_MANIFEST_DIR")))
        .expect("scan-bash corpus");
    let cases: Vec<Case> = serde_json::from_str(&corpus).expect("scan-bash corpus json");
    let mismatches: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            let actual = scan(&case.input, case.dialect);
            (actual != case.expected).then(|| {
                let input: String = case.input.chars().take(200).collect();
                format!(
                    "{} ({}, {:?}) input {:?}\n  expected {}\n  actual   {}",
                    case.name,
                    case.source,
                    case.dialect,
                    input,
                    serde_json::to_string(&case.expected).unwrap_or_default(),
                    serde_json::to_string(&actual).unwrap_or_default(),
                )
            })
        })
        .take(20)
        .collect();
    assert_matches(cases.len(), &mismatches, "scan");
}

#[test]
fn replays_the_powershell_scanner_corpus() {
    let corpus = std::fs::read_to_string(format!("{}/tests/corpus/scan-powershell.json", env!("CARGO_MANIFEST_DIR")))
        .expect("scan-powershell corpus");
    let cases: Vec<PowerShellCase> = serde_json::from_str(&corpus).expect("scan-powershell corpus json");
    let mismatches: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            let actual = scan_power_shell(&case.input);
            (actual != case.expected).then(|| {
                let input: String = case.input.chars().take(200).collect();
                format!(
                    "{} ({}) input {:?}\n  expected {}\n  actual   {}",
                    case.name,
                    case.source,
                    input,
                    serde_json::to_string(&case.expected).unwrap_or_default(),
                    serde_json::to_string(&actual).unwrap_or_default(),
                )
            })
        })
        .take(20)
        .collect();
    assert_matches(cases.len(), &mismatches, "scan-powershell");
}

#[test]
fn reports_invalid_structure_for_ansi_quote_failures_in_arithmetic_context() {
    let cases = [
        ("echo ${a[$'", Dialect::Bash),
        ("echo ${a[$'", Dialect::Posix),
        (r"echo ${a[$'\170", Dialect::Bash),
        (r"echo ${a[$'\170", Dialect::Zsh),
        (r"echo ${a[$'\170", Dialect::Posix),
        (r"a[$'\:']=1", Dialect::Posix),
        (r"echo ${a[$'\:']}", Dialect::Posix),
    ];
    let expected = serde_json::json!({ "kind": "opaque", "reason": "invalid-structure" });
    for (input, dialect) in cases {
        assert_eq!(serde_json::to_value(scan(input, dialect)).unwrap(), expected, "{input:?} in {dialect:?}");
    }
}

#[test]
fn counts_redirect_offsets_in_utf16_code_units() {
    let scanned = scan("2>\"💥\"", Dialect::Posix);
    let expected = serde_json::json!({
        "kind": "scanned",
        "commands": [{
            "resource": "2>\"💥\"",
            "words": ["2>\"💥\""],
            "rawWords": ["2>\"💥\""],
            "redirectPrefixEnd": 6,
        }],
    });
    assert_eq!(serde_json::to_value(scanned).unwrap(), expected);
}
