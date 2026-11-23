use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex_lite::Regex;
use serde_json::Value;

use crate::clock::Clock;
use crate::schema::{AiError, AiErrorReason, HttpContext, HttpRateLimitDetails, ProviderFailureClassification};

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

static CONTEXT_OVERFLOW_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    compile(&[
        r"(?i)prompt is too long",
        r"(?i)input is too long for (?:the )?(?:requested|selected) model",
        r"(?i)too large for the model'?s configured input limit",
        r"(?i)exceeded this model context window limit",
        r"(?i)exceeds the context window",
        r"(?i)exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))",
        r"(?i)input token count.*exceeds the maximum",
        r"(?i)number of input tokens exceeds maximum length",
        r"(?i)tokens in request more than max tokens allowed",
        r"(?i)maximum prompt length is \d+",
        r"(?i)reduce the length of the messages",
        r"(?i)requested input length \d+ exceeds maximum input length",
        r"(?i)maximum context length is \d+ tokens",
        r"(?i)exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?",
        r"(?i)input(?: \(\d+ tokens\))? is longer than the model'?s context length",
        r"(?i)exceeds the limit of \d+",
        r"(?i)exceeds the available context size",
        r"(?i)greater than the context length",
        r"(?i)`inputs` tokens \+ `max_new_tokens` must be <= \d+",
        r"(?i)context window exceeds limit",
        r"(?i)context window exceeded",
        r"(?i)exceeded model token limit",
        r"(?i)context[_ ]length[_ ]exceeded",
        r"(?i)context length is only \d+ tokens",
        r"(?i)input length.*exceeds.*context length",
        r"(?i)prompt (?:too long|exceeds max length|超长)",
        r"(?i)too large for model with \d+ maximum context length",
        r"(?i)prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?",
        r"(?i)model_context_window_exceeded",
        r"(?i)range of input length should be",
        r"(?i)too many tokens",
        r"(?i)token limit exceeded",
        r"(?i)request_too_large",
    ])
});

static PAYLOAD_PATTERNS: LazyLock<Vec<Regex>> =
    LazyLock::new(|| compile(&[r"(?i)request entity too large", r"(?i)payload too large", r"(?i)request too large"]));

static EXCLUSIONS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    compile(&[
        r"(?i)^(throttling error|service unavailable):",
        r"(?i)rate limit",
        r"(?i)too many requests",
        r"(?i)max[_ ]tokens must be less than",
        r"(?i)too many tokens(?: per day)?, please wait",
    ])
});

static NO_BODY: LazyLock<Regex> = LazyLock::new(|| compile_one(r"(?i)^4(?:00|13)\s*(status code)?\s*\(no body\)"));

static CONTENT_POLICY_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    compile_one(
        r"(?i)violating our usage policy|blocked by content filtering policy|content[-_\s]?policy|rejected (?:as a result of our|by the) safety system|detected potentially unsafe or sensitive content|rejected in accordance with our terms of use",
    )
});

static AUTH_TEXT: LazyLock<Regex> = LazyLock::new(|| compile_one(r"(?i)incorrect api key provided"));

static RATE_LIMIT_TEXT: LazyLock<Regex> =
    LazyLock::new(|| compile_one(r"(?i)rate increased too quickly|rate[-_\s]?limit|too[_\s]?many[_\s]?requests"));

static QUOTA_TEXT: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    compile(&[
        r"(?i)insufficient[-_\s]?(?:quota|balance)",
        r"(?i)budget exceeded",
        r"(?i)usage limit",
        r"(?i)package has expired",
        r"(?i)plan does not yet include",
    ])
});

static QUOTA_EXCEEDED: LazyLock<Regex> = LazyLock::new(|| compile_one(r"(?i)quota[-_\s]?exceeded"));
static LIMIT_EXHAUSTED: LazyLock<Regex> = LazyLock::new(|| compile_one(r"(?i)limit exhausted"));

static SERVER_ERROR_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    compile_one(
        r"(?i)\b(?:try again|(?:please |you can )?retry (?:the |this |your )?request|try (?:the |this |your )?request again|(?:currently |temporarily )?at capacity|overloaded|temporarily unavailable|service[-_\s]?unavailable|(?:server|internal)[-_\s]?error|server (?:is )?busy|provider returned (?:an )?error|resource[-_\s]?exhausted|upstream (?:connect|connection|request)|request buffer limit while retrying upstream)\b",
    )
});

static GATEWAY_CODE_LABEL: LazyLock<Regex> = LazyLock::new(|| compile_one(r"^[^:\n]+: \[([A-Za-z0-9_.-]+)\]"));

const QUOTA_CODES: &[&str] = &[
    "insufficient_quota",
    "usage_not_included",
    "billing_error",
    "gousagelimiterror",
    "budgetlimiterror",
    "freeusagelimiterror",
    "creditlimitexceeded",
];

const AUTH_CODES: &[&str] = &["authentication_error", "permission_error", "permission_denied", "api_key_invalid"];

const SERVER_CODES: &[&str] = &[
    "api_error",
    "internal_error",
    "internalserverexception",
    "modelstreamerrorexception",
    "overloaded_error",
    "server_error",
    "server_is_overloaded",
    "slow_down",
    "serviceunavailableexception",
];

const INVALID_REQUEST_CODES: &[&str] =
    &["model_not_found", "invalid_prompt", "invalid_request", "invalid_request_error", "validationexception"];

const CONTENT_POLICY_CODES: &[&str] = &[
    "content_filter",
    "content_moderation",
    "responsibleaipolicyviolation",
    "content_policy_violation",
    "image_content_policy_violation",
    "cyber_policy",
    "refusal",
    "datainspectionfailed",
    "moderation_blocked",
    "safety",
    "prohibited_content",
    "blocklist",
    "image_safety",
    "content_blocked",
];

#[derive(Debug, Clone, Default)]
pub struct HttpFailureInput {
    pub message: String,
    pub url: Option<String>,
    pub status: Option<u16>,
    pub data: Option<Value>,
    pub response_headers: BTreeMap<String, String>,
    pub response_body: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderFailure {
    pub message: String,
    pub status: Option<u16>,
    pub raw_body: Option<String>,
    pub data: Option<Value>,
    pub http: Option<HttpContext>,
    pub retry_after_ms: Option<u64>,
    pub rate_limit: Option<HttpRateLimitDetails>,
}

pub fn http_failure(input: HttpFailureInput, clock: &dyn Clock) -> AiError {
    let headers = normalize_headers(input.response_headers);
    let retry_after = retry_after_ms(&headers, clock.now_ms());
    let rate_limit = rate_limit_details(&headers, retry_after);
    let http = match (input.status, input.url) {
        (Some(status), Some(url)) => Some(HttpContext { url, status, headers: headers.clone() }),
        _ => None,
    };
    let reason = classify_provider_failure(ProviderFailure {
        message: input.message,
        status: input.status,
        raw_body: input.response_body,
        data: input.data,
        http,
        retry_after_ms: retry_after,
        rate_limit,
    });
    AiError { reason }
}

pub fn classify_provider_failure(input: ProviderFailure) -> AiErrorReason {
    let body = input.raw_body.clone().unwrap_or_default();
    let mut codes: Vec<String> = Vec::new();
    codes.extend(provider_codes(input.data.as_ref()));
    codes.extend(provider_codes(Some(&Value::String(body.clone()))));
    codes.extend(provider_codes(Some(&Value::String(input.message.clone()))));
    if let Some(label) = GATEWAY_CODE_LABEL.captures(&input.message).and_then(|captures| captures.get(1)) {
        codes.push(label.as_str().to_string());
    }
    if let Some(value) = input.http.as_ref().and_then(|http| http.headers.get("x-amzn-errortype")) {
        codes.extend(aws_error_type(Some(&Value::String(value.clone()))));
    }
    let codes: Vec<String> = codes.into_iter().map(|code| code.to_lowercase()).collect();
    let text = [input.message.as_str(), body.as_str()]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let client_scoped = input.status.is_none_or(|status| (400..500).contains(&status));

    if client_scoped
        && (codes.iter().any(|code| {
            code == "context_length_exceeded" || code == "model_context_window_exceeded" || code == "request_too_large"
        }) || (input.status != Some(429)
            && !codes.iter().any(|code| code == "throttlingexception")
            && is_context_overflow(&text)))
    {
        return AiErrorReason::InvalidRequest {
            message: input.message,
            body: input.raw_body,
            http: input.http,
            parameter: None,
            classification: Some(ProviderFailureClassification::ContextOverflow),
        };
    }
    if input.status == Some(413) || any(&PAYLOAD_PATTERNS, &text) {
        return AiErrorReason::InvalidRequest {
            message: input.message,
            body: input.raw_body,
            http: input.http,
            parameter: None,
            classification: Some(ProviderFailureClassification::PayloadTooLarge),
        };
    }
    if codes.iter().any(|code| CONTENT_POLICY_CODES.contains(&code.as_str()))
        || (client_scoped && CONTENT_POLICY_TEXT.is_match(&input.message))
    {
        return AiErrorReason::ContentPolicy { message: input.message, body: input.raw_body, http: input.http };
    }
    if input.status == Some(402)
        || codes.iter().any(|code| QUOTA_CODES.contains(&code.as_str()))
        || (input.status == Some(429) && quota_text(&text))
    {
        return AiErrorReason::QuotaExceeded {
            message: input.message,
            body: input.raw_body,
            http: input.http,
            retry_after_ms: input.retry_after_ms,
        };
    }
    if input.status == Some(401)
        || input.status == Some(403)
        || codes.iter().any(|code| AUTH_CODES.contains(&code.as_str()))
        || (input.status == Some(400) && AUTH_TEXT.is_match(&text))
    {
        return AiErrorReason::Authentication { message: input.message, body: input.raw_body, http: input.http };
    }
    if input.status == Some(429)
        || codes
            .iter()
            .any(|code| code.contains("rate_limit") || code == "too_many_requests" || code == "throttlingexception")
        || RATE_LIMIT_TEXT.is_match(&text)
    {
        return AiErrorReason::RateLimit {
            message: input.message,
            body: input.raw_body,
            http: input.http,
            retry_after_ms: input.retry_after_ms,
            rate_limit: input.rate_limit,
        };
    }
    if input.status == Some(408)
        || input.status == Some(409)
        || input.status.is_some_and(|status| status >= 500)
        || (input.status.is_none_or(|status| status < 400)
            && ((!codes.iter().any(|code| INVALID_REQUEST_CODES.contains(&code.as_str()))
                && SERVER_ERROR_TEXT.is_match(&text))
                || codes.iter().any(|code| {
                    SERVER_CODES.contains(&code.as_str()) || code.contains("exhausted") || code.contains("unavailable")
                })))
    {
        return AiErrorReason::ProviderInternal {
            message: input.message,
            body: input.raw_body,
            http: input.http,
            retry_after_ms: input.retry_after_ms,
        };
    }
    if codes.iter().any(|code| INVALID_REQUEST_CODES.contains(&code.as_str())) {
        return AiErrorReason::InvalidRequest {
            message: input.message,
            body: input.raw_body,
            http: input.http,
            parameter: None,
            classification: None,
        };
    }
    if input.status.is_some_and(|status| (400..500).contains(&status)) {
        return AiErrorReason::InvalidRequest {
            message: input.message,
            body: input.raw_body,
            http: input.http,
            parameter: None,
            classification: None,
        };
    }
    AiErrorReason::UnknownProvider { message: input.message, body: input.raw_body, http: input.http }
}

pub fn is_context_overflow(message: &str) -> bool {
    !any(&EXCLUSIONS, message) && (any(&CONTEXT_OVERFLOW_PATTERNS, message) || NO_BODY.is_match(message))
}

pub fn provider_error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let message = nested_text(&value, "error", "message")
        .or_else(|| field(&value, "error"))
        .or_else(|| field(&value, "message"))
        .or_else(|| field(&value, "Message"))
        .or_else(|| field(&value, "detail"))
        .or_else(|| {
            value
                .get("errors")
                .and_then(Value::as_array)
                .and_then(|errors| errors.first())
                .and_then(|error| field(error, "message"))
        })?;
    Some(message.to_string())
}

fn normalize_headers(headers: BTreeMap<String, String>) -> BTreeMap<String, String> {
    headers.into_iter().map(|(name, value)| (name.to_lowercase(), value)).collect()
}

fn retry_after_ms(headers: &BTreeMap<String, String>, now_ms: u64) -> Option<u64> {
    if let Some(value) = headers.get("retry-after-ms") {
        let millis = js_number(value);
        if millis.is_finite() {
            return Some(clamp_retry_after(millis));
        }
    }
    let value = headers.get("retry-after")?;
    let seconds = js_number(value);
    if seconds.is_finite() {
        return Some(clamp_retry_after(seconds * 1000.0));
    }
    let date = parse_http_date(value)?;
    Some(clamp_retry_after((date - now_ms as i128) as f64))
}

fn clamp_retry_after(millis: f64) -> u64 {
    millis.ceil().clamp(0.0, MAX_SAFE_INTEGER) as u64
}

fn js_number(value: &str) -> f64 {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return 0.0;
    }
    match trimmed {
        "Infinity" | "+Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ => trimmed.parse::<f64>().unwrap_or(f64::NAN),
    }
}

fn rate_limit_details(headers: &BTreeMap<String, String>, retry_after: Option<u64>) -> Option<HttpRateLimitDetails> {
    let mut limit = BTreeMap::new();
    let mut remaining = BTreeMap::new();
    let mut reset = BTreeMap::new();
    for (name, value) in headers {
        if let Some(key) = name.strip_prefix("x-ratelimit-limit-") {
            add_rate_limit_value(&mut limit, key, value);
            continue;
        }
        if let Some(key) = name.strip_prefix("x-ratelimit-remaining-") {
            add_rate_limit_value(&mut remaining, key, value);
            continue;
        }
        if let Some(key) = name.strip_prefix("x-ratelimit-reset-") {
            add_rate_limit_value(&mut reset, key, value);
            continue;
        }
        let Some(rest) = name.strip_prefix("anthropic-ratelimit-") else { continue };
        if let Some(key) = rest.strip_suffix("-limit") {
            add_rate_limit_value(&mut limit, key, value);
        } else if let Some(key) = rest.strip_suffix("-remaining") {
            add_rate_limit_value(&mut remaining, key, value);
        } else if let Some(key) = rest.strip_suffix("-reset") {
            add_rate_limit_value(&mut reset, key, value);
        }
    }
    if retry_after.is_none() && limit.is_empty() && remaining.is_empty() && reset.is_empty() {
        return None;
    }
    Some(HttpRateLimitDetails {
        retry_after_ms: retry_after,
        limit: (!limit.is_empty()).then_some(limit),
        remaining: (!remaining.is_empty()).then_some(remaining),
        reset: (!reset.is_empty()).then_some(reset),
    })
}

fn add_rate_limit_value(target: &mut BTreeMap<String, String>, key: &str, value: &str) {
    if !key.is_empty() {
        target.insert(key.to_string(), value.to_string());
    }
}

fn quota_text(text: &str) -> bool {
    if any(&QUOTA_TEXT, text) {
        return true;
    }
    if QUOTA_EXCEEDED.find_iter(text).any(|found| !ends_with_ignore_case(&text[..found.start()], "token_")) {
        return true;
    }
    LIMIT_EXHAUSTED.find_iter(text).any(|found| {
        let before = text[..found.start()].to_lowercase();
        !["rate", "rate-", "rate_", "rate "].iter().any(|prefix| before.ends_with(prefix))
    })
}

fn ends_with_ignore_case(value: &str, suffix: &str) -> bool {
    value.to_lowercase().ends_with(suffix)
}

fn provider_codes(value: Option<&Value>) -> Vec<String> {
    let Some(value) = value else { return Vec::new() };
    let decoded = match value {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(decoded) => decoded,
            Err(_) => return Vec::new(),
        },
        other => other.clone(),
    };
    let Value::Object(record) = decoded else { return Vec::new() };
    let error = record.get("error").and_then(Value::as_object);
    let inner = error.and_then(|error| error.get("innererror")).and_then(Value::as_object);
    let metadata = error.and_then(|error| error.get("metadata")).and_then(Value::as_object);
    let response = record.get("response").and_then(Value::as_object);
    let response_error = response.and_then(|response| response.get("error")).and_then(Value::as_object);
    let exception = record.get("exception").and_then(Value::as_object);
    let mut codes = Vec::new();
    push_string(&mut codes, record.get("code"));
    codes.extend(aws_error_type(record.get("__type")));
    codes.extend(aws_error_type(record.get("code")));
    push_string(&mut codes, record.get("error_type"));
    if let Some(error) = error {
        push_string(&mut codes, error.get("code"));
        push_string(&mut codes, error.get("type"));
        push_string(&mut codes, error.get("status"));
        push_string(&mut codes, error.get("error_type"));
        if let Some(details) = error.get("details").and_then(Value::as_array) {
            for detail in details {
                push_string(&mut codes, detail.get("reason"));
            }
        }
    }
    if let Some(inner) = inner {
        push_string(&mut codes, inner.get("code"));
    }
    if let Some(metadata) = metadata {
        push_string(&mut codes, metadata.get("error_type"));
    }
    if let Some(response_error) = response_error {
        push_string(&mut codes, response_error.get("code"));
    }
    if let Some(response) = response {
        push_string(&mut codes, response.get("error_type"));
    }
    if let Some(exception) = exception {
        push_string(&mut codes, exception.get("type"));
    }
    codes
}

fn aws_error_type(value: Option<&Value>) -> Vec<String> {
    let Some(Value::String(value)) = value else { return Vec::new() };
    let head = value.split(':').next().unwrap_or("");
    let name = head.split_once('#').map_or(head, |(_, rest)| rest);
    vec![name.to_string()]
}

fn push_string(codes: &mut Vec<String>, value: Option<&Value>) {
    if let Some(Value::String(value)) = value {
        codes.push(value.clone());
    }
}

fn nested_text<'a>(value: &'a Value, first: &str, second: &str) -> Option<&'a str> {
    value.get(first)?.get(second)?.as_str().and_then(non_blank)
}

fn field<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.get(name)?.as_str().and_then(non_blank)
}

fn non_blank(value: &str) -> Option<&str> {
    value.chars().any(|character| !character.is_whitespace()).then_some(value)
}

fn any(patterns: &[Regex], text: &str) -> bool {
    patterns.iter().any(|pattern| pattern.is_match(text))
}

fn compile(patterns: &[&str]) -> Vec<Regex> {
    patterns.iter().map(|pattern| compile_one(pattern)).collect()
}

fn compile_one(pattern: &str) -> Regex {
    Regex::new(pattern).expect("provider error pattern compiles")
}

fn parse_http_date(value: &str) -> Option<i128> {
    let value = value.trim();
    if let Some(millis) = parse_iso_date(value) {
        return Some(millis);
    }
    let rest = value.split_once(',').map_or(value, |(_, rest)| rest.trim());
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() == 5 && parts[4].eq_ignore_ascii_case("GMT") {
        let day = parts[0].parse::<u32>().ok()?;
        let month = month(parts[1])?;
        let year = parts[2].parse::<i32>().ok()?;
        let (hour, minute, second) = parse_time(parts[3])?;
        return civil_to_ms(year, month, day, hour, minute, second, 0);
    }
    if parts.len() == 4 && parts[3].eq_ignore_ascii_case("GMT") {
        let (day, month, year) = parse_rfc850_date(parts[0])?;
        let (hour, minute, second) = parse_time(parts[1])?;
        return civil_to_ms(year, month, day, hour, minute, second, 0);
    }
    if parts.len() == 5 {
        let month = month(parts[0])?;
        let day = parts[1].parse::<u32>().ok()?;
        let (hour, minute, second) = parse_time(parts[2])?;
        let year = parts[3].parse::<i32>().ok()?;
        return civil_to_ms(year, month, day, hour, minute, second, 0);
    }
    None
}

fn parse_rfc850_date(value: &str) -> Option<(u32, u32, i32)> {
    let mut parts = value.split('-');
    let day = parts.next()?.parse::<u32>().ok()?;
    let month = month(parts.next()?)?;
    let year = parts.next()?.parse::<i32>().ok()?;
    Some((day, month, if year < 100 { 1900 + year } else { year }))
}

fn parse_iso_date(value: &str) -> Option<i128> {
    let (date, time) = value.split_once(['T', ' '])?;
    let mut date_parts = date.split('-');
    let year = date_parts.next()?.parse::<i32>().ok()?;
    let month = date_parts.next()?.parse::<u32>().ok()?;
    let day = date_parts.next()?.parse::<u32>().ok()?;
    let (time, offset_minutes) = if let Some(time) = time.strip_suffix('Z') {
        (time, 0i64)
    } else if let Some(index) = time.rfind(['+', '-']) {
        let sign = if time.as_bytes()[index] == b'-' { -1 } else { 1 };
        let offset = &time[index + 1..];
        let mut offset_parts = offset.split(':');
        let hours = offset_parts.next()?.parse::<i64>().ok()?;
        let minutes = offset_parts.next().map_or(Some(0), |minutes| minutes.parse::<i64>().ok())?;
        (&time[..index], sign * (hours * 60 + minutes))
    } else {
        (time, 0)
    };
    let mut time_parts = time.split(':');
    let hour = time_parts.next()?.parse::<u32>().ok()?;
    let minute = time_parts.next()?.parse::<u32>().ok()?;
    let seconds = time_parts.next()?;
    let (second, millis) = match seconds.split_once('.') {
        Some((second, fraction)) => {
            let fraction: String = fraction.chars().take(3).collect();
            let millis = format!("{fraction:0<3}").parse::<u32>().ok()?;
            (second.parse::<u32>().ok()?, millis)
        }
        None => (seconds.parse::<u32>().ok()?, 0),
    };
    let millis = civil_to_ms(year, month, day, hour, minute, second, millis)?;
    Some(millis - i128::from(offset_minutes) * 60_000)
}

fn parse_time(value: &str) -> Option<(u32, u32, u32)> {
    let mut parts = value.split(':');
    let hour = parts.next()?.parse::<u32>().ok()?;
    let minute = parts.next()?.parse::<u32>().ok()?;
    let second = parts.next()?.parse::<u32>().ok()?;
    Some((hour, minute, second))
}

fn month(value: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let value = value.to_lowercase();
    MONTHS.iter().position(|month| value.starts_with(month)).map(|index| index as u32 + 1)
}

fn civil_to_ms(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32, millis: u32) -> Option<i128> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let year = i128::from(year);
    let month = i128::from(month);
    let day = i128::from(day);
    let days = if month <= 2 { year - 1 } else { year };
    let era = if days >= 0 { days } else { days - 399 } / 400;
    let year_of_era = days - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(
        days * 86_400_000
            + i128::from(hour) * 3_600_000
            + i128::from(minute) * 60_000
            + i128::from(second) * 1_000
            + i128::from(millis),
    )
}
