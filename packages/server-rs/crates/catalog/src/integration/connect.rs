use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialExternal, CredentialExternalType, CredentialKey, CredentialKeyType, CredentialValue, FormAnswer,
    FormField1, FormFields1, FormIntegerField1Maximum, FormIntegerField1Minimum, FormNumberField1Maximum,
    FormNumberField1Minimum, FormStringField1Format, FormValue, FormWhen1, FormWhen1Op, FormWhen1Value, IntegrationID,
    IntegrationMethod, IntegrationMethodID,
};
use regex_lite::Regex;
use serde_json::Value;
use url::Url;

use crate::credential::CreateInput;
use crate::integration::{AuthorizationError, ConnectionService, IntegrationService};

static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[^\s@]+@[^\s@]+\.[^\s@]+$").expect("the email pattern is valid"));

pub struct KeyInput {
    pub integration_id: IntegrationID,
    pub key: String,
    pub answer: Option<FormAnswer>,
    pub label: Option<String>,
}

pub struct ExternalInput {
    pub integration_id: IntegrationID,
    pub method_id: IntegrationMethodID,
    pub answer: Option<FormAnswer>,
    pub label: Option<String>,
}

impl ConnectionService<'_> {
    pub async fn key(&self, input: KeyInput) -> Result<(), AuthorizationError> {
        let registry = self.service.state.get().await.map_err(AuthorizationError::new)?;
        let method = registry.entries.get(&input.integration_id.0).and_then(|entry| {
            entry.info.methods.iter().find_map(|method| match method {
                IntegrationMethod::Key(method) => Some(method),
                _ => None,
            })
        });
        let Some(method) = method else {
            return Err(AuthorizationError::new(std::io::Error::other(format!(
                "Key method not found: {}",
                input.integration_id.0
            ))));
        };
        let answer = input.answer.unwrap_or(JsonMap(Vec::new()));
        if let Some(invalid) = answer_error(method.form.as_ref(), &answer, Some("Key")) {
            return Err(AuthorizationError::new(std::io::Error::other(invalid)));
        }
        let value = CredentialValue::Key(CredentialKey {
            r#type: CredentialKeyType::Key,
            key: input.key,
            metadata: metadata(&answer),
        });
        create(self.service, &input.integration_id, input.label, value).await
    }

    pub async fn external(&self, input: ExternalInput) -> Result<(), AuthorizationError> {
        let registry = self.service.state.get().await.map_err(AuthorizationError::new)?;
        let method = registry.entries.get(&input.integration_id.0).and_then(|entry| {
            entry.info.methods.iter().find_map(|method| match method {
                IntegrationMethod::External(method) if method.id == input.method_id => Some(method),
                _ => None,
            })
        });
        let Some(method) = method else {
            return Err(AuthorizationError::new(std::io::Error::other(format!(
                "External method not found: {}",
                input.method_id.0
            ))));
        };
        let answer = input.answer.unwrap_or(JsonMap(Vec::new()));
        if let Some(invalid) = answer_error(method.form.as_ref(), &answer, Some("External")) {
            return Err(AuthorizationError::new(std::io::Error::other(invalid)));
        }
        let value = CredentialValue::External(CredentialExternal {
            r#type: CredentialExternalType::External,
            method_id: method.id.clone(),
            metadata: metadata(&answer),
        });
        create(self.service, &input.integration_id, input.label, value).await
    }
}

pub(crate) async fn create_credential(
    service: &std::sync::Arc<IntegrationService>,
    integration_id: &IntegrationID,
    label: Option<String>,
    value: CredentialValue,
) -> Result<(), AuthorizationError> {
    create(service.as_ref(), integration_id, label, value).await
}

async fn create(
    service: &IntegrationService,
    integration_id: &IntegrationID,
    label: Option<String>,
    value: CredentialValue,
) -> Result<(), AuthorizationError> {
    let label = match label {
        Some(label) => label,
        None => default_label(service, integration_id).await?,
    };
    service
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id.clone(),
            value,
            label: Some(label),
            activate: None,
        })
        .map_err(AuthorizationError::new)?;
    Ok(())
}

async fn default_label(
    service: &IntegrationService,
    integration_id: &IntegrationID,
) -> Result<String, AuthorizationError> {
    let registry = service.state.get().await.map_err(AuthorizationError::new)?;
    let name = registry
        .entries
        .get(&integration_id.0)
        .map(|entry| entry.info.name.clone())
        .unwrap_or_else(|| integration_id.0.clone());
    let labels: HashSet<String> = service
        .credentials
        .list(integration_id)
        .map_err(AuthorizationError::new)?
        .into_iter()
        .map(|credential| credential.label)
        .collect();
    Ok((0..=labels.len())
        .map(|index| match index {
            0 => name.clone(),
            index => format!("{name} {}", index + 1),
        })
        .find(|candidate| !labels.contains(candidate))
        .unwrap_or(name))
}

fn metadata(answer: &FormAnswer) -> Option<JsonMap<Value>> {
    if answer.0.is_empty() {
        return None;
    }
    Some(JsonMap(answer.0.iter().map(|(key, value)| (key.clone(), json(value))).collect()))
}

fn json(value: &FormValue) -> Value {
    match value {
        FormValue::String(value) => Value::String(value.clone()),
        FormValue::Number(value) => number(*value),
        FormValue::Bool(value) => Value::Bool(*value),
        FormValue::Array(values) => Value::Array(values.iter().cloned().map(Value::String).collect()),
    }
}

fn number(value: f64) -> Value {
    if value.is_finite() && value.fract() == 0.0 && (i64::MIN as f64..=i64::MAX as f64).contains(&value) {
        return Value::Number(serde_json::Number::from(value as i64));
    }
    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
}

pub(crate) fn answer_error(form: Option<&FormFields1>, answer: &FormAnswer, method: Option<&str>) -> Option<String> {
    if let Some(form) = form {
        return validate_fields(&form.0).or_else(|| validate_answer(&form.0, answer));
    }
    let method = method?;
    if answer.0.is_empty() {
        return None;
    }
    Some(format!("{method} method does not accept a form answer"))
}

fn validate_fields(fields: &[FormField1]) -> Option<String> {
    if fields.is_empty() {
        return Some("Form must have at least one field".to_owned());
    }
    let mut keys = HashSet::new();
    let mut earlier: HashMap<&str, &FormField1> = HashMap::new();
    for field in fields {
        let key = field_key(field);
        if !keys.insert(key) {
            return Some(format!("Duplicate form field key: {key}"));
        }
        if matches!(field, FormField1::External(_)) {
            continue;
        }
        for when in field_when(field).unwrap_or_default() {
            let Some(target) = earlier.get(when.key.as_str()) else {
                return Some(format!("Form field condition must reference an earlier field: {key} -> {}", when.key));
            };
            if let Some(invalid) = validate_when(when, target) {
                return Some(format!("{invalid}: {key} -> {}", when.key));
            }
        }
        earlier.insert(key, field);
    }
    None
}

fn validate_when(when: &FormWhen1, target: &FormField1) -> Option<String> {
    match target {
        FormField1::Boolean(_) => {
            if !matches!(when.value, FormWhen1Value::Bool(_)) {
                return Some("Form field condition value must be a boolean".to_owned());
            }
            return None;
        }
        FormField1::Number(_) | FormField1::Integer(_) => {
            if when_number(&when.value).is_none() {
                return Some("Form field condition value must be a number".to_owned());
            }
            return None;
        }
        _ => {}
    }
    let Some(value) = when_string(&when.value) else {
        return Some("Form field condition value must be a string".to_owned());
    };
    let (closed, options) = match target {
        FormField1::String(field) => {
            (field.options.is_some() && !field.custom.unwrap_or(false), field.options.as_deref().unwrap_or_default())
        }
        FormField1::Multiselect(field) => (!field.custom.unwrap_or(false), field.options.as_slice()),
        _ => (false, &[][..]),
    };
    if closed && !options.iter().any(|option| option.value == value) {
        return Some("Form field condition value must be one of the field's options".to_owned());
    }
    None
}

fn when_string(value: &FormWhen1Value) -> Option<&str> {
    match value {
        FormWhen1Value::String(value) => Some(value),
        FormWhen1Value::String2(_) => Some("Infinity"),
        FormWhen1Value::String3(_) => Some("-Infinity"),
        FormWhen1Value::String4(_) => Some("NaN"),
        _ => None,
    }
}

fn when_number(value: &FormWhen1Value) -> Option<f64> {
    match value {
        FormWhen1Value::Number(value) => Some(value.0),
        FormWhen1Value::String2(_) => Some(f64::INFINITY),
        FormWhen1Value::String3(_) => Some(f64::NEG_INFINITY),
        FormWhen1Value::String4(_) => Some(f64::NAN),
        _ => None,
    }
}

fn validate_answer(fields: &[FormField1], answer: &FormAnswer) -> Option<String> {
    let known: HashSet<&str> = fields.iter().map(field_key).collect();
    for (key, _) in &answer.0 {
        if !known.contains(key.as_str()) {
            return Some(format!("Unknown form field: {key}"));
        }
    }
    for field in fields {
        let key = field_key(field);
        let value = answer_value(answer, key);
        if matches!(field, FormField1::External(_)) {
            if !matches!(value, Some(FormValue::Bool(true))) {
                return Some(format!("External form field must be acknowledged: {key}"));
            }
            continue;
        }
        let active = is_active(field, answer);
        let Some(value) = value else {
            if field_required(field) && active {
                return Some(format!("Missing required form field: {key}"));
            }
            continue;
        };
        if !active {
            return Some(format!("Form field is not active: {key}"));
        }
        if let Some(invalid) = validate_field(field, value) {
            return Some(invalid);
        }
    }
    None
}

fn validate_field(field: &FormField1, value: &FormValue) -> Option<String> {
    match field {
        FormField1::String(field) => validate_string(&field.key, field, value),
        FormField1::Number(field) => validate_number(
            &field.key,
            value,
            false,
            number_minimum(field.minimum.as_ref()),
            number_maximum(field.maximum.as_ref()),
        ),
        FormField1::Integer(field) => validate_number(
            &field.key,
            value,
            true,
            integer_minimum(field.minimum.as_ref()),
            integer_maximum(field.maximum.as_ref()),
        ),
        FormField1::Boolean(field) => {
            if !matches!(value, FormValue::Bool(_)) {
                return Some(format!("Expected boolean for form field: {}", field.key));
            }
            None
        }
        FormField1::Multiselect(field) => {
            let FormValue::Array(values) = value else {
                return Some(format!("Expected string array for form field: {}", field.key));
            };
            if field.required.unwrap_or(false) && values.is_empty() {
                return Some(format!("Missing required form field: {}", field.key));
            }
            if let Some(minimum) = field.min_items
                && (values.len() as f64) < minimum
            {
                return Some(format!("Too few selections for form field: {}", field.key));
            }
            if let Some(maximum) = field.max_items
                && (values.len() as f64) > maximum
            {
                return Some(format!("Too many selections for form field: {}", field.key));
            }
            if !field.custom.unwrap_or(false)
                && values.iter().any(|value| !field.options.iter().any(|option| option.value == *value))
            {
                return Some(format!("Invalid option for form field: {}", field.key));
            }
            None
        }
        FormField1::External(_) => None,
    }
}

fn validate_string(
    key: &str,
    field: &opencode_contract::generated::FormStringField1,
    value: &FormValue,
) -> Option<String> {
    let FormValue::String(value) = value else {
        return Some(format!("Expected string for form field: {key}"));
    };
    if field.required.unwrap_or(false) && value.is_empty() {
        return Some(format!("Missing required form field: {key}"));
    }
    let length = value.encode_utf16().count() as f64;
    if let Some(minimum) = field.min_length
        && length < minimum
    {
        return Some(format!("Form field is too short: {key}"));
    }
    if let Some(maximum) = field.max_length
        && length > maximum
    {
        return Some(format!("Form field is too long: {key}"));
    }
    if let Some(pattern) = &field.pattern {
        let Ok(pattern) = Regex::new(pattern) else {
            return Some(format!("Form field has invalid pattern: {key}"));
        };
        if !pattern.is_match(value) {
            return Some(format!("Form field does not match pattern: {key}"));
        }
    }
    if let Some(format) = field.format {
        match format {
            FormStringField1Format::Email => {
                if !EMAIL.is_match(value) {
                    return Some(format!("Expected email for form field: {key}"));
                }
            }
            FormStringField1Format::Uri => {
                if Url::parse(value).is_err() {
                    return Some(format!("Expected URI for form field: {key}"));
                }
            }
            FormStringField1Format::Date => {
                if !is_date(value) {
                    return Some(format!("Expected date for form field: {key}"));
                }
            }
            FormStringField1Format::DateTime => {
                if opencode_util::time::parse_rfc3339(value).is_none() {
                    return Some(format!("Expected date-time for form field: {key}"));
                }
            }
        }
    }
    if let Some(options) = &field.options
        && !field.custom.unwrap_or(false)
        && !options.iter().any(|option| option.value == *value)
    {
        return Some(format!("Invalid option for form field: {key}"));
    }
    None
}

fn validate_number(
    key: &str,
    value: &FormValue,
    integer: bool,
    minimum: Option<f64>,
    maximum: Option<f64>,
) -> Option<String> {
    let FormValue::Number(value) = value else {
        return Some(format!("Expected number for form field: {key}"));
    };
    if !value.is_finite() {
        return Some(format!("Expected number for form field: {key}"));
    }
    if integer && value.fract() != 0.0 {
        return Some(format!("Expected integer for form field: {key}"));
    }
    if let Some(minimum) = minimum
        && *value < minimum
    {
        return Some(format!("Form field is too small: {key}"));
    }
    if let Some(maximum) = maximum
        && *value > maximum
    {
        return Some(format!("Form field is too large: {key}"));
    }
    None
}

fn number_minimum(value: Option<&FormNumberField1Minimum>) -> Option<f64> {
    match value? {
        FormNumberField1Minimum::Number(value) => Some(value.0),
        FormNumberField1Minimum::String(_) => Some(f64::INFINITY),
        FormNumberField1Minimum::String2(_) => Some(f64::NEG_INFINITY),
        FormNumberField1Minimum::String3(_) => Some(f64::NAN),
    }
}

fn number_maximum(value: Option<&FormNumberField1Maximum>) -> Option<f64> {
    match value? {
        FormNumberField1Maximum::Number(value) => Some(value.0),
        FormNumberField1Maximum::String(_) => Some(f64::INFINITY),
        FormNumberField1Maximum::String2(_) => Some(f64::NEG_INFINITY),
        FormNumberField1Maximum::String3(_) => Some(f64::NAN),
    }
}

fn integer_minimum(value: Option<&FormIntegerField1Minimum>) -> Option<f64> {
    match value? {
        FormIntegerField1Minimum::Number(value) => Some(value.0),
        FormIntegerField1Minimum::String(_) => Some(f64::INFINITY),
        FormIntegerField1Minimum::String2(_) => Some(f64::NEG_INFINITY),
        FormIntegerField1Minimum::String3(_) => Some(f64::NAN),
    }
}

fn integer_maximum(value: Option<&FormIntegerField1Maximum>) -> Option<f64> {
    match value? {
        FormIntegerField1Maximum::Number(value) => Some(value.0),
        FormIntegerField1Maximum::String(_) => Some(f64::INFINITY),
        FormIntegerField1Maximum::String2(_) => Some(f64::NEG_INFINITY),
        FormIntegerField1Maximum::String3(_) => Some(f64::NAN),
    }
}

fn is_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    if ![0, 1, 2, 3, 5, 6, 8, 9].iter().all(|index| bytes[*index].is_ascii_digit()) {
        return false;
    }
    let Ok(year) = value[0..4].parse::<i32>() else {
        return false;
    };
    let Ok(month) = value[5..7].parse::<u32>() else {
        return false;
    };
    let Ok(day) = value[8..10].parse::<u32>() else {
        return false;
    };
    opencode_util::time::Date::new(year, month, day).is_some()
}

fn is_active(field: &FormField1, answer: &FormAnswer) -> bool {
    let Some(when) = field_when(field) else {
        return true;
    };
    when.iter().all(|when| matches_when(when, answer_value(answer, &when.key)))
}

fn matches_when(when: &FormWhen1, value: Option<&FormValue>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let hit = match value {
        FormValue::String(value) => when_string(&when.value) == Some(value.as_str()),
        FormValue::Number(value) => when_number(&when.value).is_some_and(|when| when == *value),
        FormValue::Bool(value) => matches!(&when.value, FormWhen1Value::Bool(when) if when == value),
        FormValue::Array(values) => {
            when_string(&when.value).is_some_and(|when| values.iter().any(|value| value == when))
        }
    };
    match when.op {
        FormWhen1Op::Eq => hit,
        FormWhen1Op::Neq => !hit,
    }
}

fn answer_value<'a>(answer: &'a FormAnswer, key: &str) -> Option<&'a FormValue> {
    answer.0.iter().find(|(candidate, _)| candidate.as_str() == key).map(|(_, value)| value)
}

fn field_key(field: &FormField1) -> &str {
    match field {
        FormField1::String(field) => &field.key,
        FormField1::Number(field) => &field.key,
        FormField1::Integer(field) => &field.key,
        FormField1::Boolean(field) => &field.key,
        FormField1::Multiselect(field) => &field.key,
        FormField1::External(field) => &field.key,
    }
}

fn field_required(field: &FormField1) -> bool {
    match field {
        FormField1::String(field) => field.required.unwrap_or(false),
        FormField1::Number(field) => field.required.unwrap_or(false),
        FormField1::Integer(field) => field.required.unwrap_or(false),
        FormField1::Boolean(field) => field.required.unwrap_or(false),
        FormField1::Multiselect(field) => field.required.unwrap_or(false),
        FormField1::External(_) => false,
    }
}

fn field_when(field: &FormField1) -> Option<&[FormWhen1]> {
    match field {
        FormField1::String(field) => field.when.as_deref(),
        FormField1::Number(field) => field.when.as_deref(),
        FormField1::Integer(field) => field.when.as_deref(),
        FormField1::Boolean(field) => field.when.as_deref(),
        FormField1::Multiselect(field) => field.when.as_deref(),
        FormField1::External(_) => None,
    }
}
