use crate::builtin::{Builtin, Services};

pub const ID: &str = "opencode.provider.nvidia";

pub async fn register(services: &Services, _context: &super::PluginContext, builtin: &mut Builtin) {
    let registration = services
        .provider
        .transform(|editor| {
            for item in editor.list() {
                if item.provider.package != "@opencode/ai/providers/openai-compatible" {
                    continue;
                }
                if super::base_url(&item.provider) != Some("https://integrate.api.nvidia.com/v1") {
                    continue;
                }
                editor.update(&item.provider.id, |provider| {
                    super::set_headers(provider, &[("HTTP-Referer", "https://opencode.ai/"), ("X-Title", "opencode")]);
                    super::default_headers(provider, &[("X-BILLING-INVOKE-ORIGIN", "OpenCode")]);
                });
            }
            Ok(())
        })
        .await;
    builtin.registrations.push(registration);
}
