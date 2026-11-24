use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::ExecutionContext;
use crate::media::route::MediaRequest;
use crate::route::decision::DecisionRoute;
use crate::route::executor::{BoxFuture, RequestExecutor};
use crate::schema::{
    AiError, AiErrorReason, HttpOptions, MediaAsset, ModelId, ProviderId, ProviderMetadata, ProviderOptions,
    RequestOptions, RequestTimeout, StringMap,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
#[allow(clippy::large_enum_variant)]
pub enum DecisionPart {
    Text {
        text: String,
    },
    Json {
        value: Value,
    },
    Media {
        media: MediaAsset,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChoiceValue {
    String(String),
    Bool(bool),
}

impl ChoiceValue {
    pub fn key(&self) -> String {
        match self {
            ChoiceValue::String(value) => value.clone(),
            ChoiceValue::Bool(value) => value.to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PredicateCriteria {
    #[serde(rename = "true", default, skip_serializing_if = "Option::is_none")]
    pub true_value: Option<String>,
    #[serde(rename = "false", default, skip_serializing_if = "Option::is_none")]
    pub false_value: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChoiceOption {
    pub value: ChoiceValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScoreLevel {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DecisionQuestion {
    Predicate {
        name: String,
        instructions: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<PredicateCriteria>,
    },
    Choice {
        name: String,
        instructions: String,
        choices: Vec<ChoiceOption>,
    },
    Score {
        name: String,
        instructions: String,
        levels: Vec<ScoreLevel>,
    },
}

impl DecisionQuestion {
    pub fn name(&self) -> &str {
        match self {
            DecisionQuestion::Predicate { name, .. }
            | DecisionQuestion::Choice { name, .. }
            | DecisionQuestion::Score { name, .. } => name,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChoiceProbability {
    pub value: ChoiceValue,
    pub probability: Number,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScoreProbability {
    pub value: i64,
    pub label: String,
    pub probability: Number,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DecisionAnswer {
    Predicate {
        name: String,
        probability: Number,
    },
    Choice {
        name: String,
        choice: ChoiceValue,
        probabilities: Vec<ChoiceProbability>,
        confidence: Number,
    },
    Score {
        name: String,
        score: Number,
        probabilities: Vec<ScoreProbability>,
        confidence: Number,
    },
    Error {
        name: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

impl DecisionAnswer {
    pub fn name(&self) -> &str {
        match self {
            DecisionAnswer::Predicate { name, .. }
            | DecisionAnswer::Choice { name, .. }
            | DecisionAnswer::Score { name, .. }
            | DecisionAnswer::Error { name, .. } => name,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DecisionUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Number>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionResponse {
    pub answers: Vec<DecisionAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<DecisionUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

pub fn normalized_number(number: &Number) -> Number {
    let Some(value) = number.as_f64() else {
        return number.clone();
    };
    crate::protocols::shared::number_from_f64(value).unwrap_or_else(|| number.clone())
}

pub fn usage_total(input: Option<&Number>, output: Option<&Number>, total: Option<&Number>) -> Option<Number> {
    if let Some(total) = total {
        return Some(normalized_number(total));
    }
    if input.is_none() && output.is_none() {
        return None;
    }
    crate::protocols::shared::number_from_f64(
        input.and_then(Number::as_f64).unwrap_or(0.0) + output.and_then(Number::as_f64).unwrap_or(0.0),
    )
}

#[derive(Clone)]
pub struct DecisionModel {
    pub id: ModelId,
    pub provider: ProviderId,
    pub route: Arc<DecisionRoute>,
    pub transport: Option<RequestOptions>,
    pub compatibility: Option<Value>,
    pub provider_options: Option<ProviderOptions>,
}

#[derive(Clone)]
pub struct DecisionRequest {
    pub model: DecisionModel,
    pub input: Vec<DecisionPart>,
    pub questions: Vec<DecisionQuestion>,
    pub provider_options: Option<ProviderOptions>,
    pub headers: Option<StringMap>,
    pub timeout: Option<RequestTimeout>,
    pub http: Option<HttpOptions>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRequestInput {
    pub input: Vec<DecisionPart>,
    pub questions: Vec<DecisionQuestion>,
    #[serde(default)]
    pub provider_options: Option<ProviderOptions>,
    #[serde(default)]
    pub headers: Option<StringMap>,
    #[serde(default)]
    pub timeout: Option<RequestTimeout>,
    #[serde(default)]
    pub http: Option<HttpOptions>,
}

impl DecisionRequestInput {
    pub fn with_model(self, model: DecisionModel) -> DecisionRequest {
        DecisionRequest {
            model,
            input: self.input,
            questions: self.questions,
            provider_options: self.provider_options,
            headers: self.headers,
            timeout: self.timeout,
            http: self.http,
        }
    }
}

impl MediaRequest for DecisionRequest {
    fn id(&self) -> &str {
        &self.model.id
    }

    fn provider(&self) -> &ProviderId {
        &self.model.provider
    }

    fn headers(&self) -> Option<&StringMap> {
        self.headers.as_ref()
    }

    fn timeout(&self) -> Option<&RequestTimeout> {
        self.timeout.as_ref()
    }

    fn http(&self) -> Option<&HttpOptions> {
        self.http.as_ref()
    }

    fn provider_options(&self) -> Option<&ProviderOptions> {
        self.provider_options.as_ref()
    }

    fn model_transport(&self) -> Option<&RequestOptions> {
        self.model.transport.as_ref()
    }

    fn model_provider_options(&self) -> Option<&ProviderOptions> {
        self.model.provider_options.as_ref()
    }

    fn resolved(
        &self,
        headers: Option<StringMap>,
        timeout: Option<RequestTimeout>,
        http: Option<HttpOptions>,
        provider_options: Option<ProviderOptions>,
    ) -> Self {
        Self {
            model: self.model.clone(),
            input: self.input.clone(),
            questions: self.questions.clone(),
            provider_options,
            headers,
            timeout,
            http,
        }
    }
}

pub struct DecisionClient;

impl DecisionClient {
    pub fn generate(
        request: DecisionRequest,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<DecisionResponse, AiError>> {
        let route = request.model.route.clone();
        let executor = executor.clone();
        let context = context.clone();
        Box::pin(async move {
            let response = route.generate(request.clone(), &executor, &context).await?;
            if answers_match(&request.questions, &response.answers) {
                return Ok(response);
            }
            Err(AiError {
                reason: AiErrorReason::InvalidProviderOutput {
                    route: Some(route.id.clone()),
                    message: "Decision answers do not match the requested questions".to_string(),
                    body: serde_json::to_string(&response.answers).ok(),
                    http: None,
                    classification: None,
                },
            })
        })
    }
}

fn answers_match(questions: &[DecisionQuestion], answers: &[DecisionAnswer]) -> bool {
    if questions.len() != answers.len() {
        return false;
    }
    questions.iter().zip(answers).all(|(question, answer)| {
        if answer.name() != question.name() {
            return false;
        }
        if matches!(answer, DecisionAnswer::Error { .. }) {
            return true;
        }
        match question {
            DecisionQuestion::Predicate { .. } => matches!(answer, DecisionAnswer::Predicate { .. }),
            DecisionQuestion::Choice { choices, .. } => {
                let DecisionAnswer::Choice { choice, probabilities, .. } = answer else {
                    return false;
                };
                let values = choices.iter().map(|option| option.value.clone()).collect::<Vec<_>>();
                values.contains(choice)
                    && same_members(&values, &probabilities.iter().map(|item| item.value.clone()).collect::<Vec<_>>())
            }
            DecisionQuestion::Score { levels, .. } => {
                let DecisionAnswer::Score { score, probabilities, .. } = answer else {
                    return false;
                };
                let Some(score) = score.as_f64() else {
                    return false;
                };
                if score < 0.0 || score > levels.len() as f64 - 1.0 {
                    return false;
                }
                same_members(
                    &(0..levels.len() as i64).collect::<Vec<_>>(),
                    &probabilities.iter().map(|item| item.value).collect::<Vec<_>>(),
                )
            }
        }
    })
}

fn same_members<A: PartialEq + Eq + std::hash::Hash>(expected: &[A], received: &[A]) -> bool {
    expected.len() == received.len()
        && received.iter().collect::<HashSet<&A>>().len() == received.len()
        && received.iter().all(|item| expected.contains(item))
}

pub fn usage_number(map: &Map<String, Value>, key: &str) -> Option<Number> {
    map.get(key).and_then(Value::as_number).map(normalized_number)
}
