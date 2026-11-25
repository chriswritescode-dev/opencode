use opencode_contract::generated::{IntegrationEnvMethod, IntegrationEnvMethodType, IntegrationID, IntegrationMethod};

use crate::builtin::{Builtin, Services};
use crate::integration::Implementation;

pub const ID: &str = "opencode.provider.neon";

pub async fn register(services: &Services, _context: &super::PluginContext, builtin: &mut Builtin) {
    let registration = services
        .integration
        .transform(|editor| {
            editor.method_update(Implementation::Method {
                integration_id: IntegrationID("neon".to_owned()),
                method: IntegrationMethod::Env(IntegrationEnvMethod {
                    r#type: IntegrationEnvMethodType::Env,
                    names: vec!["NEON_AI_GATEWAY_TOKEN".to_owned()],
                }),
            });
            Ok(())
        })
        .await;
    builtin.registrations.push(registration);
}
