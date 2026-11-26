mod corpus;
mod edit;
mod image;
mod output;
mod patch;
mod patch_parse;
mod read;
mod registry;
mod search;
mod skill;
mod snapshot;
mod tools;
mod validation;
mod write;

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use opencode_tool::{Content, ToolContext, ToolError, ToolOutput, ToolRuntime};

pub struct Echo {
    pub text: String,
}

impl ToolRuntime for Echo {
    fn execute<'a>(&'a self, _input: Value, _context: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        let text = self.text.clone();
        Box::pin(async move {
            Ok(ToolOutput { output: json!({ "text": text }), content: vec![Content::text(text)], metadata: None })
        })
    }
}

pub fn echo(text: impl Into<String>) -> Arc<dyn ToolRuntime> {
    Arc::new(Echo { text: text.into() })
}

pub fn text_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "text": { "type": "string" } },
        "required": ["text"],
        "additionalProperties": false
    })
}
