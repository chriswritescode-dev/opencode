use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opencode_ai::HttpSender;
use opencode_contract::generated::{AgentID, ModelRef, ProviderID, SessionID};
use serde::Deserialize;
use serde_json::Value;

use crate::builtin::{Builtin, Services};
use crate::provider::ProviderService;
use crate::provider_plugin::hooks::{Compaction, HttpRequest, ModelRequest, ModelResolve, RequestKind, Title};
use crate::provider_plugin::http;
use crate::provider_plugin::refresh::Refresh;
use crate::provider_plugin::{AppInfo, PluginContext, SessionAccess};

use super::{API_VERSION, Discovery, UtilityTitleInput, UtilityTitleRequest, models, utility_title};

const ROUTE_TIMEOUT: Duration = Duration::from_secs(5);

const ROUTE_TTL: f64 = 300_000.0;

const TITLE_MODELS: [&str; 1] = ["gpt-4o-mini"];

#[derive(Clone, Debug, PartialEq)]
struct Pick {
    model: String,
    account: String,
    token: Option<String>,
    until: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Metadata {
    agent: bool,
    vision: bool,
}

#[derive(Clone)]
struct State {
    http: Arc<dyn HttpSender>,
    app: AppInfo,
    sessions: Arc<dyn SessionAccess>,
    discovered: Arc<Refresh<Discovery>>,
    providers: Arc<ProviderService>,
    provider_id: ProviderID,
    routes: Arc<Mutex<HashMap<String, Pick>>>,
}

#[derive(Deserialize)]
struct AutoRoute {
    session_token: String,
    expires_at: f64,
    selected_model: SelectedModel,
}

#[derive(Deserialize)]
struct SelectedModel {
    id: String,
}

pub fn register(
    services: &Services,
    context: &PluginContext,
    builtin: &mut Builtin,
    provider_id: &ProviderID,
    discovered: &Arc<Refresh<Discovery>>,
) {
    let state = State {
        http: context.http.clone(),
        app: context.app.clone(),
        sessions: context.sessions.clone(),
        discovered: discovered.clone(),
        providers: services.provider.clone(),
        provider_id: provider_id.clone(),
        routes: Arc::new(Mutex::new(HashMap::new())),
    };
    let hooks = context.hooks.clone();
    builtin.hooks.push(hooks.on_model_request(Some(provider_id.clone()), {
        let state = state.clone();
        move |mut event: ModelRequest| {
            let state = state.clone();
            Box::pin(async move {
                let session = state.sessions.session(event.session_id.clone()).await;
                let child = session.as_ref().is_some_and(|session| session.parent_id.is_some());
                let interaction = interaction_type(event.kind, child);
                let affinity =
                    session.map(|session| session.affinity().0).unwrap_or_else(|| event.session_id.0.clone());
                event.headers.insert("X-Interaction-Type".to_owned(), interaction.to_owned());
                event.headers.insert("X-Interaction-Id".to_owned(), affinity);
                if interaction != "conversation-agent" {
                    event.headers.insert("x-initiator".to_owned(), "agent".to_owned());
                }
                let pick = opencode_util::lock(&state.routes).get(&event.session_id.0).cloned();
                if let Some(pick) = pick
                    && let Some(token) = pick.token
                    && pick.model == event.model.id
                {
                    event.headers.insert("Copilot-Session-Token".to_owned(), token);
                }
                event
            })
        }
    }));
    builtin.hooks.push(hooks.on_model_resolve(Some(provider_id.clone()), {
        let state = state.clone();
        move |mut event: ModelResolve| {
            let state = state.clone();
            Box::pin(async move {
                if event.model.id != models::AUTO || event.kind == RequestKind::Title {
                    return event;
                }
                let now = now_ms();
                let cached = opencode_util::lock(&state.routes).get(&event.session_id.0).cloned();
                let pick = match cached {
                    Some(pick)
                        if pick.until > now && Some(&pick.account) == state.discovered.current().token.as_ref() =>
                    {
                        Some(pick)
                    }
                    _ => route(&state, &event.session_id).await,
                };
                let Some(pick) = pick else {
                    return event;
                };
                {
                    let mut routes = opencode_util::lock(&state.routes);
                    routes.retain(|_, pick| pick.until > now);
                    routes.insert(event.session_id.0.clone(), pick.clone());
                }
                event.model = ModelRef { id: pick.model, provider_id: state.provider_id.clone(), variant: None };
                event
            })
        }
    }));
    builtin.hooks.push(hooks.on_compaction(Some(provider_id.clone()), {
        let state = state.clone();
        move |event: Compaction| {
            let state = state.clone();
            Box::pin(async move {
                if let Some(pick) = opencode_util::lock(&state.routes).get_mut(&event.session_id.0) {
                    pick.until = 0.0;
                }
                event
            })
        }
    }));
    builtin.hooks.push(hooks.on_title(Some(provider_id.clone()), {
        let state = state.clone();
        move |mut event: Title| {
            let state = state.clone();
            Box::pin(async move {
                let loaded = state.discovered.current();
                let Some(base_url) = loaded.base_url.as_deref() else {
                    return event;
                };
                let Some(token) = loaded.token.as_deref() else {
                    return event;
                };
                let Some(snapshot) = loaded.models.as_ref() else {
                    return event;
                };
                if state.sessions.agent_model(AgentID("title".to_owned())).await.is_some() {
                    return event;
                }
                let Some(model) = TITLE_MODELS.iter().copied().find(|id| snapshot.contains_key(*id)) else {
                    return event;
                };
                let input = UtilityTitleInput { base_url, token, model, app: &state.app };
                let request = UtilityTitleRequest {
                    session_id: &event.session_id.0,
                    system: &event.system,
                    messages: &event.messages,
                    max_tokens: event.max_tokens,
                };
                match utility_title(state.http.as_ref(), &input, &request).await {
                    Ok(title) => event.result = Some(title),
                    Err(error) => log::debug!("GitHub Copilot utility title failed: {error}"),
                }
                event
            })
        }
    }));
    builtin.hooks.push(hooks.on_http_request(Some(provider_id.clone()), {
        let state = state.clone();
        move |mut event: HttpRequest| {
            let state = state.clone();
            Box::pin(async move {
                let token = header(&event.request.headers, "x-api-key")
                    .or_else(|| header(&event.request.headers, "authorization").map(strip_bearer))
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned);
                let Some(token) = token else {
                    return event;
                };
                let body: Option<Value> = serde_json::from_slice(&event.request.body).ok();
                let metadata = request_metadata(&event.request.url, body.as_ref());
                apply_headers(&mut event.request.headers, &token, &state.app, metadata);
                event
            })
        }
    }));
}

async fn route(state: &State, session_id: &SessionID) -> Option<Pick> {
    let loaded = state.discovered.current();
    let base_url = loaded.base_url?;
    let token = loaded.token?;
    let snapshot = loaded.models?;
    let user = state.sessions.last_user_message(session_id.clone()).await;
    let mut body = serde_json::Map::new();
    body.insert("prompt".to_owned(), Value::String(user.as_ref().map(|user| user.text.clone()).unwrap_or_default()));
    if user.as_ref().is_some_and(|user| user.has_image) {
        body.insert("has_image".to_owned(), Value::Bool(true));
    }
    let mut headers = state
        .providers
        .get(&state.provider_id)
        .await
        .ok()
        .flatten()
        .and_then(|provider| provider.headers)
        .map(|headers| headers.0)
        .unwrap_or_default();
    super::upsert(&mut headers, "Authorization", format!("Bearer {token}"));
    super::upsert(&mut headers, "User-Agent", state.app.useragent());
    super::upsert(&mut headers, "X-GitHub-Api-Version", API_VERSION.to_owned());
    let picked = match http::send(
        state.http.as_ref(),
        http::json(format!("{base_url}/auto"), headers, &Value::Object(body)),
        Some(ROUTE_TIMEOUT),
    )
    .await
    {
        Ok(response) => serde_json::from_slice::<AutoRoute>(&response.body).ok(),
        Err(_) => None,
    };
    let now = now_ms();
    if let Some(picked) = picked.filter(|picked| snapshot.contains_key(&picked.selected_model.id)) {
        return Some(Pick {
            model: picked.selected_model.id,
            account: token,
            token: Some(picked.session_token),
            until: picked.expires_at * 1000.0 - ROUTE_TTL,
        });
    }
    let fallback = snapshot.values().find(|model| model.0.is_chat_fallback == Some(true)).or_else(|| {
        snapshot.values().find(|model| {
            model.0.billing.as_ref().is_some_and(|billing| {
                billing.auto_discount.is_some_and(|discount| discount != 0.0 && !discount.is_nan())
            })
        })
    })?;
    Some(Pick { model: fallback.0.id.clone(), account: token, token: None, until: now + ROUTE_TTL })
}

fn interaction_type(kind: RequestKind, child: bool) -> &'static str {
    if kind == RequestKind::Title {
        return "conversation-background";
    }
    if kind == RequestKind::Compaction {
        return "conversation-compaction";
    }
    if child {
        return "conversation-subagent";
    }
    "conversation-agent"
}

fn apply_headers(headers: &mut Vec<(String, String)>, token: &str, app: &AppInfo, metadata: Metadata) {
    headers.retain(|(name, _)| !name.eq_ignore_ascii_case("x-api-key"));
    super::upsert(headers, "authorization", format!("Bearer {token}"));
    super::upsert(headers, "user-agent", app.useragent());
    super::upsert(headers, "openai-intent", "conversation-edits".to_owned());
    super::upsert(headers, "x-github-api-version", API_VERSION.to_owned());
    if metadata.agent {
        super::upsert(headers, "x-initiator", "agent".to_owned());
    } else if !headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("x-initiator")) {
        headers.push(("x-initiator".to_owned(), "user".to_owned()));
    }
    if metadata.vision {
        super::upsert(headers, "copilot-vision-request", "true".to_owned());
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
}

fn strip_bearer(value: &str) -> &str {
    let Some(prefix) = value.get(..6) else {
        return value;
    };
    if !prefix.eq_ignore_ascii_case("bearer") {
        return value;
    }
    let rest = &value[6..];
    let token = rest.trim_start();
    if token.len() == rest.len() {
        return value;
    }
    token
}

fn request_metadata(url: &str, body: Option<&Value>) -> Metadata {
    let Some(body) = body.and_then(Value::as_object) else {
        return Metadata::default();
    };
    if let Some(input) = body.get("input").and_then(Value::as_array) {
        return Metadata {
            agent: last_role(input) != Some("user"),
            vision: input.iter().any(|item| {
                [item.get("content"), item.get("output")]
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_array)
                    .flatten()
                    .any(|part| part.get("type").and_then(Value::as_str) == Some("input_image"))
            }),
        };
    }
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return Metadata::default();
    };
    if url.contains("completions") {
        let previous = messages.len().checked_sub(2).and_then(|index| messages.get(index));
        return Metadata {
            agent: last_role(messages) != Some("user")
                || previous.and_then(|previous| previous.get("role")).and_then(Value::as_str) == Some("tool"),
            vision: messages.iter().any(|message| {
                message.get("content").and_then(Value::as_array).is_some_and(|parts| {
                    parts.iter().any(|part| part.get("type").and_then(Value::as_str) == Some("image_url"))
                })
            }),
        };
    }
    let content = messages.last().and_then(|last| last.get("content")).and_then(Value::as_array);
    Metadata {
        agent: last_role(messages) != Some("user")
            || content.is_none_or(|content| {
                !content
                    .iter()
                    .any(|part| part.is_object() && part.get("type").and_then(Value::as_str) != Some("tool_result"))
            }),
        vision: messages.iter().any(|message| {
            message.get("content").and_then(Value::as_array).is_some_and(|parts| parts.iter().any(is_image_part))
        }),
    }
}

fn last_role(messages: &[Value]) -> Option<&str> {
    messages.last().and_then(|last| last.get("role")).and_then(Value::as_str)
}

fn is_image_part(part: &Value) -> bool {
    match part.get("type").and_then(Value::as_str) {
        Some("image") => true,
        Some("tool_result") => part.get("content").and_then(Value::as_array).is_some_and(|nested| {
            nested.iter().any(|nested| nested.get("type").and_then(Value::as_str) == Some("image"))
        }),
        _ => false,
    }
}

fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_millis() as f64).unwrap_or(0.0)
}
