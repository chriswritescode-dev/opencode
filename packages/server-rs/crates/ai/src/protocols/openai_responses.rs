use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use regex_lite::Regex;
use serde_json::{Map, Value};

use crate::ExecutionContext;
use crate::prepare::cache_policy::request_ttl;
use crate::prepare::effort_updates::resolve_effort_updates;
use crate::prepare::tool_updates::{
    DeclarableGuard, PlacedGuard, ToolUpdateEncoding, ToolUpdateKind, adds_tools, entry_name, flatten_tools,
    tool_update_kinds,
};
use crate::protocols::open_responses::{self, ProviderAdapter};
use crate::protocols::open_responses_channel::{self, HeaderTransform};
use crate::protocols::openai_freeform::{self, Custom};
use crate::protocols::shared;
use crate::protocols::utils::cache;
use crate::protocols::utils::open_responses_options;
use crate::protocols::utils::responses_hosted_tools::{self, Definition, Definitions, HostedState};
use crate::route::auth::set_header;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::route::transport::channel::ChannelPreparation;
use crate::route::transport::http::JsonRequestParts;
use crate::schema::{AiError, LlmRequest, ToolChoiceType, ToolContent, ToolDefinition, ToolEntry, ToolResultValue};

pub const ID: &str = "openai-responses";
pub const NAME: &str = "OpenAI Responses";
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const PATH: &str = open_responses::PATH;
pub const WEBSOCKET_PROTOCOL_HEADER: &str = open_responses_channel::WEBSOCKET_PROTOCOL_HEADER;
pub const WEBSOCKET_ROTATE_AFTER_MS: u64 = open_responses_channel::WEBSOCKET_ROTATE_AFTER_MS;

pub const ADAPTER: ProviderAdapter = ProviderAdapter {
    id: ID,
    name: NAME,
    native_tool: None,
    lower_media: None,
    restore_hosted_tool_item: Some(is_openai_responses_hosted_tool_item),
};

static HOSTED_TOOLS: LazyLock<Definitions> = LazyLock::new(|| {
    let mut definitions = responses_hosted_tools::common();
    definitions.insert(
        "web_search_preview_call",
        Definition { name: "web_search_preview", input: responses_hosted_tools::action_input, result: None },
    );
    definitions.insert(
        "computer_call",
        Definition { name: "computer_use", input: responses_hosted_tools::action_input, result: None },
    );
    definitions.insert(
        "image_generation_call",
        Definition { name: "image_generation", input: empty_input, result: Some(image_generation_result) },
    );
    definitions
});

static GPT_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|/)gpt-([0-9]+)(?:\.[0-9]+)?(?:-|$)").expect("gpt version pattern compiles"));

pub struct OpenAiResponses;

impl Protocol for OpenAiResponses {
    fn id(&self) -> &str {
        ID
    }

    fn namespaces(&self) -> bool {
        true
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        from_request(&request.request, model)
    }

    fn default_path(&self, _model: &Model, _body: &Value) -> Option<String> {
        Some(PATH.to_string())
    }

    fn framing(&self) -> crate::route::framing::Definition {
        crate::route::framing::sse()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        Box::new(HostedState {
            state: open_responses::initial(request, context, &ADAPTER),
            definitions: HOSTED_TOOLS.clone(),
        })
    }

    fn supports_effort_updates(&self, model: &Model, request: &LlmRequest) -> bool {
        supports_effort_updates(model, request)
    }

    fn prepare_channel(&self, _model: &Model, parts: &JsonRequestParts) -> Option<Result<ChannelPreparation, AiError>> {
        let options = open_responses_channel::Options {
            id: ID.to_string(),
            name: NAME.to_string(),
            rotate_after_ms: Some(WEBSOCKET_ROTATE_AFTER_MS),
            enabled: None,
            url: None,
            headers: Some(Arc::new(protocol_headers) as HeaderTransform),
            continuation: None,
        };
        open_responses_channel::prepare(&options, parts).transpose()
    }

    fn tool_updates(&self) -> Option<ToolUpdateEncoding> {
        let placeable: PlacedGuard = Arc::new(placeable);
        let declarable: DeclarableGuard = Arc::new(declarable);
        Some(ToolUpdateEncoding {
            kinds: vec![ToolUpdateKind::Add, ToolUpdateKind::Remove, ToolUpdateKind::Replace],
            placeable: Some(placeable),
            declarable: Some(declarable),
        })
    }
}

fn protocol_headers(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    let existing =
        headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("openai-beta")).map(|(_, value)| value.clone());
    set_header(headers, "openai-beta", existing.unwrap_or_else(|| WEBSOCKET_PROTOCOL_HEADER.to_string()))
}

fn placeable(model: &Model, _request: &LlmRequest, _index: usize) -> bool {
    tool_update_kinds(model).contains(&ToolUpdateKind::Add)
}

fn declarable(tool: &ToolEntry) -> bool {
    !is_hosted_tool(tool)
}

fn supports_effort_updates(model: &Model, request: &LlmRequest) -> bool {
    if request.provider_options.as_ref().is_some_and(|options| options.contains_key("contextManagement")) {
        return false;
    }
    if provider_string(request, "reasoningMode").as_deref() == Some("pro") {
        return false;
    }
    let http_mode = request
        .http
        .as_ref()
        .and_then(|http| http.body.as_ref())
        .and_then(|body| body.get("reasoning"))
        .and_then(|reasoning| reasoning.get("mode"))
        .and_then(Value::as_str);
    if http_mode == Some("pro") {
        return false;
    }
    if let Some(updates) = model
        .capabilities
        .as_ref()
        .and_then(|capabilities| capabilities.get("reasoning"))
        .and_then(|reasoning| reasoning.get("updates"))
        .and_then(Value::as_bool)
    {
        return updates;
    }
    GPT_VERSION
        .captures(&model.id)
        .and_then(|captures| captures.get(1))
        .and_then(|version| version.as_str().parse::<u64>().ok())
        .is_some_and(|version| version >= 6)
}

pub fn from_request(request: &LlmRequest, model: &Model) -> Result<Value, AiError> {
    let management = decode_context_management(request.provider_options.as_ref())?;
    let options = open_responses_options::resolve(request);
    let reasoning_mode = provider_string(request, "reasoningMode");
    let reasoning_context = provider_string(request, "reasoningContext");
    let (updates, effort) = resolve_effort_updates(request, options.reasoning_effort.as_deref());
    let mut options_with_effort = options.clone();
    options_with_effort.reasoning_effort = effort;
    let generation = open_responses::lower_generation(request, model, &options_with_effort);
    let custom = openai_freeform::tools(request, model);
    let layout = lower_tool_layout(request, model, &custom)?;
    let mut declared = !layout.head.is_empty();
    let mut unavailable: Vec<String> = Vec::new();
    let conversation = {
        let mut lower_tool_changes = |changes: &crate::schema::ToolChanges| -> Result<Vec<Value>, AiError> {
            let mut items = Vec::new();
            for tool in &changes.add {
                unavailable.retain(|name| name != entry_name(tool));
            }
            if declared && !changes.add.is_empty() {
                items.push(developer(&incremental_notice(&unavailable)));
            }
            if !changes.add.is_empty() {
                let tools = changes
                    .add
                    .iter()
                    .map(|tool| lower_tool_entry(tool, &custom))
                    .collect::<Result<Vec<Value>, AiError>>()?;
                items.push(additional_tools(tools));
            }
            declared = declared || !changes.add.is_empty();
            for name in &changes.remove {
                if !unavailable.contains(name) {
                    unavailable.push(name.clone());
                }
            }
            if !changes.remove.is_empty() {
                items.push(developer(&removal_notice(&changes.remove)));
            }
            Ok(items)
        };
        open_responses::lower_conversation(&updates, model, &ADAPTER, Some(&mut lower_tool_changes))?
    };
    let reasoning = generation.get("reasoning").and_then(Value::as_object).cloned();
    let mut conversation = conversation;
    let mut input = layout.head;
    if let Some(Value::Array(items)) = conversation.remove("input") {
        input.extend(items);
    }
    let mut body = Map::new();
    if let Some(model_id) = conversation.remove("model") {
        body.insert("model".to_string(), model_id);
    }
    body.insert("input".to_string(), Value::Array(input));
    for (key, value) in generation {
        body.insert(key, value);
    }
    for (key, value) in cache::openai_request_ttl(request_ttl(request)) {
        body.insert(key, value);
    }
    let has_mode = reasoning_mode.as_deref().is_some_and(|mode| !mode.is_empty());
    let has_context = reasoning_context.as_deref().is_some_and(|context| !context.is_empty());
    if reasoning.is_some() || has_mode || has_context {
        let mut merged = reasoning.unwrap_or_default();
        if let Some(mode) = reasoning_mode {
            merged.insert("mode".to_string(), Value::String(mode));
        }
        if let Some(context) = reasoning_context {
            merged.insert("context".to_string(), Value::String(context));
        }
        body.insert("reasoning".to_string(), Value::Object(merged));
    }
    if let Some(management) = management {
        body.insert("context_management".to_string(), Value::Array(management));
    }
    if let Some(tools) = layout.tools {
        body.insert("tools".to_string(), Value::Array(tools));
    }
    if !request.tools.is_empty() || adds_tools(request) {
        if let Some(choice) = open_responses::allowed_tool_choice(&options, &custom) {
            body.insert("tool_choice".to_string(), choice);
        } else if let Some(choice) = request.tool_choice.as_ref() {
            body.insert("tool_choice".to_string(), lower_tool_choice(choice, &request.tools, &custom)?);
        }
    }
    Ok(Value::Object(body))
}

struct ToolLayout {
    head: Vec<Value>,
    tools: Option<Vec<Value>>,
}

fn lower_tool_layout(
    request: &LlmRequest,
    model: &Model,
    custom: &BTreeMap<String, Custom>,
) -> Result<ToolLayout, AiError> {
    let declares_in_place = tool_update_kinds(model).contains(&ToolUpdateKind::Add);
    let declared = if declares_in_place {
        request.tools.iter().filter(|tool| !is_hosted_tool(tool)).cloned().collect::<Vec<ToolEntry>>()
    } else {
        Vec::new()
    };
    let top = if declared.is_empty() {
        request.tools.clone()
    } else {
        request.tools.iter().filter(|tool| is_hosted_tool(tool)).cloned().collect::<Vec<ToolEntry>>()
    };
    let head = if declared.is_empty() {
        Vec::new()
    } else {
        let tools =
            declared.iter().map(|tool| lower_tool_entry(tool, custom)).collect::<Result<Vec<Value>, AiError>>()?;
        vec![additional_tools(tools)]
    };
    let tools = if top.is_empty() {
        None
    } else {
        Some(top.iter().map(|tool| lower_tool_entry(tool, custom)).collect::<Result<Vec<Value>, AiError>>()?)
    };
    Ok(ToolLayout { head, tools })
}

pub fn lower_tool_entry(entry: &ToolEntry, custom: &BTreeMap<String, Custom>) -> Result<Value, AiError> {
    match entry {
        ToolEntry::Namespace(namespace) => lower_namespace(namespace),
        ToolEntry::Tool(tool) => {
            if native_image_tool_input(tool).is_some() {
                let Some(native) = native_image_tool(tool) else {
                    return Err(shared::invalid_request("OpenAI Responses image generation tool options are invalid"));
                };
                return Ok(Value::Object(native.clone()));
            }
            if let Some(definition) = custom.get(&tool.name) {
                return Ok(open_responses::lower_custom_tool(tool, definition));
            }
            open_responses::lower_tool(NAME, tool)
        }
    }
}

fn lower_namespace(namespace: &crate::schema::ToolNamespace) -> Result<Value, AiError> {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("namespace".to_string()));
    object.insert("name".to_string(), Value::String(namespace.name.clone()));
    object.insert(
        "description".to_string(),
        Value::String(
            namespace.description.clone().unwrap_or_else(|| format!("Tools in the {} namespace.", namespace.name)),
        ),
    );
    let tools = flatten_tools(&namespace.tools)
        .iter()
        .map(|tool| open_responses::lower_tool(NAME, tool))
        .collect::<Result<Vec<Value>, AiError>>()?;
    object.insert("tools".to_string(), Value::Array(tools));
    Ok(Value::Object(object))
}

fn is_hosted_tool(tool: &ToolEntry) -> bool {
    matches!(tool, ToolEntry::Tool(tool) if tool.native.is_some())
}

fn additional_tools(tools: Vec<Value>) -> Value {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("additional_tools".to_string()));
    object.insert("role".to_string(), Value::String("developer".to_string()));
    object.insert("tools".to_string(), Value::Array(tools));
    Value::Object(object)
}

fn developer(content: &str) -> Value {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("message".to_string()));
    object.insert("role".to_string(), Value::String("developer".to_string()));
    object.insert("content".to_string(), Value::String(content.to_string()));
    Value::Object(object)
}

fn incremental_notice(unavailable: &[String]) -> String {
    let mut notice = String::from(
        "Tool update: the tools below are added to the ones you already have. Earlier tools stay available unless marked unavailable, and a tool declared again replaces its earlier definition.",
    );
    if !unavailable.is_empty() {
        notice.push('\n');
        notice.push_str(&format!("Still unavailable: {}.", unavailable.join(", ")));
    }
    notice
}

fn removal_notice(names: &[String]) -> String {
    format!("These tools are no longer available. Do not call them: {}.", names.join(", "))
}

fn lower_tool_choice(
    choice: &crate::schema::ToolChoice,
    tools: &[ToolEntry],
    custom: &BTreeMap<String, Custom>,
) -> Result<Value, AiError> {
    if choice.r#type != ToolChoiceType::Tool {
        return open_responses::lower_tool_choice(NAME, choice, custom);
    }
    let Some(name) = choice.name.as_deref().filter(|name| !name.is_empty()) else {
        return Err(shared::invalid_request(format!("{NAME} tool choice requires a tool name")));
    };
    if tools.iter().any(|entry| match entry {
        ToolEntry::Tool(tool) => tool.name == name && native_image_tool(tool).is_some(),
        ToolEntry::Namespace(_) => false,
    }) {
        let mut object = Map::new();
        object.insert("type".to_string(), Value::String("image_generation".to_string()));
        return Ok(Value::Object(object));
    }
    Ok(open_responses::named_tool(name, custom))
}

fn native_image_tool_input(tool: &ToolDefinition) -> Option<&Map<String, Value>> {
    let native = tool.native.as_ref()?.get("openai")?.as_object()?;
    (native.get("type").and_then(Value::as_str) == Some("image_generation")).then_some(native)
}

fn native_image_tool(tool: &ToolDefinition) -> Option<&Map<String, Value>> {
    let native = native_image_tool_input(tool)?;
    is_image_generation_tool(native).then_some(native)
}

fn is_image_generation_tool(native: &Map<String, Value>) -> bool {
    optional_literal(native, "action", &["auto", "generate", "edit"])
        && optional_literal(native, "background", &["auto", "opaque", "transparent"])
        && optional_literal(native, "input_fidelity", &["low", "high"])
        && optional_int(native, "output_compression", 0.0, Some(100.0))
        && optional_literal(native, "output_format", &["png", "jpeg", "webp"])
        && optional_int(native, "partial_images", 0.0, None)
        && optional_literal(native, "quality", &["auto", "low", "medium", "high"])
        && optional_image_size(native)
}

fn optional_literal(object: &Map<String, Value>, key: &str, allowed: &[&str]) -> bool {
    match object.get(key) {
        None => true,
        Some(Value::String(value)) => allowed.contains(&value.as_str()),
        Some(_) => false,
    }
}

fn optional_int(object: &Map<String, Value>, key: &str, minimum: f64, maximum: Option<f64>) -> bool {
    let Some(value) = object.get(key) else {
        return true;
    };
    let Some(number) = value.as_f64().filter(|number| number.is_finite() && number.fract() == 0.0) else {
        return false;
    };
    number >= minimum && maximum.is_none_or(|maximum| number <= maximum)
}

fn optional_image_size(object: &Map<String, Value>) -> bool {
    let Some(value) = object.get("size") else {
        return true;
    };
    let Some(size) = value.as_str() else {
        return false;
    };
    if size == "auto" {
        return true;
    }
    let Some((width, height)) = size.split_once('x') else {
        return false;
    };
    matches!(width.parse::<u64>(), Ok(width) if width > 0) && matches!(height.parse::<u64>(), Ok(height) if height > 0)
}

fn empty_input(_item: &Value) -> Value {
    Value::Object(Map::new())
}

fn image_generation_result(item: &Value) -> Result<ToolResultValue, AiError> {
    if let Some(result) = item.get("result").and_then(Value::as_str).filter(|result| !result.is_empty()) {
        BASE64_STANDARD
            .decode(result)
            .map_err(|_| shared::event_error(ID, "OpenAI Responses returned invalid image base64", None))?;
        let format = item.get("output_format").and_then(Value::as_str).unwrap_or("png");
        return Ok(ToolResultValue::Content {
            value: vec![ToolContent::File {
                uri: format!("data:image/{format};base64,{result}"),
                mime: format!("image/{format}"),
                name: None,
            }],
        });
    }
    Ok(match item.get("error").filter(|error| !error.is_null()) {
        Some(error) => ToolResultValue::Error { value: error.clone() },
        None => ToolResultValue::Json { value: item.clone() },
    })
}

fn is_openai_responses_hosted_tool_item(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let valid = match object.get("type").and_then(Value::as_str)? {
        "computer_call" => {
            object.get("id").and_then(Value::as_str).is_some()
                && shared::optional_string(object, "status")
                && shared::optional_string(object, "call_id")
                && shared::optional_null_object(object, "action")
                && optional_object_array(object, "pending_safety_checks")
        }
        "web_search_preview_call" => {
            object.get("id").and_then(Value::as_str).is_some()
                && shared::optional_string(object, "status")
                && shared::optional_null_object(object, "action")
        }
        "image_generation_call" => {
            object.get("id").and_then(Value::as_str).is_some()
                && shared::optional_string(object, "status")
                && shared::optional_null_string(object, "result")
                && optional_literal(object, "output_format", &["png", "jpeg", "webp"])
                && shared::optional_null_string(object, "revised_prompt")
        }
        _ => false,
    };
    valid.then(|| value.clone())
}

fn optional_object_array(object: &Map<String, Value>, key: &str) -> bool {
    match object.get(key) {
        None => true,
        Some(Value::Array(values)) => values.iter().all(Value::is_object),
        Some(_) => false,
    }
}

fn provider_string(request: &LlmRequest, key: &str) -> Option<String> {
    request.provider_options.as_ref()?.get(key)?.as_str().map(str::to_string)
}

fn decode_context_management(input: Option<&Map<String, Value>>) -> Result<Option<Vec<Value>>, AiError> {
    let Some(value) = input.and_then(|input| input.get("contextManagement")) else {
        return Ok(None);
    };
    let Some(edits) = value.as_array() else {
        return Err(context_management_error());
    };
    let edits = edits.iter().map(decode_context_management_edit).collect::<Result<Vec<Value>, AiError>>()?;
    Ok(Some(edits))
}

fn decode_context_management_edit(value: &Value) -> Result<Value, AiError> {
    let object = value.as_object().ok_or_else(context_management_error)?;
    if object.get("type").and_then(Value::as_str) != Some("compaction") {
        return Err(context_management_error());
    }
    let mut edit = Map::new();
    edit.insert("type".to_string(), Value::String("compaction".to_string()));
    if let Some(threshold) = object.get("compactThreshold") {
        let valid = threshold
            .as_f64()
            .is_some_and(|threshold| threshold.is_finite() && threshold.fract() == 0.0 && threshold > 0.0);
        if !valid {
            return Err(context_management_error());
        }
        edit.insert("compact_threshold".to_string(), threshold.clone());
    }
    Ok(Value::Object(edit))
}

fn context_management_error() -> AiError {
    shared::invalid_request(format!("{NAME} invalid provider option contextManagement"))
}
