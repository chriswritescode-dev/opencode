use crate::builtin::{Builtin, Services};

pub const ID: &str = "opencode.provider.cerebras";

pub async fn register(services: &Services, _context: &super::PluginContext, builtin: &mut Builtin) {
    let registration = services
        .provider
        .transform(|editor| {
            for item in editor.list() {
                if item.provider.package != "@opencode/ai/providers/cerebras" {
                    continue;
                }
                editor.update(&item.provider.id, |provider| {
                    let headers = provider.headers.get_or_insert_with(Default::default);
                    headers.0.retain(|(name, _)| name != "X-Cerebras-3rd-Party-Integration");
                    headers.0.push(("X-Cerebras-3rd-Party-Integration".to_owned(), "opencode".to_owned()));
                });
            }
            Ok(())
        })
        .await;
    builtin.registrations.push(registration);
}
