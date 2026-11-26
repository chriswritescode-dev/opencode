use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use opencode_ai::route::executor::{HttpResponse, HttpSender};
use opencode_contract::codec::{decode_json, encode_json};
use opencode_contract::generated::{
    ConnectionInfo, CredentialValue, IntegrationID, WebSearchID, WebsearchQueryResponseData,
};
use serde_json::Value;

use crate::console_access;
use crate::integration::{self, IntegrationService};
use crate::provider_plugin::http;

use super::follow::follow;

const PROVIDER_ID: &str = "opencode";

const NAME: &str = "OpenCode Web Search";

const TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Clone)]
pub struct ConsoleWebSearch {
    pub provider_id: WebSearchID,
    pub name: &'static str,
    connection: ConnectionInfo,
    integration: Arc<IntegrationService>,
    http: Arc<dyn HttpSender>,
}

impl ConsoleWebSearch {
    pub fn new(integration: Arc<IntegrationService>, http: Arc<dyn HttpSender>, connection: ConnectionInfo) -> Self {
        Self { provider_id: WebSearchID(PROVIDER_ID.to_owned()), name: NAME, connection, integration, http }
    }

    pub async fn execute(&self, query: &str) -> Result<Vec<Value>, String> {
        let active = self
            .integration
            .connection()
            .active(&IntegrationID(PROVIDER_ID.to_owned()))
            .await
            .map_err(|error| error.to_string())?
            .filter(|active| integration::key(active) == integration::key(&self.connection))
            .ok_or_else(|| "OpenCode Console connection changed".to_owned())?;
        let credential = self
            .integration
            .connection()
            .resolve(&active)
            .await
            .map_err(|error| error.to_string())?
            .filter(|credential| !matches!(credential, CredentialValue::External(_)))
            .ok_or_else(|| "OpenCode Console is not connected".to_owned())?;
        let server = console_access::normalize_server(&console_access::server(&credential))?;
        let mut headers = vec![("accept".to_owned(), "application/json".to_owned())];
        headers.extend(console_access::headers(&credential));
        let request = http::json(
            format!("{server}/api/websearch"),
            headers,
            &serde_json::json!({ "query": query, "providerID": self.provider_id.0 }),
        );
        let body = tokio::time::timeout(TIMEOUT, async {
            let response = follow(self.http.as_ref(), request, 0).await?;
            read_body(response).await
        })
        .await
        .map_err(|_| "OpenCode web search request timed out".to_owned())??;
        let response: WebsearchQueryResponseData =
            decode_json(&String::from_utf8(body).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        if response.provider_id != self.provider_id {
            return Err(format!(
                "OpenCode web search returned provider {} instead of {}",
                response.provider_id.0, self.provider_id.0
            ));
        }
        Ok(response
            .results
            .iter()
            .map(|result| serde_json::from_str(&encode_json(result)).expect("a web search result encodes as JSON"))
            .collect())
    }
}

async fn read_body(response: HttpResponse) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    let mut stream = response.body;
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.map_err(|error| error.message().to_owned())?);
    }
    Ok(body)
}
