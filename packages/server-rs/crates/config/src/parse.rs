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
        return ParseOutcome {
            source: source.to_owned(),
            info: None,
            diagnostics: vec![Diagnostic {
                kind: DiagnosticKind::Invalid,
                path: vec!["$".to_owned()],
                message: "rejected malformed JSON or JSONC document".to_owned(),
            }],
        };
    };

    match normalize(&input) {
        Normalized::Rejected { diagnostics } => ParseOutcome { source: source.to_owned(), info: None, diagnostics },
        Normalized::Normalized { encoded, mut diagnostics } => {
            match ConfigEntryDocumentInfo::decode(&Value::Object(encoded), &mut Path::new()) {
                Ok(info) => ParseOutcome { source: source.to_owned(), info: Some(info), diagnostics },
                Err(_) => {
                    diagnostics.push(Diagnostic {
                        kind: DiagnosticKind::Invalid,
                        path: vec!["$".to_owned()],
                        message: "rejected canonical configuration after final validation".to_owned(),
                    });
                    ParseOutcome { source: source.to_owned(), info: None, diagnostics }
                }
            }
        }
    }
}
