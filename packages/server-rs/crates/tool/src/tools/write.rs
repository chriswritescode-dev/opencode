use futures_util::future::BoxFuture;
use opencode_fs::access::AccessError;
use opencode_fs::write::{FileWriter, WriteError, WriteInput};
use serde_json::Value;

use crate::context::{Content, ToolContext, ToolError, ToolOutput, ToolRuntime, file_invocation};

#[derive(Clone)]
pub struct WriteDeps {
    pub writer: FileWriter,
}

pub struct WriteRuntime {
    deps: WriteDeps,
}

impl WriteRuntime {
    pub fn new(deps: WriteDeps) -> Self {
        WriteRuntime { deps }
    }

    async fn execute_inner(&self, input: Value, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = input.get("path").and_then(Value::as_str).unwrap_or_default().to_owned();
        let content = input.get("content").and_then(Value::as_str).unwrap_or_default().to_owned();
        let written = self
            .deps
            .writer
            .write(WriteInput { path: path.clone(), content }, &file_invocation(context))
            .await
            .map_err(|error| write_error(error, &path))?;
        let message =
            format!("{} file successfully: {}", if written.existed { "Wrote" } else { "Created" }, written.resource);
        Ok(ToolOutput { output: Value::String(message.clone()), content: vec![Content::text(message)], metadata: None })
    }
}

impl ToolRuntime for WriteRuntime {
    fn execute<'a>(&'a self, input: Value, context: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(self.execute_inner(input, context))
    }
}

fn write_error(error: WriteError, path: &str) -> ToolError {
    match error {
        WriteError::Access(AccessError::Permission(error)) => ToolError::new(error.to_string()),
        WriteError::Access(AccessError::Failed { .. }) | WriteError::Fs(_) => {
            ToolError::new(format!("Unable to write {path}"))
        }
    }
}
