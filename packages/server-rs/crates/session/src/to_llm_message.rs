use opencode_ai::{
    AssistantMessage, ContentPart, Message, ProviderMetadata, ReasoningPart, SystemMessage, TextPart, UserMessage,
};
use opencode_contract::generated::{
    ModelRef, SessionMessageAssistant, SessionMessageAssistantContentItem, SessionMessageAssistantTool1,
    SessionMessageAssistantTool1State, SessionMessageInfo, SessionMessageNative1, SessionMessageUser,
};
use serde_json::Value;

pub fn to_llm_messages(messages: &[SessionMessageInfo], model: &ModelRef, provider_metadata_key: &str) -> Vec<Message> {
    messages
        .iter()
        .flat_map(|message| match message {
            SessionMessageInfo::User(user) => user_messages(user),
            SessionMessageInfo::Assistant(assistant) => assistant_messages(assistant, model, provider_metadata_key),
            SessionMessageInfo::System(system) => {
                vec![Message::System(SystemMessage { id: None, content: vec![text_part(&system.text)], tools: None })]
            }
            _ => Vec::new(),
        })
        .collect()
}

fn user_messages(user: &SessionMessageUser) -> Vec<Message> {
    let skills = user.skills.iter().flatten().filter_map(|skill| skill.text.as_deref()).map(text_part);
    let text = (!user.text.is_empty()).then(|| text_part(&user.text));
    let content = skills.chain(text).collect::<Vec<_>>();
    if content.is_empty() {
        return Vec::new();
    }
    vec![Message::User(UserMessage { id: Some(user.id.0.clone()), content, provider_metadata: None })]
}

fn assistant_messages(
    assistant: &SessionMessageAssistant,
    model: &ModelRef,
    provider_metadata_key: &str,
) -> Vec<Message> {
    let same_provider = assistant.model.provider_id == model.provider_id;
    let same_model = same_provider && assistant.model.id == model.id;
    let kept = match &assistant.error {
        Some(error) if error.r#type != "aborted" => {
            let end = assistant
                .content
                .iter()
                .rposition(|item| match item {
                    SessionMessageAssistantContentItem::Tool(tool) => tool.executed != Some(true) && ran_tool(tool),
                    _ => false,
                })
                .map_or(0, |index| index + 1);
            &assistant.content[..end]
        }
        _ => assistant.content.as_slice(),
    };
    let cut = assistant.error.is_some() || assistant.finish.is_none();
    let tail = if cut { assistant.content.last() } else { None };
    let items = if cut {
        kept.iter().filter(|item| kept_when_cut(item)).collect::<Vec<_>>()
    } else {
        kept.iter().collect::<Vec<_>>()
    };
    let mut content = Vec::new();
    for (index, item) in items.iter().copied().enumerate() {
        match item {
            SessionMessageAssistantContentItem::Text(text) => {
                let is_tail = tail.is_some_and(|tail| std::ptr::eq(item, tail));
                content.push(ContentPart::Text(TextPart {
                    text: text.text.clone(),
                    cache: None,
                    provider_metadata: if same_model && !is_tail {
                        provider_metadata(provider_metadata_key, text.native.as_ref())
                    } else {
                        None
                    },
                }));
            }
            SessionMessageAssistantContentItem::Reasoning(reasoning) => {
                if cut
                    && !items[index + 1..].iter().copied().any(|after| match after {
                        SessionMessageAssistantContentItem::Tool(_) => true,
                        SessionMessageAssistantContentItem::Text(text) => !text.text.trim().is_empty(),
                        SessionMessageAssistantContentItem::Reasoning(_) => false,
                    })
                {
                    continue;
                }
                let part = if same_model {
                    ReasoningPart {
                        text: reasoning.text.clone(),
                        encrypted: None,
                        cache: None,
                        provider_metadata: provider_metadata(provider_metadata_key, reasoning.native.as_ref()),
                    }
                } else {
                    if reasoning.text.is_empty() {
                        continue;
                    }
                    ReasoningPart {
                        text: reasoning.text.clone(),
                        encrypted: None,
                        cache: None,
                        provider_metadata: None,
                    }
                };
                content.push(ContentPart::Reasoning(part));
            }
            SessionMessageAssistantContentItem::Tool(_) => {}
        }
    }
    let content = content
        .into_iter()
        .filter(|part| match part {
            ContentPart::Text(text) => {
                if cut {
                    !text.text.trim().is_empty()
                } else {
                    !text.text.is_empty()
                }
            }
            ContentPart::Reasoning(reasoning) => {
                !reasoning.text.is_empty()
                    || reasoning.provider_metadata.as_ref().is_some_and(|metadata| !metadata.is_empty())
            }
            _ => true,
        })
        .collect::<Vec<_>>();
    if content.is_empty() {
        return Vec::new();
    }
    vec![Message::Assistant(AssistantMessage { id: Some(assistant.id.0.clone()), content, provider_metadata: None })]
}

fn kept_when_cut(item: &SessionMessageAssistantContentItem) -> bool {
    match item {
        SessionMessageAssistantContentItem::Tool(tool) => {
            ran_tool(tool)
                || matches!(&tool.state, SessionMessageAssistantTool1State::Error(error) if error.error.r#type.starts_with("tool.input"))
        }
        _ => true,
    }
}

fn ran_tool(tool: &SessionMessageAssistantTool1) -> bool {
    tool.time.ran.is_some() || matches!(tool.state, SessionMessageAssistantTool1State::Completed(_))
}

fn text_part(text: &str) -> ContentPart {
    ContentPart::Text(TextPart { text: text.to_string(), cache: None, provider_metadata: None })
}

fn provider_metadata(key: &str, native: Option<&SessionMessageNative1>) -> Option<ProviderMetadata> {
    let native = native?;
    Some(ProviderMetadata::from_iter([(key.to_string(), Value::Object(native.0.iter().cloned().collect()))]))
}
