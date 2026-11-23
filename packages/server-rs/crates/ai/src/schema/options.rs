pub type ProviderOptions = serde_json::Map<String, serde_json::Value>;
pub type ProviderMetadata = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StringMap(Vec<(String, String)>);

impl StringMap {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut map = Self::new();
        for (key, value) in pairs {
            map.insert(key, value);
        }
        map
    }

    pub fn insert(&mut self, key: String, value: String) {
        match self.0.iter_mut().find(|(existing, _)| existing == &key) {
            Some(entry) => entry.1 = value,
            None => self.0.push((key, value)),
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(existing, _)| existing == key).map(|(_, value)| value.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(key, value)| (key.as_str(), value.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl serde::Serialize for StringMap {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> serde::Deserialize<'de> for StringMap {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = StringMap;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a map of strings")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut access: A) -> Result<StringMap, A::Error> {
                let mut map = StringMap::new();
                while let Some((key, value)) = access.next_entry::<String, String>()? {
                    map.insert(key, value);
                }
                Ok(map)
            }
        }

        deserializer.deserialize_map(Visitor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SanitizerKind {
    Gemini,
    Moonshot,
    None,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GenerationOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheHint {
    pub r#type: CacheHintType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheHintType {
    Ephemeral,
    Persistent,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum CachePolicy {
    Mode(CacheMode),
    Object(CachePolicyObject),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheMode {
    Auto,
    None,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CachePolicyObject {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<CacheMessages>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum CacheMessages {
    Named(String),
    Tail { tail: serde_json::Number },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestEncoding {
    Zstd,
    Gzip,
    None,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientTls {
    pub cert: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HttpOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<StringMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<RequestEncoding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<ClientTls>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum HttpTimeout {
    Millis(serde_json::Number),
    Disabled(bool),
}

impl<'de> serde::Deserialize<'de> for HttpTimeout {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
        match value {
            serde_json::Value::Bool(false) => Ok(HttpTimeout::Disabled(false)),
            serde_json::Value::Number(number) if number.as_f64().is_some_and(|number| number > 0.0) => {
                Ok(HttpTimeout::Millis(number))
            }
            other => Err(serde::de::Error::custom(format!(
                "expected a positive number of milliseconds or false, got {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TimeoutOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<HttpTimeout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HttpTimeout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk: Option<HttpTimeout>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum RequestTimeout {
    Timeout(HttpTimeout),
    Config(TimeoutOptions),
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestOptions {
    #[serde(flatten, default)]
    pub generation: GenerationOptions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_options: Option<ProviderOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<StringMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<RequestTimeout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpOptions>,
}

pub fn merge_generation(items: &[Option<&GenerationOptions>]) -> GenerationOptions {
    let last = |select: fn(&GenerationOptions) -> Option<&serde_json::Number>| {
        items.iter().filter_map(|item| *item).filter_map(select).next_back().cloned()
    };
    GenerationOptions {
        max_tokens: last(|item| item.max_tokens.as_ref()),
        temperature: last(|item| item.temperature.as_ref()),
        top_p: last(|item| item.top_p.as_ref()),
        top_k: last(|item| item.top_k.as_ref()),
        frequency_penalty: last(|item| item.frequency_penalty.as_ref()),
        presence_penalty: last(|item| item.presence_penalty.as_ref()),
        seed: last(|item| item.seed.as_ref()),
        stop: items.iter().filter_map(|item| *item).filter_map(|item| item.stop.clone()).next_back(),
    }
}

pub fn merge_provider_options(items: &[Option<&ProviderOptions>]) -> Option<ProviderOptions> {
    crate::json::merge_records(items.iter().filter_map(|item| *item))
}

pub fn merge_string_maps(items: &[Option<&StringMap>]) -> Option<StringMap> {
    let defined: Vec<&StringMap> = items.iter().filter_map(|item| *item).collect();
    if defined.is_empty() {
        return None;
    }
    if defined.len() == 1 {
        return Some(defined[0].clone());
    }
    let mut merged = StringMap::new();
    for item in defined {
        for (key, value) in item.iter() {
            merged.insert(key.to_string(), value.to_string());
        }
    }
    (!merged.is_empty()).then_some(merged)
}

pub fn merge_request_options(items: &[Option<&RequestOptions>]) -> Option<RequestOptions> {
    let generation = merge_generation(&items.iter().map(|item| item.map(|item| &item.generation)).collect::<Vec<_>>());
    let provider_options = merge_provider_options(
        &items.iter().map(|item| item.and_then(|item| item.provider_options.as_ref())).collect::<Vec<_>>(),
    );
    let headers =
        merge_string_maps(&items.iter().map(|item| item.and_then(|item| item.headers.as_ref())).collect::<Vec<_>>());
    let timeout =
        merge_timeout(&items.iter().map(|item| item.and_then(|item| item.timeout.as_ref())).collect::<Vec<_>>());
    let http = merge_http(&items.iter().map(|item| item.and_then(|item| item.http.as_ref())).collect::<Vec<_>>());
    let options = RequestOptions { generation, provider_options, headers, timeout, http };
    (options != RequestOptions::default()).then_some(options)
}

fn timeout_config(timeout: Option<&RequestTimeout>) -> TimeoutOptions {
    match timeout {
        None => TimeoutOptions::default(),
        Some(RequestTimeout::Timeout(value)) => {
            TimeoutOptions { total: Some(value.clone()), ..TimeoutOptions::default() }
        }
        Some(RequestTimeout::Config(options)) => options.clone(),
    }
}

pub fn merge_timeout(items: &[Option<&RequestTimeout>]) -> Option<RequestTimeout> {
    let configs: Vec<TimeoutOptions> = items.iter().map(|item| timeout_config(*item)).collect();
    let last =
        |select: fn(&TimeoutOptions) -> Option<&HttpTimeout>| configs.iter().filter_map(select).next_back().cloned();
    let options = TimeoutOptions {
        total: last(|item| item.total.as_ref()),
        headers: last(|item| item.headers.as_ref()),
        chunk: last(|item| item.chunk.as_ref()),
    };
    (options != TimeoutOptions::default()).then_some(RequestTimeout::Config(options))
}

pub fn merge_http(items: &[Option<&HttpOptions>]) -> Option<HttpOptions> {
    let query =
        merge_string_maps(&items.iter().map(|item| item.and_then(|item| item.query.as_ref())).collect::<Vec<_>>());
    let body = crate::json::merge_records(items.iter().filter_map(|item| item.and_then(|item| item.body.as_ref())));
    let encoding = items.iter().filter_map(|item| item.and_then(|item| item.encoding.clone())).next_back();
    let tls = items.iter().filter_map(|item| item.and_then(|item| item.tls.clone())).next_back();
    let options = HttpOptions { query, body, encoding, tls };
    (options != HttpOptions::default()).then_some(options)
}
