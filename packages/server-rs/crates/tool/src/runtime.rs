use std::collections::BTreeMap;

use futures_util::future::BoxFuture;
use serde_json::Value;

use crate::context::{Content, ToolContext, ToolError, ToolOutput};
use crate::image::{Image, ImageError};
use crate::input;
use crate::registry::RegisteredTool;

#[derive(Clone, Debug)]
pub struct BeforeEvent {
    pub tool: String,
    pub codemode: bool,
    pub session_id: String,
    pub agent: String,
    pub message_id: String,
    pub call_id: String,
    pub input: Value,
}

#[derive(Clone, Debug)]
pub struct AfterEvent {
    pub tool: String,
    pub codemode: bool,
    pub session_id: String,
    pub agent: String,
    pub message_id: String,
    pub call_id: String,
    pub input: Value,
    pub result: Result<ToolOutput, ToolError>,
}

pub trait ToolHooks: Send + Sync {
    fn before<'a>(&'a self, event: BeforeEvent) -> BoxFuture<'a, BeforeEvent>;
    fn after<'a>(&'a self, event: AfterEvent) -> BoxFuture<'a, AfterEvent>;
}

pub struct NoHooks;

impl ToolHooks for NoHooks {
    fn before<'a>(&'a self, event: BeforeEvent) -> BoxFuture<'a, BeforeEvent> {
        Box::pin(async move { event })
    }

    fn after<'a>(&'a self, event: AfterEvent) -> BoxFuture<'a, AfterEvent> {
        Box::pin(async move { event })
    }
}

pub async fn run(
    tool: &RegisteredTool,
    name: &str,
    input: Value,
    context: &ToolContext,
    entries: &BTreeMap<String, RegisteredTool>,
    hooks: &dyn ToolHooks,
    image: &Image,
) -> Result<ToolOutput, ToolError> {
    let before = hooks
        .before(BeforeEvent {
            tool: name.to_owned(),
            codemode: false,
            session_id: context.session_id.clone(),
            agent: context.agent.clone(),
            message_id: context.message_id.clone(),
            call_id: context.call_id.clone(),
            input,
        })
        .await;
    let selected = if before.tool == name {
        tool
    } else {
        entries
            .get(&before.tool)
            .ok_or_else(|| ToolError::new(format!("Tool is not available for this request: {}", before.tool)))?
    };
    execute_tool(selected, &before.tool, before.input, context, hooks, image).await
}

async fn execute_tool(
    tool: &RegisteredTool,
    name: &str,
    input: Value,
    context: &ToolContext,
    hooks: &dyn ToolHooks,
    image: &Image,
) -> Result<ToolOutput, ToolError> {
    let mut result = match input::decode(name, &tool.definition.input_schema, &input) {
        Ok(decoded) => tool.runtime.execute(decoded, context).await,
        Err(error) => Err(error),
    };
    if let Ok(output) = &mut result {
        normalize_content(output);
    }
    let after = hooks
        .after(AfterEvent {
            tool: name.to_owned(),
            codemode: false,
            session_id: context.session_id.clone(),
            agent: context.agent.clone(),
            message_id: context.message_id.clone(),
            call_id: context.call_id.clone(),
            input,
            result,
        })
        .await;
    after.result.map(|mut output| {
        normalize_content(&mut output);
        normalize_images(&mut output.content, image);
        output
    })
}

fn normalize_content(output: &mut ToolOutput) {
    if output.content.is_empty() {
        output.content.push(Content::text(stringify(&output.output)));
    }
}

fn normalize_images(content: &mut Vec<Content>, image: &Image) {
    let mut normalized = Vec::with_capacity(content.len());
    let mut decode = 0usize;
    let mut size = 0usize;
    for item in std::mem::take(content) {
        let Content::File { uri, mime, name } = &item else {
            normalized.push(item);
            continue;
        };
        if !mime.starts_with("image/") {
            normalized.push(item);
            continue;
        }
        let Some(base64) = data_uri(uri) else {
            normalized.push(item);
            continue;
        };
        let resource = name.clone().unwrap_or_else(|| format!("{mime} tool output"));
        match image.normalize(&resource, mime, base64) {
            Ok(Some(result)) => normalized.push(Content::File {
                uri: format!("data:{};base64,{}", result.mime, result.content),
                mime: result.mime,
                name: name.clone(),
            }),
            Ok(None) => normalized.push(item),
            Err(ImageError::Decode { .. }) => decode += 1,
            Err(ImageError::Size { .. }) => size += 1,
        }
    }
    if decode > 0 {
        normalized.push(Content::text(format!(
            "[{decode} image{} omitted: could not be decoded.]",
            if decode == 1 { "" } else { "s" }
        )));
    }
    if size > 0 {
        normalized.push(Content::text(format!(
            "[{size} image{} omitted: could not be resized below the image size limit.]",
            if size == 1 { "" } else { "s" }
        )));
    }
    *content = normalized;
}

fn data_uri(uri: &str) -> Option<&str> {
    let rest = uri.strip_prefix("data:")?;
    let (metadata, content) = rest.split_once(',')?;
    metadata.strip_suffix(";base64")?;
    Some(content)
}

pub fn stringify(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    }
}
