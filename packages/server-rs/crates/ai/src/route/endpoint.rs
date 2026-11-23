use std::sync::Arc;

use serde_json::Value;
use url::Url;

use crate::schema::{AiError, AiErrorReason, LlmRequest, StringMap};

pub type PathFunction = Arc<dyn Fn(&Value, &LlmRequest) -> String + Send + Sync>;

#[derive(Clone)]
pub enum PathPart {
    Static(String),
    Function(PathFunction),
    Default,
}

#[derive(Clone)]
pub struct Endpoint {
    pub base_url: Option<String>,
    pub path: PathPart,
    pub query: StringMap,
}

pub struct EndpointInput<'a> {
    pub request: &'a LlmRequest,
    pub body: &'a Value,
}

#[derive(Clone, Default)]
pub struct EndpointPatch {
    pub base_url: Option<Option<String>>,
    pub path: Option<PathPart>,
    pub query: Option<StringMap>,
}

impl Endpoint {
    pub fn path(value: impl Into<String>) -> Self {
        Self { base_url: None, path: PathPart::Static(value.into()), query: StringMap::new() }
    }

    pub fn new(base_url: impl Into<String>, path: impl Into<String>) -> Self {
        Self { base_url: Some(base_url.into()), path: PathPart::Static(path.into()), query: StringMap::new() }
    }

    pub fn function(path: PathFunction) -> Self {
        Self { base_url: None, path: PathPart::Function(path), query: StringMap::new() }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    pub fn with_query(mut self, query: StringMap) -> Self {
        self.query = query;
        self
    }

    pub fn merge(&self, patch: EndpointPatch) -> Self {
        let mut query = self.query.clone();
        if let Some(extra) = patch.query {
            for (key, value) in extra.iter() {
                query.insert(key.to_string(), value.to_string());
            }
        }
        Self {
            base_url: patch.base_url.unwrap_or_else(|| self.base_url.clone()),
            path: patch.path.unwrap_or_else(|| self.path.clone()),
            query,
        }
    }

    pub fn render(&self, input: &EndpointInput) -> Result<Url, AiError> {
        let part = match &self.path {
            PathPart::Static(part) => part.clone(),
            PathPart::Function(function) => function(input.body, input.request),
            PathPart::Default => return Err(unresolved_path()),
        };
        let base = self.base_url.as_deref().unwrap_or("");
        let value = format!("{}{}", trim_base_url(base), part);
        let mut url = Url::parse(&value).map_err(|error| AiError {
            reason: AiErrorReason::InvalidRequest {
                message: format!("Invalid endpoint URL: {error}"),
                body: None,
                http: None,
                parameter: None,
                classification: None,
            },
        })?;
        for (key, value) in self.query.iter() {
            set_query(&mut url, key, value);
        }
        Ok(url)
    }
}

fn unresolved_path() -> AiError {
    AiError {
        reason: AiErrorReason::InvalidRequest {
            message: "Endpoint path is not resolved".to_string(),
            body: None,
            http: None,
            parameter: None,
            classification: None,
        },
    }
}

pub fn trim_base_url(value: &str) -> &str {
    value.trim_end_matches('/')
}

pub fn set_query(url: &mut Url, key: &str, value: &str) {
    let mut pairs: Vec<(String, String)> =
        url.query_pairs().map(|(existing, value)| (existing.into_owned(), value.into_owned())).collect();
    let mut replaced = false;
    for pair in &mut pairs {
        if pair.0 == key {
            pair.1 = value.to_string();
            replaced = true;
            break;
        }
    }
    if !replaced {
        pairs.push((key.to_string(), value.to_string()));
    }
    url.set_query(None);
    let mut query = url.query_pairs_mut();
    for (existing, value) in pairs {
        query.append_pair(&existing, &value);
    }
}
