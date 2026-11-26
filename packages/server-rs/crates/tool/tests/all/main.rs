mod corpus;
mod registry;
mod snapshot;

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use opencode_tool::{ToolContext, ToolError, ToolOutput, ToolRuntime};

pub struct Echo {
    pub text: String,
}

impl ToolRuntime for Echo {
    fn execute<'a>(&'a self, _input: Value, _context: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        let text = self.text.clone();
        Box::pin(async move {
            Ok(ToolOutput {
                output: json!({ "text": text }),
                content: vec![json!({ "type": "text", "text": text })],
                metadata: None,
            })
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
