use crate::prepare::effort_updates::effort_update;
use crate::prepare::tool_updates::tools_update;
use crate::route::model::Model;
use crate::schema::{
    CacheHint, CacheHintType, CacheMessages, CacheMode, CachePolicy, CachePolicyObject, ContentPart, LlmRequest,
    Message, SystemPart, ToolEntry,
};

const BREAKPOINT_CAP: usize = 4;

const RESPECTS_INLINE_HINTS: [&str; 15] = [
    "alibaba-chat",
    "alibaba-messages",
    "anthropic-messages",
    "anthropic-compatible-messages",
    "bedrock-mantle-messages",
    "cloudflare-ai-gateway-messages",
    "google-vertex-messages",
    "meta-messages",
    "minimax-messages",
    "moonshot-messages",
    "vercel-ai-gateway-messages",
    "zai-coding-messages",
    "bedrock-converse",
    "openrouter",
    "digitalocean",
];

pub fn request_ttl(request: &LlmRequest) -> Option<&str> {
    match request.cache.as_ref()? {
        CachePolicy::Object(object) => object.ttl.as_deref(),
        CachePolicy::Mode(_) => None,
    }
}

pub fn places_markers(model: &Model, request: &LlmRequest) -> bool {
    let policy = select_policy(model, request);
    policy.tools == Some(true) || policy.system == Some(true) || policy.messages.is_some()
}

pub fn apply_cache_policy(model: &Model, request: LlmRequest) -> LlmRequest {
    let policy = select_policy(model, &request);
    if policy.tools != Some(true) && policy.system != Some(true) && policy.messages.is_none() {
        return request;
    }

    let hint = make_hint(request_ttl(&request));
    let mut budget = Budget { remaining: BREAKPOINT_CAP.saturating_sub(count_hints(&request)) };
    let tools = if policy.tools == Some(true) {
        mark_last_tool(&request.tools, &hint, &mut budget)
    } else {
        request.tools.clone()
    };
    let system = if policy.system == Some(true) {
        mark_system_boundaries(&request.system, &hint, &mut budget)
    } else {
        request.system.clone()
    };
    let messages = match policy.messages.as_ref() {
        Some(strategy) => mark_messages(&request.messages, strategy, &hint, &mut budget),
        None => request.messages.clone(),
    };

    if tools == request.tools && system == request.system && messages == request.messages {
        return request;
    }
    let mut request = request;
    request.tools = tools;
    request.system = system;
    request.messages = messages;
    request
}

fn tail(count: u64) -> CacheMessages {
    CacheMessages::Tail { tail: serde_json::Number::from(count) }
}

fn auto_policy() -> CachePolicyObject {
    CachePolicyObject { tools: Some(true), system: Some(true), messages: Some(tail(1)), ttl: None }
}

fn qwen_policy() -> CachePolicyObject {
    CachePolicyObject { system: Some(true), messages: Some(tail(1)), ..CachePolicyObject::default() }
}

fn none_policy() -> CachePolicyObject {
    CachePolicyObject::default()
}

fn is_auto(policy: Option<&CachePolicy>) -> bool {
    match policy {
        None => true,
        Some(CachePolicy::Mode(CacheMode::Auto)) => true,
        Some(CachePolicy::Mode(CacheMode::None)) => false,
        Some(CachePolicy::Object(object)) => {
            object.tools.is_none() && object.system.is_none() && object.messages.is_none()
        }
    }
}

fn resolve(policy: Option<&CachePolicy>) -> CachePolicyObject {
    match policy {
        Some(CachePolicy::Mode(CacheMode::None)) => none_policy(),
        Some(CachePolicy::Object(object)) if !is_auto(policy) => object.clone(),
        _ => auto_policy(),
    }
}

fn gateway_policy(model_id: &str) -> CachePolicyObject {
    let id = model_id.strip_prefix('~').unwrap_or(model_id);
    if id.starts_with("anthropic/") {
        return auto_policy();
    }
    if id.starts_with("qwen/") || id.starts_with("alibaba/qwen") {
        return qwen_policy();
    }
    none_policy()
}

fn select_policy(model: &Model, request: &LlmRequest) -> CachePolicyObject {
    let format = model.compatibility_str("cacheFormat");
    let route = model.route.id.as_str();
    let auto = is_auto(request.cache.as_ref());
    if format == Some("alibaba") {
        if auto {
            return qwen_policy();
        }
        let mut policy = resolve(request.cache.as_ref());
        policy.tools = Some(false);
        return policy;
    }
    if format == Some("anthropic") {
        return resolve(request.cache.as_ref());
    }
    if !RESPECTS_INLINE_HINTS.contains(&route) {
        return none_policy();
    }
    if !auto {
        return resolve(request.cache.as_ref());
    }
    if route == "openrouter" || route == "vercel-ai-gateway-messages" {
        return gateway_policy(&model.id);
    }
    if route == "alibaba-chat" {
        if model.id.to_ascii_lowercase().starts_with("qwen") {
            return qwen_policy();
        }
        return none_policy();
    }
    auto_policy()
}

fn make_hint(ttl: Option<&str>) -> CacheHint {
    CacheHint { r#type: CacheHintType::Ephemeral, ttl: ttl.map(str::to_string) }
}

struct Budget {
    remaining: usize,
}

fn mark_last_tool(tools: &[ToolEntry], hint: &CacheHint, budget: &mut Budget) -> Vec<ToolEntry> {
    let Some(target) = tools.last() else {
        return tools.to_vec();
    };
    match target {
        ToolEntry::Namespace(namespace) => {
            let nested = mark_last_tool(&namespace.tools, hint, budget);
            if nested == namespace.tools {
                return tools.to_vec();
            }
            let mut next = tools.to_vec();
            let mut namespace = namespace.clone();
            namespace.tools = nested;
            if let Some(last) = next.last_mut() {
                *last = ToolEntry::Namespace(namespace);
            }
            next
        }
        ToolEntry::Tool(definition) => {
            if definition.cache.is_some() || budget.remaining == 0 {
                return tools.to_vec();
            }
            budget.remaining -= 1;
            let mut next = tools.to_vec();
            let mut definition = definition.clone();
            definition.cache = Some(hint.clone());
            if let Some(last) = next.last_mut() {
                *last = ToolEntry::Tool(definition);
            }
            next
        }
    }
}

fn count_tool_hints(tools: &[ToolEntry]) -> usize {
    tools
        .iter()
        .map(|tool| match tool {
            ToolEntry::Tool(tool) => usize::from(tool.cache.is_some()),
            ToolEntry::Namespace(tool) => count_tool_hints(&tool.tools),
        })
        .sum()
}

fn mark_system_boundaries(system: &[SystemPart], hint: &CacheHint, budget: &mut Budget) -> Vec<SystemPart> {
    if system.is_empty() {
        return system.to_vec();
    }
    let last = system.len() - 1;
    let mut next = system.to_vec();
    let mut changed = false;
    for (index, part) in next.iter_mut().enumerate() {
        if index != 0 && index != last {
            continue;
        }
        let SystemPart::Text { cache, .. } = part;
        if cache.is_some() || budget.remaining == 0 {
            continue;
        }
        budget.remaining -= 1;
        changed = true;
        *cache = Some(hint.clone());
    }
    if changed {
        return next;
    }
    system.to_vec()
}

fn message_content(message: &Message) -> &[ContentPart] {
    match message {
        Message::System(message) => &message.content,
        Message::User(message) => &message.content,
        Message::Assistant(message) => &message.content,
        Message::Tool(message) => &message.content,
    }
}

fn is_cacheable_part(part: &ContentPart) -> bool {
    !matches!(part, ContentPart::Compaction(_) | ContentPart::Effort(_))
}

fn part_cache(part: &ContentPart) -> Option<&CacheHint> {
    match part {
        ContentPart::Text(part) => part.cache.as_ref(),
        ContentPart::Media(part) => part.cache.as_ref(),
        ContentPart::ToolCall(part) => part.cache.as_ref(),
        ContentPart::ToolResult(part) => part.cache.as_ref(),
        ContentPart::Reasoning(part) => part.cache.as_ref(),
        ContentPart::Compaction(_) | ContentPart::Effort(_) => None,
    }
}

fn set_part_cache(part: &mut ContentPart, hint: &CacheHint) {
    match part {
        ContentPart::Text(part) => part.cache = Some(hint.clone()),
        ContentPart::Media(part) => part.cache = Some(hint.clone()),
        ContentPart::ToolCall(part) => part.cache = Some(hint.clone()),
        ContentPart::ToolResult(part) => part.cache = Some(hint.clone()),
        ContentPart::Reasoning(part) => part.cache = Some(hint.clone()),
        ContentPart::Compaction(_) | ContentPart::Effort(_) => {}
    }
}

fn mark_message_at(messages: &[Message], index: usize, hint: &CacheHint, budget: &mut Budget) -> Vec<Message> {
    let Some(target) = messages.get(index) else {
        return messages.to_vec();
    };
    let content = message_content(target);
    if content.is_empty() {
        return messages.to_vec();
    }
    let mark_at = content
        .iter()
        .rposition(|part| matches!(part, ContentPart::Text(_)))
        .or_else(|| content.iter().rposition(is_cacheable_part));
    let Some(mark_at) = mark_at else {
        return messages.to_vec();
    };
    let Some(part) = content.get(mark_at) else {
        return messages.to_vec();
    };
    if !is_cacheable_part(part) || part_cache(part).is_some() || budget.remaining == 0 {
        return messages.to_vec();
    }
    budget.remaining -= 1;
    let mut next = messages.to_vec();
    if let Some(target) = next.get_mut(index) {
        let content = match target {
            Message::System(message) => &mut message.content,
            Message::User(message) => &mut message.content,
            Message::Assistant(message) => &mut message.content,
            Message::Tool(message) => &mut message.content,
        };
        if let Some(part) = content.get_mut(mark_at) {
            set_part_cache(part, hint);
        }
    }
    next
}

fn is_marker(message: &Message) -> bool {
    effort_update(message).is_some() || tools_update(message).is_some()
}

fn mark_messages(
    messages: &[Message],
    strategy: &CacheMessages,
    hint: &CacheHint,
    budget: &mut Budget,
) -> Vec<Message> {
    if messages.is_empty() {
        return messages.to_vec();
    }
    match strategy {
        CacheMessages::Named(name) => match name.as_str() {
            "latest-user-message" => messages
                .iter()
                .rposition(|message| matches!(message, Message::User(_)))
                .map_or_else(|| messages.to_vec(), |index| mark_message_at(messages, index, hint, budget)),
            "latest-assistant" => messages
                .iter()
                .rposition(|message| matches!(message, Message::Assistant(_)))
                .map_or_else(|| messages.to_vec(), |index| mark_message_at(messages, index, hint, budget)),
            _ => messages.to_vec(),
        },
        CacheMessages::Tail { tail } => {
            let mut start = messages.len();
            let mut remaining = tail.as_f64().unwrap_or(0.0);
            while remaining > 0.0 && start > 0 {
                start -= 1;
                if !is_marker(&messages[start]) {
                    remaining -= 1.0;
                }
            }
            let mut next = messages.to_vec();
            for (index, message) in messages.iter().enumerate().skip(start) {
                if is_marker(message) {
                    continue;
                }
                next = mark_message_at(&next, index, hint, budget);
            }
            next
        }
    }
}

fn count_hints(request: &LlmRequest) -> usize {
    let tools = count_tool_hints(&request.tools);
    let system = request
        .system
        .iter()
        .filter(|part| match part {
            SystemPart::Text { cache, .. } => cache.is_some(),
        })
        .count();
    let messages = request
        .messages
        .iter()
        .map(|message| {
            message_content(message).iter().filter(|part| is_cacheable_part(part) && part_cache(part).is_some()).count()
        })
        .sum::<usize>();
    tools + system + messages
}
