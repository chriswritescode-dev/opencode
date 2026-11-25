use std::collections::HashSet;

use opencode_contract::generated::ProviderInfoActivation;

use crate::builtin::{Builtin, Services};

pub const ID: &str = "opencode.provider.llmgateway";

pub async fn register(services: &Services, _context: &super::PluginContext, builtin: &mut Builtin) {
    let configured: HashSet<String> = services
        .integration
        .list()
        .await
        .map(|list| list.into_iter().map(|item| item.id.0).collect())
        .unwrap_or_default();
    let registration = services
        .provider
        .transform(move |editor| {
            for item in editor.list() {
                if item.provider.activation == ProviderInfoActivation::Disabled {
                    continue;
                }
                if item.provider.package != "@opencode/ai/providers/openai-compatible" {
                    continue;
                }
                if super::base_url(&item.provider) != Some("https://api.llmgateway.io/v1") {
                    continue;
                }
                if !configured.contains(&item.provider.id.0) {
                    continue;
                }
                editor.update(&item.provider.id, |provider| {
                    super::set_headers(
                        provider,
                        &[("HTTP-Referer", "https://opencode.ai/"), ("X-Title", "opencode"), ("X-Source", "opencode")],
                    );
                });
            }
            Ok(())
        })
        .await;
    builtin.registrations.push(registration);
}
