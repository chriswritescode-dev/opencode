use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use opencode_contract::generated::{
    FormAnswer, FormField, FormFields, FormID, FormInfo, FormMetadata, FormStringFieldFormat, FormValue, FormWhen,
    FormWhenOp, FormWhenValue,
};
use tokio::sync::oneshot;

const RETENTION: Duration = Duration::from_secs(600);

pub struct CreateInput {
    pub id: Option<FormID>,
    pub session_id: String,
    pub title: String,
    pub metadata: Option<FormMetadata>,
    pub fields: Vec<FormField>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FormState {
    Pending,
    Answered(FormAnswer),
    Cancelled(Option<String>),
}

pub trait FormEvents: Send + Sync {
    fn created(&self, form: &FormInfo);
    fn replied(&self, id: &FormID, session_id: &str, answer: &FormAnswer);
    fn cancelled(&self, id: &FormID, session_id: &str);
}

pub trait FormClock: Send + Sync {
    fn now(&self) -> Instant;
}

pub struct SystemClock;

impl FormClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum FormError {
    #[error("Form not found: {0}")]
    NotFound(String),
    #[error("Form already settled: {0}")]
    AlreadySettled(String),
    #[error("Form already exists: {0}")]
    AlreadyExists(String),
    #[error("{message}")]
    InvalidAnswer { id: String, message: String },
    #[error("{message}")]
    InvalidForm { message: String },
}

struct Entry {
    form: FormInfo,
    state: FormState,
    settled_at: Option<Instant>,
    waiter: Option<oneshot::Sender<FormState>>,
}

struct Inner {
    entries: Mutex<IndexMap<String, Entry>>,
    events: Arc<dyn FormEvents>,
    clock: Arc<dyn FormClock>,
    closed: AtomicBool,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, IndexMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn prune(&self) {
        let now = self.clock.now();
        let mut entries = self.lock();
        entries.retain(|_, entry| match entry.settled_at {
            Some(settled_at) => now.saturating_duration_since(settled_at) < RETENTION,
            None => true,
        });
    }
}

struct AskGuard {
    inner: Arc<Inner>,
    id: FormID,
    armed: bool,
}

impl AskGuard {
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for AskGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = cancel_entry(&self.inner, &self.id, None);
    }
}

#[derive(Clone)]
pub struct FormService {
    inner: Arc<Inner>,
}

impl FormService {
    pub fn new(events: Arc<dyn FormEvents>, clock: Arc<dyn FormClock>) -> Self {
        Self {
            inner: Arc::new(Inner {
                entries: Mutex::new(IndexMap::new()),
                events,
                clock,
                closed: AtomicBool::new(false),
            }),
        }
    }

    pub fn create(&self, input: CreateInput) -> Result<FormInfo, FormError> {
        let (form, receiver) = self.insert(input)?;
        drop(receiver);
        Ok(form)
    }

    pub async fn ask(&self, input: CreateInput) -> Result<FormState, FormError> {
        let (form, receiver) = self.insert(input)?;
        let guard = AskGuard { inner: Arc::clone(&self.inner), id: form.id, armed: true };
        let state = match receiver.await {
            Ok(state) => state,
            Err(_) => FormState::Cancelled(None),
        };
        guard.disarm();
        Ok(state)
    }

    pub fn get(&self, id: &FormID) -> Result<FormInfo, FormError> {
        self.inner.prune();
        let entries = self.inner.lock();
        match entries.get(id.0.as_str()) {
            Some(entry) => Ok(entry.form.clone()),
            None => Err(not_found(id)),
        }
    }

    pub fn list(&self, session_id: Option<&str>) -> Vec<FormInfo> {
        self.inner.prune();
        self.inner
            .lock()
            .values()
            .filter(|entry| entry.state == FormState::Pending)
            .filter(|entry| session_id.is_none_or(|session_id| entry.form.session_id == session_id))
            .map(|entry| entry.form.clone())
            .collect()
    }

    pub fn state(&self, id: &FormID) -> Result<FormState, FormError> {
        self.inner.prune();
        let entries = self.inner.lock();
        match entries.get(id.0.as_str()) {
            Some(entry) => Ok(entry.state.clone()),
            None => Err(not_found(id)),
        }
    }

    pub fn reply(&self, id: &FormID, answer: FormAnswer) -> Result<(), FormError> {
        self.inner.prune();
        let (session_id, state, waiter) = {
            let mut entries = self.inner.lock();
            let Some(entry) = entries.get_mut(id.0.as_str()) else {
                return Err(not_found(id));
            };
            if entry.state != FormState::Pending {
                return Err(FormError::AlreadySettled(id.0.clone()));
            }
            if let Some(message) = validate_answer(&entry.form.fields.0, &answer) {
                return Err(FormError::InvalidAnswer { id: id.0.clone(), message });
            }
            let state = FormState::Answered(answer.clone());
            entry.state = state.clone();
            entry.settled_at = Some(self.inner.clock.now());
            (entry.form.session_id.clone(), state, entry.waiter.take())
        };
        self.inner.events.replied(id, session_id.as_str(), &answer);
        if let Some(waiter) = waiter {
            let _ = waiter.send(state);
        }
        Ok(())
    }

    pub fn cancel(&self, id: &FormID, message: Option<String>) -> Result<(), FormError> {
        cancel_entry(&self.inner, id, message)
    }

    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.prune();
        let pending = {
            let entries = self.inner.lock();
            entries
                .iter()
                .filter(|(_, entry)| entry.state == FormState::Pending)
                .map(|(id, _)| FormID(id.clone()))
                .collect::<Vec<_>>()
        };
        for id in pending {
            let _ = cancel_entry(&self.inner, &id, None);
        }
    }

    fn insert(&self, input: CreateInput) -> Result<(FormInfo, oneshot::Receiver<FormState>), FormError> {
        self.inner.prune();
        let id = input.id.unwrap_or_else(|| FormID(format!("frm_{}", opencode_storage::id::ascending())));
        let (sender, receiver) = oneshot::channel();
        let form = {
            let mut entries = self.inner.lock();
            if entries.contains_key(id.0.as_str()) {
                return Err(FormError::AlreadyExists(id.0));
            }
            if let Some(message) = validate_fields(&input.fields) {
                return Err(FormError::InvalidForm { message });
            }
            let form = FormInfo {
                id: id.clone(),
                session_id: input.session_id,
                title: input.title,
                metadata: input.metadata,
                fields: FormFields(input.fields),
            };
            entries.insert(
                id.0.clone(),
                Entry { form: form.clone(), state: FormState::Pending, settled_at: None, waiter: Some(sender) },
            );
            form
        };
        self.inner.events.created(&form);
        if self.inner.closed.load(Ordering::SeqCst) {
            let _ = cancel_entry(&self.inner, &id, None);
        }
        Ok((form, receiver))
    }
}

fn cancel_entry(inner: &Inner, id: &FormID, message: Option<String>) -> Result<(), FormError> {
    inner.prune();
    let (session_id, state, waiter) = {
        let mut entries = inner.lock();
        let Some(entry) = entries.get_mut(id.0.as_str()) else {
            return Err(not_found(id));
        };
        if entry.state != FormState::Pending {
            return Err(FormError::AlreadySettled(id.0.clone()));
        }
        let state = FormState::Cancelled(message);
        entry.state = state.clone();
        entry.settled_at = Some(inner.clock.now());
        (entry.form.session_id.clone(), state, entry.waiter.take())
    };
    inner.events.cancelled(id, session_id.as_str());
    if let Some(waiter) = waiter {
        let _ = waiter.send(state);
    }
    Ok(())
}

fn not_found(id: &FormID) -> FormError {
    FormError::NotFound(id.0.clone())
}

pub fn validate_fields(fields: &[FormField]) -> Option<String> {
    if fields.is_empty() {
        return Some("Form must have at least one field".to_string());
    }
    let mut earlier: Vec<(&str, &FormField)> = Vec::new();
    let mut keys: Vec<&str> = Vec::new();
    for field in fields {
        let key = field_key(field);
        if keys.contains(&key) {
            return Some(format!("Duplicate form field key: {key}"));
        }
        keys.push(key);
        if matches!(field, FormField::External(_)) {
            continue;
        }
        if let Some(conditions) = field_when(field) {
            for condition in conditions {
                let Some(target) =
                    earlier.iter().find(|(earlier_key, _)| *earlier_key == condition.key).map(|(_, target)| *target)
                else {
                    return Some(format!(
                        "Form field condition must reference an earlier field: {key} -> {}",
                        condition.key
                    ));
                };
                if let Some(invalid) = validate_when(condition, target) {
                    return Some(format!("{invalid}: {key} -> {}", condition.key));
                }
            }
        }
        earlier.push((key, field));
    }
    None
}

fn validate_when(condition: &FormWhen, target: &FormField) -> Option<String> {
    match target {
        FormField::Boolean(_) => match condition.value {
            FormWhenValue::Bool(_) => None,
            _ => Some("Form field condition value must be a boolean".to_string()),
        },
        FormField::Number(_) | FormField::Integer(_) => match condition.value {
            FormWhenValue::Number(_) => None,
            _ => Some("Form field condition value must be a number".to_string()),
        },
        FormField::String(field) => {
            let FormWhenValue::String(value) = &condition.value else {
                return Some("Form field condition value must be a string".to_string());
            };
            let options = field.options.as_ref()?;
            if field.custom == Some(true) {
                return None;
            }
            if !options.iter().any(|option| option.value == *value) {
                return Some("Form field condition value must be one of the field's options".to_string());
            }
            None
        }
        FormField::Multiselect(field) => {
            let FormWhenValue::String(value) = &condition.value else {
                return Some("Form field condition value must be a string".to_string());
            };
            if field.custom == Some(true) {
                return None;
            }
            if !field.options.iter().any(|option| option.value == *value) {
                return Some("Form field condition value must be one of the field's options".to_string());
            }
            None
        }
        FormField::External(_) => None,
    }
}

pub fn validate_answer(fields: &[FormField], answer: &FormAnswer) -> Option<String> {
    for (key, _) in &answer.0 {
        if !fields.iter().any(|field| field_key(field) == key) {
            return Some(format!("Unknown form field: {key}"));
        }
    }
    for field in fields {
        let value = answer_value(answer, field_key(field));
        if matches!(field, FormField::External(_)) {
            if !matches!(value, Some(FormValue::Bool(true))) {
                return Some(format!("External form field must be acknowledged: {}", field_key(field)));
            }
            continue;
        }
        let active = is_active(field, answer);
        let Some(value) = value else {
            if field_required(field) == Some(true) && active {
                return Some(format!("Missing required form field: {}", field_key(field)));
            }
            continue;
        };
        if !active {
            return Some(format!("Form field is not active: {}", field_key(field)));
        }
        if let Some(invalid) = validate_field(field, value) {
            return Some(invalid);
        }
    }
    None
}

fn is_active(field: &FormField, answer: &FormAnswer) -> bool {
    let Some(conditions) = field_when(field) else {
        return true;
    };
    conditions.iter().all(|condition| matches_value(condition, answer_value(answer, condition.key.as_str())))
}

fn matches_value(condition: &FormWhen, value: Option<&FormValue>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let hit = match value {
        FormValue::Array(items) => items.iter().any(|item| match &condition.value {
            FormWhenValue::String(target) => item == target,
            _ => false,
        }),
        FormValue::String(item) => {
            matches!(&condition.value, FormWhenValue::String(target) if item == target)
        }
        FormValue::Number(item) => {
            matches!(&condition.value, FormWhenValue::Number(target) if item == target)
        }
        FormValue::Bool(item) => matches!(&condition.value, FormWhenValue::Bool(target) if item == target),
    };
    match condition.op {
        FormWhenOp::Eq => hit,
        FormWhenOp::Neq => !hit,
    }
}

fn validate_field(field: &FormField, value: &FormValue) -> Option<String> {
    let key = field_key(field);
    match field {
        FormField::String(field) => {
            let FormValue::String(value) = value else {
                return Some(format!("Expected string for form field: {key}"));
            };
            let length = value.encode_utf16().count();
            if field.required == Some(true) && length == 0 {
                return Some(format!("Missing required form field: {key}"));
            }
            if let Some(min_length) = field.min_length
                && (length as f64) < min_length
            {
                return Some(format!("Form field is too short: {key}"));
            }
            if let Some(max_length) = field.max_length
                && (length as f64) > max_length
            {
                return Some(format!("Form field is too long: {key}"));
            }
            if let Some(pattern) = &field.pattern {
                match regex_lite::Regex::new(pattern) {
                    Err(_) => return Some(format!("Form field has invalid pattern: {key}")),
                    Ok(regex) => {
                        if !regex.is_match(value) {
                            return Some(format!("Form field does not match pattern: {key}"));
                        }
                    }
                }
            }
            if field.format == Some(FormStringFieldFormat::Email) && !is_email(value) {
                return Some(format!("Expected email for form field: {key}"));
            }
            if field.format == Some(FormStringFieldFormat::Uri) && url::Url::parse(value).is_err() {
                return Some(format!("Expected URI for form field: {key}"));
            }
            if field.format == Some(FormStringFieldFormat::Date) && !is_date(value) {
                return Some(format!("Expected date for form field: {key}"));
            }
            if field.format == Some(FormStringFieldFormat::DateTime) && !is_date_time(value) {
                return Some(format!("Expected date-time for form field: {key}"));
            }
            if let Some(options) = &field.options
                && field.custom != Some(true)
                && !options.iter().any(|option| option.value == *value)
            {
                return Some(format!("Invalid option for form field: {key}"));
            }
            None
        }
        FormField::Number(field) => validate_number(key, value, field.minimum, field.maximum, false),
        FormField::Integer(field) => validate_number(key, value, field.minimum, field.maximum, true),
        FormField::Boolean(_) => match value {
            FormValue::Bool(_) => None,
            _ => Some(format!("Expected boolean for form field: {key}")),
        },
        FormField::Multiselect(field) => {
            let FormValue::Array(items) = value else {
                return Some(format!("Expected string array for form field: {key}"));
            };
            if field.required == Some(true) && items.is_empty() {
                return Some(format!("Missing required form field: {key}"));
            }
            if let Some(min_items) = field.min_items
                && (items.len() as f64) < min_items
            {
                return Some(format!("Too few selections for form field: {key}"));
            }
            if let Some(max_items) = field.max_items
                && (items.len() as f64) > max_items
            {
                return Some(format!("Too many selections for form field: {key}"));
            }
            if field.custom != Some(true)
                && items.iter().any(|item| !field.options.iter().any(|option| option.value == *item))
            {
                return Some(format!("Invalid option for form field: {key}"));
            }
            None
        }
        FormField::External(_) => None,
    }
}

fn validate_number(
    key: &str,
    value: &FormValue,
    minimum: Option<f64>,
    maximum: Option<f64>,
    integer: bool,
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

fn is_email(value: &str) -> bool {
    regex_lite::Regex::new(r"^[^\s@]+@[^\s@]+\.[^\s@]+$").is_ok_and(|regex| regex.is_match(value))
}

fn is_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let Some(year) = digits(bytes, 0, 4) else {
        return false;
    };
    let Some(month) = digits(bytes, 5, 2) else {
        return false;
    };
    let Some(day) = digits(bytes, 8, 2) else {
        return false;
    };
    if !(1..=12).contains(&month) || day == 0 {
        return false;
    }
    day <= days_in_month(year, month)
}

fn is_date_time(value: &str) -> bool {
    let bytes = value.as_bytes();
    let Some(date_len) = date_prefix_len(bytes) else {
        return false;
    };
    if date_len == bytes.len() {
        return true;
    }
    if date_len != 10 {
        return false;
    }
    if !matches!(bytes[date_len], b'T' | b't' | b' ') {
        return false;
    }
    time_suffix(&bytes[date_len + 1..])
}

fn date_prefix_len(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 4 {
        return None;
    }
    digits(bytes, 0, 4)?;
    if bytes.len() == 4 {
        return Some(4);
    }
    if bytes[4] != b'-' || bytes.len() < 7 {
        return None;
    }
    let month = digits(bytes, 5, 2)?;
    if !(1..=12).contains(&month) {
        return None;
    }
    if bytes.len() == 7 {
        return Some(7);
    }
    if bytes[7] != b'-' || bytes.len() < 10 {
        return None;
    }
    let day = digits(bytes, 8, 2)?;
    if !(1..=31).contains(&day) {
        return None;
    }
    Some(10)
}

fn time_suffix(bytes: &[u8]) -> bool {
    if bytes.len() < 5 || bytes[2] != b':' {
        return false;
    }
    let Some(hour) = digits(bytes, 0, 2) else {
        return false;
    };
    let Some(minute) = digits(bytes, 3, 2) else {
        return false;
    };
    if minute > 59 {
        return false;
    }
    let mut second = 0;
    let mut fraction_nonzero = false;
    let mut index = 5;
    if bytes.get(index) == Some(&b':') {
        let Some(value) = digits(bytes, index + 1, 2) else {
            return false;
        };
        if value > 59 {
            return false;
        }
        second = value;
        index += 3;
        if bytes.get(index) == Some(&b'.') {
            index += 1;
            let start = index;
            while let Some(byte) = bytes.get(index) {
                if !byte.is_ascii_digit() {
                    break;
                }
                if *byte != b'0' {
                    fraction_nonzero = true;
                }
                index += 1;
            }
            if index == start {
                return false;
            }
        }
    }
    if hour > 24 {
        return false;
    }
    if hour == 24 && (minute != 0 || second != 0 || fraction_nonzero) {
        return false;
    }
    match bytes.get(index) {
        None => true,
        Some(b'Z' | b'z') => index + 1 == bytes.len(),
        Some(b'+' | b'-') => offset(&bytes[index + 1..]),
        Some(_) => false,
    }
}

fn offset(bytes: &[u8]) -> bool {
    match bytes.len() {
        5 => {
            digits(bytes, 0, 2).is_some_and(|hour| hour <= 23)
                && bytes[2] == b':'
                && digits(bytes, 3, 2).is_some_and(|minute| minute <= 59)
        }
        4 => {
            digits(bytes, 0, 2).is_some_and(|hour| hour <= 23) && digits(bytes, 2, 2).is_some_and(|minute| minute <= 59)
        }
        _ => false,
    }
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => 29,
        2 => 28,
        _ => 0,
    }
}

fn digits(bytes: &[u8], start: usize, length: usize) -> Option<u32> {
    let slice = bytes.get(start..start + length)?;
    let mut value = 0;
    for byte in slice {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + u32::from(byte - b'0');
    }
    Some(value)
}

fn answer_value<'a>(answer: &'a FormAnswer, key: &str) -> Option<&'a FormValue> {
    answer.0.iter().find(|(entry_key, _)| entry_key.as_str() == key).map(|(_, value)| value)
}

fn field_key(field: &FormField) -> &str {
    match field {
        FormField::String(field) => &field.key,
        FormField::Number(field) => &field.key,
        FormField::Integer(field) => &field.key,
        FormField::Boolean(field) => &field.key,
        FormField::Multiselect(field) => &field.key,
        FormField::External(field) => &field.key,
    }
}

fn field_required(field: &FormField) -> Option<bool> {
    match field {
        FormField::String(field) => field.required,
        FormField::Number(field) => field.required,
        FormField::Integer(field) => field.required,
        FormField::Boolean(field) => field.required,
        FormField::Multiselect(field) => field.required,
        FormField::External(_) => None,
    }
}

fn field_when(field: &FormField) -> Option<&[FormWhen]> {
    match field {
        FormField::String(field) => field.when.as_deref(),
        FormField::Number(field) => field.when.as_deref(),
        FormField::Integer(field) => field.when.as_deref(),
        FormField::Boolean(field) => field.when.as_deref(),
        FormField::Multiselect(field) => field.when.as_deref(),
        FormField::External(_) => None,
    }
}
