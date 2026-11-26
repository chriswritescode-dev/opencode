use crate::builtin::{Builtin, Services};

pub const ID: &str = "opencode.provider.openrouter";

const PACKAGE: &str = "@opencode/ai/providers/openrouter";

const DISABLED_MODELS: [&str; 2] = ["gpt-5-chat-latest", "openai/gpt-5-chat"];

pub async fn register(services: &Services, _context: &super::PluginContext, builtin: &mut Builtin) {
    let provider_registration = services
        .provider
        .transform(|editor| {
            for item in editor.list() {
                if item.provider.package != PACKAGE {
                    continue;
                }
                editor.update(&item.provider.id, |provider| {
                    super::set_headers(provider, &[("HTTP-Referer", "https://opencode.ai/"), ("X-Title", "opencode")]);
                });
            }
            Ok(())
        })
        .await;
    builtin.registrations.push(provider_registration);

    let model_registration = services
        .model
        .transform(|editor| {
            for item in editor.provider_list() {
                if item.provider.package != PACKAGE {
                    continue;
                }
                for model_id in DISABLED_MODELS {
                    if editor.get(&item.provider.id, model_id).is_none() {
                        continue;
                    }
                    editor.update(&item.provider.id, model_id, |model| model.enabled = false);
                }
            }
            Ok(())
        })
        .await;
    builtin.registrations.push(model_registration);
}
