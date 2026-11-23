use std::fs;
use std::path::Path;

use opencode_contract::Stability;
use opencode_contract::generated::ROUTES;
use opencode_http::app::IMPLEMENTED;

#[test]
fn coverage_document_is_current() {
    for operation in IMPLEMENTED {
        assert!(
            ROUTES.iter().any(|route| route.operation_id == *operation),
            "IMPLEMENTED contains `{operation}`, which is not in the contract ROUTES table"
        );
    }

    let expected = coverage_document();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/coverage.md");

    if std::env::var_os("OPENCODE_UPDATE_COVERAGE").is_some_and(|value| value == "1") {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("coverage document directory");
        }
        fs::write(&path, &expected).expect("write coverage document");
        return;
    }

    let actual = fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        actual, expected,
        "docs/coverage.md is out of date; regenerate it from packages/server-rs with `OPENCODE_UPDATE_COVERAGE=1 cargo test -p opencode-http coverage`"
    );
}

fn coverage_document() -> String {
    let header = format!(
        "# Operation coverage\n\nImplemented: {} of {} operations\n\n| Operation | Method | Path | Stability | Status |\n| --- | --- | --- | --- | --- |\n",
        IMPLEMENTED.len(),
        ROUTES.len()
    );
    let rows = ROUTES
        .iter()
        .map(|route| {
            format!(
                "| `{}` | {} | `{}` | {} | {} |",
                route.operation_id,
                route.method.as_str(),
                route.path,
                stability(route.stability),
                if IMPLEMENTED.contains(&route.operation_id) { "implemented" } else { "not implemented (501)" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{header}{rows}\n")
}

fn stability(stability: Stability) -> &'static str {
    match stability {
        Stability::Stable => "stable",
        Stability::Experimental => "experimental",
    }
}
