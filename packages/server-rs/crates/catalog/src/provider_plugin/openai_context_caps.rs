use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use opencode_contract::generated::{CredentialValue, IntegrationID, ModelInfoLimit, ProviderID};

use crate::builtin::{Builtin, Services};
use crate::provider_plugin::{refresh, settings};

pub const ID: &str = "opencode.provider.openai-context-caps";

const LEGACY_INPUT: f64 = 272_000.0;

const CURRENT_INPUT: f64 = 500_000.0;

const METHOD_IDS: [&str; 3] = ["chatgpt-browser", "chatgpt-headless", "chatgpt-token-sharing"];

const INTEGRATION_ID: &str = "openai";

pub async fn register(services: &Services, _context: &super::PluginContext, builtin: &mut Builtin) {
    let (_, credential) = settings::stored_credential(services, &IntegrationID(INTEGRATION_ID.to_owned())).await;
    let signed_in = Arc::new(AtomicBool::new(is_signed_in(credential.as_ref())));

    let model_registration = {
        let signed_in = signed_in.clone();
        services
            .model
            .transform(move |editor| {
                if !signed_in.load(Ordering::Relaxed) {
                    return Ok(());
                }
                for model in editor.list(Some(&ProviderID(INTEGRATION_ID.to_owned()))) {
                    let input = cap(model.model_id.as_str());
                    let current = model.limit.input.filter(|value| *value != 0.0).unwrap_or(model.limit.context);
                    if current <= input {
                        continue;
                    }
                    let output = model.limit.output;
                    model.limit = ModelInfoLimit { context: input + output, input: Some(input), output };
                }
                Ok(())
            })
            .await
    };
    builtin.registrations.push(model_registration);

    let integrations = services.integration.clone();
    let credentials = services.credentials.clone();
    let providers = services.provider.clone();
    let handle = refresh::on_switch(&services.bus, INTEGRATION_ID, move || {
        let integrations = integrations.clone();
        let credentials = credentials.clone();
        let providers = providers.clone();
        let signed_in = signed_in.clone();
        Box::pin(async move {
            let (_, credential) = settings::read_stored_credential(
                &integrations,
                &credentials,
                &IntegrationID(INTEGRATION_ID.to_owned()),
            )
            .await;
            let next = is_signed_in(credential.as_ref());
            if signed_in.swap(next, Ordering::Relaxed) == next {
                return;
            }
            providers.reload().await;
        })
    });
    builtin.tasks.push(handle);
}

fn is_signed_in(credential: Option<&CredentialValue>) -> bool {
    matches!(
        credential,
        Some(CredentialValue::Oauth(oauth)) if METHOD_IDS.contains(&oauth.method_id.0.as_str())
    )
}

fn cap(api_id: &str) -> f64 {
    let Some(rest) = api_id.strip_prefix("gpt-") else {
        return CURRENT_INPUT;
    };
    if !rest.starts_with(|character: char| character.is_ascii_digit()) {
        return CURRENT_INPUT;
    }
    if api_id.contains("-astra") {
        return LEGACY_INPUT;
    }
    let major_end = rest.find(|character: char| !character.is_ascii_digit()).unwrap_or(rest.len());
    let major: u32 = rest[..major_end].parse().unwrap_or(u32::MAX);
    let minor = rest[major_end..]
        .strip_prefix('.')
        .map(|rest| {
            let end = rest.find(|character: char| !character.is_ascii_digit()).unwrap_or(rest.len());
            rest[..end].parse().unwrap_or(0)
        })
        .unwrap_or(0);
    if major > 6 || (major == 6 && minor >= 1) { CURRENT_INPUT } else { LEGACY_INPUT }
}
