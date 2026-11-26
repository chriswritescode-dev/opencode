use std::sync::Arc;

use opencode_contract::generated::{CredentialValue, IntegrationID, ProviderID};
use url::Url;

use crate::builtin::{Builtin, Services};
use crate::integration::IntegrationService;
use crate::provider_plugin::hooks::{HttpRequest, WsHandshake};
use crate::provider_plugin::settings;
use crate::provider_plugin::{AppInfo, PluginContext};

use super::METHOD_ID;
use super::discovery::{COGNITIVE_SCOPE, Discovery, FOUNDRY_SCOPE};

#[derive(Clone)]
struct State {
    integrations: Arc<IntegrationService>,
    discovery: Arc<Discovery>,
    app: AppInfo,
    provider_id: ProviderID,
}

pub fn register(
    services: &Services,
    context: &PluginContext,
    builtin: &mut Builtin,
    provider_id: &ProviderID,
    discovery: &Arc<Discovery>,
) {
    let state = State {
        integrations: services.integration.clone(),
        discovery: discovery.clone(),
        app: context.app.clone(),
        provider_id: provider_id.clone(),
    };
    let hooks = context.hooks.clone();
    builtin.hooks.push(hooks.on_http_request(Some(provider_id.clone()), {
        let state = state.clone();
        move |mut event: HttpRequest| {
            let state = state.clone();
            Box::pin(async move {
                let Some(authorization) = authorization(&state, &event.request.url).await else {
                    return event;
                };
                event.request.headers.retain(|(name, _)| {
                    !name.eq_ignore_ascii_case("api-key") && !name.eq_ignore_ascii_case("x-api-key")
                });
                upsert(&mut event.request.headers, "authorization", authorization);
                upsert(&mut event.request.headers, "user-agent", state.app.useragent());
                event
            })
        }
    }));
    builtin.hooks.push(hooks.on_ws_handshake(Some(provider_id.clone()), {
        let state = state.clone();
        move |mut event: WsHandshake| {
            let state = state.clone();
            Box::pin(async move {
                let Some(authorization) = authorization(&state, &event.url).await else {
                    return event;
                };
                event.headers.shift_remove("api-key");
                event.headers.shift_remove("x-api-key");
                event.headers.insert("authorization".to_owned(), authorization);
                event.headers.insert("user-agent".to_owned(), state.app.useragent());
                event
            })
        }
    }));
}

async fn authorization(state: &State, url: &str) -> Option<String> {
    let (_, credential) =
        settings::active_credential(&state.integrations, &IntegrationID(state.provider_id.0.clone())).await;
    let usable = match &credential {
        Some(CredentialValue::External(credential)) => credential.method_id.0 == METHOD_ID,
        _ => false,
    };
    if !usable {
        return None;
    }
    let scope = scope(url)?;
    match state.discovery.token(scope).await {
        Ok(token) => Some(format!("Bearer {}", token.access)),
        Err(error) => {
            log::warn!("Azure token request failed: {error}");
            None
        }
    }
}

fn scope(url: &str) -> Option<&'static str> {
    let target = Url::parse(url).ok()?;
    if target.host_str().is_some_and(|host| host.ends_with(".services.ai.azure.com"))
        && !target.path().starts_with("/models")
    {
        return Some(FOUNDRY_SCOPE);
    }
    Some(COGNITIVE_SCOPE)
}

fn upsert(headers: &mut Vec<(String, String)>, name: &str, value: String) {
    headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
    headers.push((name.to_owned(), value));
}
