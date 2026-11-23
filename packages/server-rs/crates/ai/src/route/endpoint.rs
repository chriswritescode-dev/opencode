use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use url::Url;

use crate::schema::{AiError, AiErrorReason, LlmRequest};

pub type PathFunction = Arc<dyn Fn(&Value, &LlmRequest) -> String + Send + Sync>;

#[derive(Clone)]
pub enum PathPart {
    Static(String),
    Function(PathFunction),
}

#[derive(Clone)]
pub struct Endpoint {
    pub base_url: Option<String>,
    pub path: PathPart,
    pub query: BTreeMap<String, String>,
}

pub struct EndpointInput<'a> {
    pub request: &'a LlmRequest,
    pub body: &'a Value,
}

#[derive(Clone, Default)]
pub struct EndpointPatch {
    pub base_url: Option<Option<String>>,
    pub path: Option<PathPart>,
    pub query: Option<BTreeMap<String, String>>,
}

impl Endpoint {
    pub fn path(value: impl Into<String>) -> Self {
        Self { base_url: None, path: PathPart::Static(value.into()), query: BTreeMap::new() }
    }

    pub fn new(base_url: impl Into<String>, path: impl Into<String>) -> Self {
        Self { base_url: Some(base_url.into()), path: PathPart::Static(path.into()), query: BTreeMap::new() }
    }

    pub fn function(path: PathFunction) -> Self {
        Self { base_url: None, path: PathPart::Function(path), query: BTreeMap::new() }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    pub fn with_query(mut self, query: BTreeMap<String, String>) -> Self {
        self.query = query;
        self
    }

    pub fn merge(&self, patch: EndpointPatch) -> Self {
        let mut query = self.query.clone();
        if let Some(extra) = patch.query {
            query.extend(extra);
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
        for (key, value) in &self.query {
            set_query(&mut url, key, value);
        }
        Ok(url)
    }
}

pub fn trim_base_url(value: &str) -> &str {
    value.trim_end_matches('/')
}

pub fn set_query(url: &mut Url, key: &str, value: &str) {
    let existing: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(existing, _)| existing != key)
        .map(|(existing, value)| (existing.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    let mut pairs = url.query_pairs_mut();
    for (existing, value) in existing {
        pairs.append_pair(&existing, &value);
    }
    pairs.append_pair(key, value);
}
