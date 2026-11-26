pub mod sign_in;

use std::sync::Arc;

use opencode_contract::generated::{IntegrationID, IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod};

use crate::builtin::{Builtin, Services};
use crate::integration::Implementation;
use crate::provider_plugin::PluginContext;

pub const ID: &str = "opencode.provider.opencode";

const INTEGRATION_ID: &str = "opencode";

pub async fn register(services: &Services, context: &PluginContext, builtin: &mut Builtin) {
    let registration = services
        .integration
        .transform({
            let http = context.http.clone();
            let user_agent = context.app.useragent();
            move |editor| {
                let integration_id = IntegrationID(INTEGRATION_ID.to_owned());
                editor.update(&integration_id, |integration| integration.name = "OpenCode".to_owned());
                editor.method_update(Implementation::OAuth(Arc::new(sign_in::DeviceSignIn::new(
                    http.clone(),
                    user_agent.clone(),
                ))));
                editor.method_update(Implementation::Method {
                    integration_id,
                    method: IntegrationMethod::Key(IntegrationKeyMethod {
                        r#type: IntegrationKeyMethodType::Key,
                        label: Some("API key (service account)".to_owned()),
                        form: None,
                    }),
                });
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(registration);
}
