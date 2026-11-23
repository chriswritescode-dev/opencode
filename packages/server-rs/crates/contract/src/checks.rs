use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

static PATTERNS: LazyLock<Mutex<HashMap<&'static str, Option<regex_lite::Regex>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn is_int(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0
}

pub fn is_finite(value: f64) -> bool {
    value.is_finite()
}

pub fn is_greater_than(value: f64, minimum: f64) -> bool {
    value > minimum
}

pub fn is_greater_than_or_equal_to(value: f64, minimum: f64) -> bool {
    value >= minimum
}

pub fn is_less_than(value: f64, maximum: f64) -> bool {
    value < maximum
}

pub fn is_less_than_or_equal_to(value: f64, maximum: f64) -> bool {
    value <= maximum
}

pub fn is_between(value: f64, minimum: f64, maximum: f64) -> bool {
    value >= minimum && value <= maximum
}

pub fn is_min_length(value: &str, minimum: usize) -> bool {
    value.encode_utf16().count() >= minimum
}

pub fn is_max_length(value: &str, maximum: usize) -> bool {
    value.encode_utf16().count() <= maximum
}

pub fn is_min_items<T>(value: &[T], minimum: usize) -> bool {
    value.len() >= minimum
}

pub fn is_max_items<T>(value: &[T], maximum: usize) -> bool {
    value.len() <= maximum
}

pub fn is_pattern(value: &str, pattern: &'static str) -> bool {
    let mut patterns = PATTERNS.lock().unwrap_or_else(|error| error.into_inner());
    match patterns.entry(pattern).or_insert_with(|| regex_lite::Regex::new(pattern).ok()) {
        Some(compiled) => compiled.is_match(value),
        None => false,
    }
}

pub fn is_starting_with(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
}

pub fn is_ending_with(value: &str, suffix: &str) -> bool {
    value.ends_with(suffix)
}

pub fn is_including(value: &str, needle: &str) -> bool {
    value.contains(needle)
}
