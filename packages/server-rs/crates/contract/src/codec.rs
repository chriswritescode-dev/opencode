use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError {
    pub path: String,
    pub reason: String,
}

impl DecodeError {
    pub fn new(path: &Path, reason: impl Into<String>) -> Self {
        DecodeError { path: path.render(), reason: reason.into() }
    }
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.reason)
    }
}

impl std::error::Error for DecodeError {}

#[derive(Debug, Clone, Default)]
pub struct Path {
    segments: Vec<String>,
}

impl Path {
    pub fn new() -> Self {
        Path { segments: Vec::new() }
    }

    pub fn push_key(&mut self, key: &str) {
        self.segments.push(format!(".{key}"));
    }

    pub fn push_index(&mut self, index: usize) {
        self.segments.push(format!("[{index}]"));
    }

    pub fn pop(&mut self) {
        self.segments.pop();
    }

    pub fn mark(&self) -> usize {
        self.segments.len()
    }

    pub fn truncate(&mut self, mark: usize) {
        self.segments.truncate(mark);
    }

    pub fn at(&self, reason: impl Into<String>) -> DecodeError {
        DecodeError { path: self.render(), reason: reason.into() }
    }

    fn render(&self) -> String {
        let mut rendered = String::from("$");
        for segment in &self.segments {
            rendered.push_str(segment);
        }
        rendered
    }
}

pub trait Decode: Sized {
    fn decode(value: &Value, path: &mut Path) -> Result<Self, DecodeError>;
}

pub trait Encode {
    fn encode(&self, out: &mut String);
}

pub fn decode_json<T: Decode>(input: &str) -> Result<T, DecodeError> {
    let value = serde_json::from_str::<Value>(input).map_err(|error| Path::new().at(error.to_string()))?;
    T::decode(&value, &mut Path::new())
}

pub fn encode_json<T: Encode + ?Sized>(value: &T) -> String {
    let mut out = String::new();
    value.encode(&mut out);
    out
}

pub fn round_trip<T: Decode + Encode>(input: &str) -> Result<String, DecodeError> {
    decode_json::<T>(input).map(|value| encode_json(&value))
}

pub fn durable_version(object: &Map<String, Value>, path: &mut Path) -> Result<u32, DecodeError> {
    path.push_key("durable");
    let result = decode_durable_version(object, path);
    path.pop();
    result
}

fn decode_durable_version(object: &Map<String, Value>, path: &mut Path) -> Result<u32, DecodeError> {
    let Some(durable) = object.get("durable") else {
        return Err(path.at("missing required field"));
    };
    let envelope = as_object(durable, path)?;
    path.push_key("version");
    let result = match envelope.get("version").and_then(Value::as_f64) {
        Some(value) if value.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&value) => Ok(value as u32),
        _ => Err(path.at("expected a durable version")),
    };
    path.pop();
    result
}

pub fn as_object<'a>(value: &'a Value, path: &Path) -> Result<&'a Map<String, Value>, DecodeError> {
    value.as_object().ok_or_else(|| path.at("expected an object"))
}

pub fn reject_present(object: &Map<String, Value>, key: &str, path: &mut Path) -> Result<(), DecodeError> {
    if !object.contains_key(key) {
        return Ok(());
    }
    path.push_key(key);
    let error = path.at("expected an absent field");
    path.pop();
    Err(error)
}

pub fn decode_field<T: Decode>(object: &Map<String, Value>, key: &str, path: &mut Path) -> Result<T, DecodeError> {
    decode_present(object, key, path, |value, path| T::decode(value, path))
}

pub fn decode_field_checked<T: Decode, F>(
    object: &Map<String, Value>,
    key: &str,
    path: &mut Path,
    check: F,
) -> Result<T, DecodeError>
where
    F: FnOnce(&T, &Path) -> Result<(), DecodeError>,
{
    decode_present(object, key, path, |value, path| {
        let decoded = T::decode(value, path)?;
        check(&decoded, path)?;
        Ok(decoded)
    })
}

pub fn decode_optional<T: Decode>(
    object: &Map<String, Value>,
    key: &str,
    path: &mut Path,
) -> Result<Option<T>, DecodeError> {
    decode_optional_present(object, key, path, |value, path| T::decode(value, path))
}

pub fn decode_optional_checked<T: Decode, F>(
    object: &Map<String, Value>,
    key: &str,
    path: &mut Path,
    check: F,
) -> Result<Option<T>, DecodeError>
where
    F: FnOnce(&T, &Path) -> Result<(), DecodeError>,
{
    decode_optional_present(object, key, path, |value, path| {
        let decoded = T::decode(value, path)?;
        check(&decoded, path)?;
        Ok(decoded)
    })
}

pub fn decode_optional_nullable<T: Decode>(
    object: &Map<String, Value>,
    key: &str,
    path: &mut Path,
) -> Result<Option<Option<T>>, DecodeError> {
    decode_optional_nullable_checked(object, key, path, |_, _| Ok(()))
}

pub fn decode_optional_nullable_checked<T: Decode, F>(
    object: &Map<String, Value>,
    key: &str,
    path: &mut Path,
    check: F,
) -> Result<Option<Option<T>>, DecodeError>
where
    F: FnOnce(&T, &Path) -> Result<(), DecodeError>,
{
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(Some(None));
    }
    path.push_key(key);
    let decoded = T::decode(value, path).and_then(|decoded| {
        check(&decoded, path)?;
        Ok(Some(decoded))
    });
    path.pop();
    decoded.map(Some)
}

pub fn write_value<T: Encode + ?Sized>(out: &mut String, first: &mut bool, key: &str, value: &T) {
    if !*first {
        out.push(',');
    }
    *first = false;
    write_string(out, key);
    out.push(':');
    value.encode(out);
}

pub fn write_optional<T: Encode>(out: &mut String, first: &mut bool, key: &str, value: &Option<T>) {
    if let Some(value) = value {
        write_value(out, first, key, value);
    }
}

pub fn write_optional_nullable<T: Encode>(out: &mut String, first: &mut bool, key: &str, value: &Option<Option<T>>) {
    match value {
        None => {}
        Some(None) => write_value(out, first, key, &Value::Null),
        Some(Some(value)) => write_value(out, first, key, value),
    }
}

pub fn write_string(out: &mut String, value: &str) {
    match serde_json::to_string(value) {
        Ok(text) => out.push_str(&text),
        Err(_) => out.push_str("\"\""),
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct JsonMap<T>(pub Vec<(String, T)>);

fn decode_present<T>(
    object: &Map<String, Value>,
    key: &str,
    path: &mut Path,
    decode: impl FnOnce(&Value, &mut Path) -> Result<T, DecodeError>,
) -> Result<T, DecodeError> {
    let Some(value) = object.get(key) else {
        path.push_key(key);
        let error = path.at("missing required field");
        path.pop();
        return Err(error);
    };
    path.push_key(key);
    let decoded = decode(value, path);
    path.pop();
    decoded
}

fn decode_optional_present<T>(
    object: &Map<String, Value>,
    key: &str,
    path: &mut Path,
    decode: impl FnOnce(&Value, &mut Path) -> Result<T, DecodeError>,
) -> Result<Option<T>, DecodeError> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    path.push_key(key);
    let decoded = decode(value, path);
    path.pop();
    decoded.map(Some)
}

fn write_element<T: Encode + ?Sized>(out: &mut String, first: &mut bool, value: &T) {
    if !*first {
        out.push(',');
    }
    *first = false;
    value.encode(out);
}

fn index_key(key: &str) -> Option<u32> {
    let value = key.parse::<u32>().ok()?;
    (value != u32::MAX && value.to_string() == key).then_some(value)
}

fn write_object<'a, T: Encode + 'a>(out: &mut String, entries: impl Iterator<Item = (&'a str, &'a T)>) {
    let mut entries: Vec<(&str, &T)> = entries.collect();
    entries.sort_by_key(|(key, _)| index_key(key).map_or((1, 0), |value| (0, value)));
    out.push('{');
    let mut first = true;
    for (key, value) in entries {
        write_value(out, &mut first, key, value);
    }
    out.push('}');
}

pub fn write_struct_object<'a>(
    out: &mut String,
    declared: &'a [(&'a str, Option<&'a dyn Encode>)],
    rest: impl IntoIterator<Item = (&'a str, Option<&'a dyn Encode>)>,
) {
    let mut entries: Vec<(&str, Option<&dyn Encode>)> = declared.to_vec();
    entries.extend(rest);
    entries.sort_by_key(|(key, _)| index_key(key).map_or((1, 0), |value| (0, value)));
    out.push('{');
    let mut first = true;
    for (key, value) in entries {
        if let Some(value) = value {
            write_value(out, &mut first, key, value);
        }
    }
    out.push('}');
}

impl Decode for String {
    fn decode(value: &Value, path: &mut Path) -> Result<Self, DecodeError> {
        value.as_str().map(str::to_owned).ok_or_else(|| path.at("expected a string"))
    }
}

impl Encode for String {
    fn encode(&self, out: &mut String) {
        write_string(out, self);
    }
}

impl Decode for f64 {
    fn decode(value: &Value, path: &mut Path) -> Result<Self, DecodeError> {
        value.as_f64().ok_or_else(|| path.at("expected a number"))
    }
}

impl Encode for f64 {
    fn encode(&self, out: &mut String) {
        if !self.is_finite() {
            out.push_str("null");
            return;
        }
        out.push_str(&opencode_api_types::js::number_to_string(*self));
    }
}

impl Decode for bool {
    fn decode(value: &Value, path: &mut Path) -> Result<Self, DecodeError> {
        value.as_bool().ok_or_else(|| path.at("expected a boolean"))
    }
}

impl Encode for bool {
    fn encode(&self, out: &mut String) {
        out.push_str(if *self { "true" } else { "false" });
    }
}

impl Decode for Value {
    fn decode(value: &Value, _path: &mut Path) -> Result<Self, DecodeError> {
        Ok(value.clone())
    }
}

impl Encode for Value {
    fn encode(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Value::Number(value) => match value.as_f64() {
                Some(value) => out.push_str(&opencode_api_types::js::number_to_string(value)),
                None => out.push_str("null"),
            },
            Value::String(value) => write_string(out, value),
            Value::Array(items) => {
                out.push('[');
                let mut first = true;
                for item in items {
                    write_element(out, &mut first, item);
                }
                out.push(']');
            }
            Value::Object(entries) => write_object(out, entries.iter().map(|(key, value)| (key.as_str(), value))),
        }
    }
}

impl<T: Decode> Decode for Option<T> {
    fn decode(value: &Value, path: &mut Path) -> Result<Self, DecodeError> {
        if value.is_null() {
            return Ok(None);
        }
        T::decode(value, path).map(Some)
    }
}

impl<T: Encode> Encode for Option<T> {
    fn encode(&self, out: &mut String) {
        match self {
            None => out.push_str("null"),
            Some(value) => value.encode(out),
        }
    }
}

impl<T: Decode> Decode for Vec<T> {
    fn decode(value: &Value, path: &mut Path) -> Result<Self, DecodeError> {
        let Some(items) = value.as_array() else {
            return Err(path.at("expected an array"));
        };
        let mut decoded = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            path.push_index(index);
            let item = T::decode(item, path);
            path.pop();
            decoded.push(item?);
        }
        Ok(decoded)
    }
}

impl<T: Encode> Encode for Vec<T> {
    fn encode(&self, out: &mut String) {
        out.push('[');
        let mut first = true;
        for item in self {
            write_element(out, &mut first, item);
        }
        out.push(']');
    }
}

impl<T: Decode> Decode for JsonMap<T> {
    fn decode(value: &Value, path: &mut Path) -> Result<Self, DecodeError> {
        let object = as_object(value, path)?;
        let mut entries = Vec::with_capacity(object.len());
        for (key, value) in object {
            path.push_key(key);
            let decoded = T::decode(value, path);
            path.pop();
            entries.push((key.clone(), decoded?));
        }
        Ok(JsonMap(entries))
    }
}

impl<T: Encode> Encode for JsonMap<T> {
    fn encode(&self, out: &mut String) {
        write_object(out, self.0.iter().map(|(key, value)| (key.as_str(), value)));
    }
}

macro_rules! impl_tuple {
    ($count:expr; $(($index:tt, $name:ident)),+) => {
        impl<$($name: Decode),+> Decode for ($($name,)+) {
            fn decode(value: &Value, path: &mut Path) -> Result<Self, DecodeError> {
                let Some(items) = value.as_array() else {
                    return Err(path.at("expected an array"));
                };
                if items.len() != $count {
                    return Err(path.at(format!("expected an array of {} items", $count)));
                }
                Ok(($({
                    path.push_index($index);
                    let decoded = $name::decode(&items[$index], path);
                    path.pop();
                    decoded?
                },)+))
            }
        }

        impl<$($name: Encode),+> Encode for ($($name,)+) {
            fn encode(&self, out: &mut String) {
                out.push('[');
                let mut first = true;
                $(
                    write_element(out, &mut first, &self.$index);
                )+
                out.push(']');
            }
        }
    };
}

impl_tuple!(1; (0, A));
impl_tuple!(2; (0, A), (1, B));
impl_tuple!(3; (0, A), (1, B), (2, C));
impl_tuple!(4; (0, A), (1, B), (2, C), (3, D));
impl_tuple!(5; (0, A), (1, B), (2, C), (3, D), (4, E));
impl_tuple!(6; (0, A), (1, B), (2, C), (3, D), (4, E), (5, F));
impl_tuple!(7; (0, A), (1, B), (2, C), (3, D), (4, E), (5, F), (6, G));
impl_tuple!(8; (0, A), (1, B), (2, C), (3, D), (4, E), (5, F), (6, G), (7, H));
