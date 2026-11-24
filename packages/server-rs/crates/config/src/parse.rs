use opencode_contract::codec::{Decode, Path};
use opencode_contract::generated::ConfigEntryDocumentInfo;
use serde_json::Value;

use crate::normalize::{Diagnostic, DiagnosticKind, Normalized, normalize};

#[derive(Debug)]
pub struct ParseOutcome {
    pub source: String,
    pub info: Option<ConfigEntryDocumentInfo>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn parse_info(text: &str, source: &str) -> ParseOutcome {
    let Some(input) = opencode_local::jsonc::parse(text) else {
        let diagnostic = Diagnostic {
            kind: DiagnosticKind::Invalid,
            path: vec!["$".to_owned()],
            message: "rejected malformed JSON or JSONC document".to_owned(),
        };
        log_diagnostic(source, &diagnostic, true);
        return ParseOutcome { source: source.to_owned(), info: None, diagnostics: vec![diagnostic] };
    };

    match normalize(&input) {
        Normalized::Rejected { diagnostics } => {
            for diagnostic in &diagnostics {
                log_diagnostic(source, diagnostic, true);
            }
            ParseOutcome { source: source.to_owned(), info: None, diagnostics }
        }
        Normalized::Normalized { encoded, diagnostics } => {
            for diagnostic in &diagnostics {
                let warn = matches!(diagnostic.kind, DiagnosticKind::Invalid | DiagnosticKind::Removed);
                log_diagnostic(source, diagnostic, warn);
            }
            match ConfigEntryDocumentInfo::decode(&Value::Object(encoded), &mut Path::new()) {
                Ok(info) => ParseOutcome { source: source.to_owned(), info: Some(info), diagnostics },
                Err(_) => {
                    let diagnostic = Diagnostic {
                        kind: DiagnosticKind::Invalid,
                        path: vec!["$".to_owned()],
                        message: "rejected canonical configuration after final validation".to_owned(),
                    };
                    log_diagnostic(source, &diagnostic, true);
                    ParseOutcome {
                        source: source.to_owned(),
                        info: None,
                        diagnostics: diagnostics.into_iter().chain([diagnostic]).collect(),
                    }
                }
            }
        }
    }
}

fn log_diagnostic(source: &str, diagnostic: &Diagnostic, warn: bool) {
    let path = if diagnostic.path.first().is_some_and(|item| item == "$") {
        "$".to_owned()
    } else {
        format!("$.{}", diagnostic.path.join("."))
    };
    if warn {
        log::warn!(
            "configuration normalization diagnostic: source={source} path={path} kind={} action={}",
            diagnostic.kind.as_str(),
            diagnostic.message
        );
    } else {
        log::debug!(
            "configuration normalization diagnostic: source={source} path={path} kind={} action={}",
            diagnostic.kind.as_str(),
            diagnostic.message
        );
    }
}
