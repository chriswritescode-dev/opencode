use opencode_contract::generated::{ProviderCompatibility, ProviderID};

use crate::builtin::{Builtin, Services};

pub const ID: &str = "opencode.provider.prompt-cache-key";

const PROVIDER_IDS: [&str; 12] = [
    "abliteration-ai",
    "aihubmix",
    "anyapi",
    "auriko",
    "edenai",
    "jalapeno",
    "llama",
    "morph",
    "snowflake-cortex",
    "tencent-tokenhub",
    "upstage",
    "venice",
];

pub async fn register(services: &Services, _context: &super::PluginContext, builtin: &mut Builtin) {
    let registration = services
        .provider
        .transform(|editor| {
            for id in PROVIDER_IDS {
                let provider_id = ProviderID(id.to_owned());
                if editor.get(&provider_id).is_none() {
                    continue;
                }
                editor.update(&provider_id, |provider| {
                    let compatibility = provider.compatibility.get_or_insert(ProviderCompatibility {
                        reasoning_field: None,
                        require_reasoning: None,
                        max_tokens_field: None,
                        require_finish_reason: None,
                        require_assistant_after_tool: None,
                        supports_prompt_cache_key: None,
                        stream_usage: None,
                        cache_format: None,
                        media_input: None,
                    });
                    compatibility.supports_prompt_cache_key = Some(true);
                });
            }
            Ok(())
        })
        .await;
    builtin.registrations.push(registration);
}
