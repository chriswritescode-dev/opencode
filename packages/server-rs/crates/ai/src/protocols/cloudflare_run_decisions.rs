use serde::Deserialize;
use serde_json::{Map, Value};

use crate::media::protocol::{Identity, TextResponse};
use crate::modalities::decision::{DecisionRequest, DecisionResponse};
use crate::protocols::system_one::{self, Response};
use crate::route::decision::DecisionProtocol;
use crate::schema::AiError;

pub const ID: &str = "cloudflare-run-decisions";
pub const NAME: &str = "Cloudflare Run";

pub struct CloudflareRunDecisions;

#[derive(Deserialize)]
struct Envelope {
    result: RunResult,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RunResult {
    Answers(Response),
    Run { state: String, result: Response },
}

impl DecisionProtocol for CloudflareRunDecisions {
    fn id(&self) -> &str {
        ID
    }

    fn name(&self) -> &str {
        NAME
    }

    fn inline_media(&self) -> bool {
        true
    }

    fn body_from(&self, request: &DecisionRequest) -> Result<Map<String, Value>, AiError> {
        let input = system_one::encode_body(request, "root")?;
        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(request.model.id.clone()));
        body.insert("input".to_string(), Value::Object(input));
        Ok(body)
    }

    fn decode(
        &self,
        identity: &Identity,
        text: &TextResponse,
        request: &DecisionRequest,
        provider_metadata_key: &str,
    ) -> Result<DecisionResponse, AiError> {
        let envelope: Envelope = identity.decode_json(text)?;
        let response = match envelope.result {
            RunResult::Answers(response) => response,
            RunResult::Run { state, result } => {
                if state != "Completed" {
                    return Err(identity.invalid(
                        format!("{} returned an invalid response", identity.name),
                        Some(text.body.clone()),
                        Some(text.http.clone()),
                    ));
                }
                result
            }
        };
        system_one::to_response(response, request, provider_metadata_key, |message| {
            identity.invalid(message, Some(text.body.clone()), Some(text.http.clone()))
        })
    }
}
