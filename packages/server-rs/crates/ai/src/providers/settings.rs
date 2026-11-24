use serde_json::{Map, Value};

use crate::route::auth::ApiKey;
use crate::schema::{HttpOptions, RequestOptions, StringMap};

#[derive(Debug, Clone, Default)]
pub struct Settings {
    values: Map<String, Value>,
}

impl Settings {
    pub fn new(values: Map<String, Value>) -> Self {
        Self { values }
    }

    pub fn value(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    pub fn string(&self, key: &str) -> Option<&str> {
        self.values.get(key).and_then(Value::as_str)
    }

    pub fn boolean(&self, key: &str) -> Option<bool> {
        self.values.get(key).and_then(Value::as_bool)
    }

    pub fn optional_string(&self, key: &str) -> Option<String> {
        self.string(key).map(str::to_string)
    }

    pub fn optional_number(&self, key: &str) -> Option<serde_json::Number> {
        self.values.get(key).and_then(Value::as_number).cloned()
    }

    pub fn api_key(&self) -> Option<ApiKey> {
        match self.values.get("apiKey") {
            Some(Value::String(value)) => Some(ApiKey::Value(value.clone())),
            Some(Value::Bool(false)) => Some(ApiKey::Disabled),
            _ => None,
        }
    }

    pub fn headers(&self) -> Option<StringMap> {
        string_map(self.values.get("headers"))
    }

    pub fn body(&self) -> Option<Map<String, Value>> {
        self.values.get("body").and_then(Value::as_object).cloned()
    }

    pub fn query_params(&self) -> Option<StringMap> {
        string_map(self.values.get("queryParams"))
    }

    pub fn request_options(&self, declared: &[&str]) -> Option<RequestOptions> {
        let provider_options = self
            .values
            .iter()
            .filter(|(key, _)| !declared.contains(&key.as_str()) && !matches!(key.as_str(), "headers" | "body"));
        let mut provider = Map::new();
        for (key, value) in provider_options {
            provider.insert(key.clone(), value.clone());
        }
        let headers = self.headers();
        let body = self.body();
        let http = body.map(|body| HttpOptions { body: Some(body), ..HttpOptions::default() });
        let options = RequestOptions {
            headers,
            http,
            provider_options: (!provider.is_empty()).then_some(provider),
            ..RequestOptions::default()
        };
        (options != RequestOptions::default()).then_some(options)
    }
}

fn string_map(value: Option<&Value>) -> Option<StringMap> {
    let entries = value?.as_object()?;
    let pairs = entries
        .iter()
        .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
        .collect::<Vec<_>>();
    (!pairs.is_empty()).then(|| StringMap::from_pairs(pairs))
}
