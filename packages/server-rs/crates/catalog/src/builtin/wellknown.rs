use std::sync::Arc;

use opencode_config::wellknown::WellKnown;
use opencode_contract::generated::{
    IntegrationCommandMethod, IntegrationCommandMethodType, IntegrationID, IntegrationMethod, IntegrationMethodID,
};
use serde_json::Value;
use tokio::sync::broadcast;
use url::Url;

use crate::builtin::{Builtin, Services};
use crate::integration::{Implementation, IntegrationService};

pub async fn register(services: &Services, builtin: &mut Builtin) {
    services.wellknown.entries().await;

    let transform = services
        .integration
        .transform({
            let wellknown = services.wellknown.clone();
            move |editor| {
                for entry in wellknown.snapshot() {
                    let Some(command) =
                        entry.manifest.get("auth").and_then(|auth| auth.get("command")).and_then(Value::as_array)
                    else {
                        continue;
                    };
                    let integration_id = IntegrationID(entry.integration_id.clone());
                    let name = host(&entry.origin);
                    editor.update(&integration_id, |integration| integration.name = name);
                    editor.method_update(Implementation::Method {
                        integration_id,
                        method: IntegrationMethod::Command(IntegrationCommandMethod {
                            id: IntegrationMethodID("login".to_owned()),
                            r#type: IntegrationCommandMethodType::Command,
                            label: "Log in".to_owned(),
                            command: command.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
                        }),
                    });
                }
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(transform);

    builtin.tasks.push(tokio::spawn(reload(services.integration.clone(), services.wellknown.clone())));
}

fn host(origin: &str) -> String {
    Url::parse(origin).ok().and_then(|url| url.host_str().map(str::to_owned)).unwrap_or_else(|| origin.to_owned())
}

async fn reload(integration: Arc<IntegrationService>, wellknown: Arc<WellKnown>) {
    let mut updates = wellknown.subscribe();
    loop {
        match updates.recv().await {
            Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => integration.reload().await,
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}
