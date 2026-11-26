use crate::builtin::{Builtin, Services};

pub const ID: &str = "opencode.provider.zenmux";

pub async fn register(services: &Services, _context: &super::PluginContext, builtin: &mut Builtin) {
    let registration = services
        .provider
        .transform(|editor| {
            for item in editor.list() {
                if item.provider.package != "@opencode/ai/providers/openai-compatible" {
                    continue;
                }
                if super::base_url(&item.provider) != Some("https://zenmux.ai/api/v1") {
                    continue;
                }
                editor.update(&item.provider.id, |provider| {
                    super::default_headers(
                        provider,
                        &[("HTTP-Referer", "https://opencode.ai/"), ("X-Title", "opencode")],
                    );
                });
            }
            Ok(())
        })
        .await;
    builtin.registrations.push(registration);
}
