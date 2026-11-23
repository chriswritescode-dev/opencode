use serde_json::{Map, Value};

use opencode_contract::checks;
use opencode_contract::generated::{ConfigEntryDocumentInfoMcpServersValue, ConfigEntryDocumentInfoProvidersValue};

use super::{
    Diagnostic, canonical, invalid, migrate_mcp_timeout, model_selection_from_string, object, provider_id, push_path,
    reorder_map, unsupported_if_present,
};

const ACTION_ONLY: [&str; 3] = ["question", "websearch", "doom_loop"];
const THEME_COLORS: [&str; 7] = ["primary", "secondary", "accent", "success", "warning", "error", "info"];
const UNSUPPORTED_PROVIDER: [&str; 3] = ["id", "whitelist", "blacklist"];
const UNSUPPORTED_MODEL: [&str; 5] = ["release_date", "attachment", "reasoning", "temperature", "experimental"];
const COMPATIBLE_KEYS: [&str; 13] = [
    "apiKey",
    "baseURL",
    "headers",
    "body",
    "extraBody",
    "timeout",
    "chunkTimeout",
    "headerTimeout",
    "disabled",
    "reasoningEffort",
    "textVerbosity",
    "user",
    "strictJsonSchema",
];
const MODEL_MODALITIES: [&str; 5] = ["text", "audio", "image", "video", "pdf"];
const MODEL_STATUSES: [&str; 4] = ["alpha", "beta", "deprecated", "active"];
const PROVIDER_OPTION_KEYS: [&str; 7] =
    ["apiKey", "baseURL", "enterpriseUrl", "setCacheKey", "timeout", "headerTimeout", "chunkTimeout"];

pub fn decode_agent(value: &Value) -> Option<Value> {
    let map = as_object(value)?;
    let mut decoded = Map::new();
    let mut extras = Map::new();
    let mut tools = None;
    let mut permission = None;
    for (key, item) in map {
        match key.as_str() {
            "model" | "variant" | "prompt" | "description" => {
                if !item.is_string() {
                    return None;
                }
                decoded.insert(key.clone(), item.clone());
            }
            "temperature" | "top_p" => {
                if !is_finite(item) {
                    return None;
                }
                decoded.insert(key.clone(), item.clone());
            }
            "disable" | "hidden" => {
                if !item.is_boolean() {
                    return None;
                }
                decoded.insert(key.clone(), item.clone());
            }
            "mode" => {
                if !matches!(item.as_str(), Some("subagent" | "primary" | "all")) {
                    return None;
                }
                decoded.insert(key.clone(), item.clone());
            }
            "tools" => tools = Some(boolean_entries(item)?),
            "options" => {
                as_object(item)?;
                decoded.insert(key.clone(), item.clone());
            }
            "color" => {
                if !is_agent_color(item) {
                    return None;
                }
                decoded.insert(key.clone(), item.clone());
            }
            "steps" | "maxSteps" => {
                if !is_positive_int(item) {
                    return None;
                }
                decoded.insert(key.clone(), item.clone());
            }
            "permission" => permission = Some(permission_entries(item)?),
            "name" => {}
            _ => {
                extras.insert(key.clone(), item.clone());
            }
        }
    }

    let mut options = match decoded.get("options") {
        Some(Value::Object(options)) => options.clone(),
        _ => Map::new(),
    };
    for (key, value) in extras {
        options.insert(key, value);
    }

    let mut normalized = Vec::new();
    for (tool, enabled) in tools.unwrap_or_default() {
        let key = if tool == "write" || tool == "patch" { "edit".to_owned() } else { tool };
        set_entry(&mut normalized, &key, Value::String(if enabled { "allow" } else { "deny" }.to_owned()));
    }
    for (key, rule) in permission.unwrap_or_default() {
        set_entry(&mut normalized, &key, rule);
    }

    let steps = decoded.get("steps").or_else(|| decoded.get("maxSteps")).cloned();
    let mut out = Map::new();
    for key in
        ["model", "variant", "temperature", "top_p", "prompt", "disable", "description", "mode", "hidden", "color"]
    {
        if let Some(value) = decoded.get(key) {
            out.insert(key.to_owned(), value.clone());
        }
    }
    out.insert("options".to_owned(), Value::Object(reorder_map(options)));
    out.insert("permission".to_owned(), entries(&normalized));
    if let Some(steps) = steps {
        out.insert("steps".to_owned(), steps);
    }
    Some(Value::Object(out))
}

pub fn migrate_agent(info: &Value) -> Value {
    let map = as_object(info).expect("decoded legacy agent");
    let mut body = Map::new();
    if let Some(Value::Object(options)) = map.get("options") {
        for (key, value) in options {
            body.insert(key.clone(), value.clone());
        }
    }
    for key in ["temperature", "top_p"] {
        if let Some(value) = map.get(key) {
            body.insert(key.to_owned(), value.clone());
        }
    }

    let mut out = Map::new();
    if let Some(model) =
        model_selection(map.get("model").and_then(Value::as_str), map.get("variant").and_then(Value::as_str))
    {
        out.insert("model".to_owned(), model);
    }
    if !body.is_empty() {
        out.insert("request".to_owned(), object(vec![("body".to_owned(), Value::Object(reorder_map(body)))]));
    }
    for (key, target) in [("prompt", "system"), ("description", "description"), ("mode", "mode"), ("hidden", "hidden")]
    {
        if let Some(value) = map.get(key) {
            out.insert(target.to_owned(), value.clone());
        }
    }
    if let Some(Value::String(color)) = map.get("color") {
        out.insert(
            "color".to_owned(),
            Value::String(if color.starts_with('#') { color.clone() } else { "#aaaaaa".to_owned() }),
        );
    }
    if let Some(steps) = map.get("steps") {
        out.insert("steps".to_owned(), steps.clone());
    }
    if let Some(disable) = map.get("disable") {
        out.insert("disabled".to_owned(), disable.clone());
    }
    if let Some(Value::Array(permission)) = map.get("permission") {
        let rules = permission_rule_values(permission);
        if !rules.is_empty() {
            out.insert("permissions".to_owned(), Value::Array(rules));
        }
    }
    Value::Object(out)
}

pub fn decode_command(value: &Value) -> Option<Value> {
    let map = as_object(value)?;
    let mut out = Map::new();
    for (key, item) in map {
        match key.as_str() {
            "template" => {
                if !item.is_string() {
                    return None;
                }
                out.insert(key.clone(), item.clone());
            }
            "description" | "agent" | "model" | "variant" => {
                if !item.is_string() {
                    return None;
                }
                out.insert(key.clone(), item.clone());
            }
            "subtask" => {
                if !item.is_boolean() {
                    return None;
                }
                out.insert(key.clone(), item.clone());
            }
            _ => {}
        }
    }
    if !out.contains_key("template") {
        return None;
    }
    Some(Value::Object(out))
}

pub fn migrate_command(command: &Value) -> Value {
    let map = as_object(command).expect("decoded legacy command");
    let mut out = Map::new();
    if let Some(template) = map.get("template") {
        out.insert("template".to_owned(), template.clone());
    }
    for key in ["description", "agent"] {
        if let Some(value) = map.get(key) {
            out.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(model) =
        model_selection(map.get("model").and_then(Value::as_str), map.get("variant").and_then(Value::as_str))
    {
        out.insert("model".to_owned(), model);
    }
    if let Some(subtask) = map.get("subtask") {
        out.insert("subagent".to_owned(), subtask.clone());
    }
    Value::Object(out)
}

pub fn model_selection(input: Option<&str>, variant: Option<&str>) -> Option<Value> {
    let Value::Object(mut selection) = model_selection_from_string(input?)? else { return None };
    let selected =
        selection.get("variant").and_then(Value::as_str).map(str::to_owned).or_else(|| variant.map(str::to_owned));
    let provider = selection.get("providerID").and_then(Value::as_str)?.to_owned();
    selection.insert("providerID".to_owned(), Value::String(provider_id(&provider).to_owned()));
    selection.shift_remove("variant");
    if let Some(selected) = selected
        && !selected.is_empty()
        && !selected.contains('#')
    {
        selection.insert("variant".to_owned(), Value::String(selected));
    }
    Some(Value::Object(selection))
}

pub fn migrate_mcp(info: &Value) -> Option<Value> {
    let map = as_object(info)?;
    let disabled = map.get("enabled").map(|enabled| !enabled.as_bool().unwrap_or(false));
    let timeout = map.get("timeout").map(migrate_mcp_timeout);
    let mut out = Map::new();
    if map.get("type").and_then(Value::as_str) == Some("local") {
        out.insert("type".to_owned(), Value::String("local".to_owned()));
        out.insert("command".to_owned(), map.get("command")?.clone());
        for key in ["cwd", "environment"] {
            if let Some(value) = map.get(key) {
                out.insert(key.to_owned(), value.clone());
            }
        }
    } else {
        out.insert("type".to_owned(), Value::String("remote".to_owned()));
        out.insert("url".to_owned(), map.get("url")?.clone());
        if let Some(headers) = map.get("headers") {
            out.insert("headers".to_owned(), headers.clone());
        }
        if let Some(oauth) = map.get("oauth") {
            out.insert("oauth".to_owned(), migrate_oauth(oauth)?);
        }
    }
    if let Some(disabled) = disabled {
        out.insert("disabled".to_owned(), Value::Bool(disabled));
    }
    if let Some(codemode) = map.get("codemode") {
        out.insert("codemode".to_owned(), codemode.clone());
    }
    if let Some(timeout) = timeout {
        out.insert("timeout".to_owned(), timeout);
    }
    canonical::<ConfigEntryDocumentInfoMcpServersValue>(&Value::Object(out))
}

fn migrate_oauth(value: &Value) -> Option<Value> {
    if value == &Value::Bool(false) {
        return Some(Value::Bool(false));
    }
    let map = as_object(value)?;
    let mut out = Map::new();
    for (key, target) in [
        ("clientId", "client_id"),
        ("clientSecret", "client_secret"),
        ("scope", "scope"),
        ("callbackPort", "callback_port"),
        ("redirectUri", "redirect_uri"),
    ] {
        if let Some(item) = map.get(key) {
            out.insert(target.to_owned(), item.clone());
        }
    }
    Some(Value::Object(out))
}

pub fn decode_mcp(value: &Value) -> Option<Value> {
    let map = as_object(value)?;
    let mut out = Map::new();
    match map.get("type").and_then(Value::as_str)? {
        "local" => {
            out.insert("type".to_owned(), Value::String("local".to_owned()));
            string_array(map.get("command")?)?;
            out.insert("command".to_owned(), map.get("command")?.clone());
            if let Some(cwd) = map.get("cwd") {
                if !cwd.is_string() {
                    return None;
                }
                out.insert("cwd".to_owned(), cwd.clone());
            }
            if let Some(environment) = map.get("environment") {
                string_record(environment)?;
                out.insert("environment".to_owned(), environment.clone());
            }
        }
        "remote" => {
            out.insert("type".to_owned(), Value::String("remote".to_owned()));
            let url = map.get("url")?;
            if !url.is_string() {
                return None;
            }
            out.insert("url".to_owned(), url.clone());
            if let Some(headers) = map.get("headers") {
                string_record(headers)?;
                out.insert("headers".to_owned(), headers.clone());
            }
            if let Some(oauth) = map.get("oauth") {
                decode_oauth(oauth)?;
                out.insert("oauth".to_owned(), oauth.clone());
            }
        }
        _ => return None,
    }
    if let Some(enabled) = map.get("enabled") {
        if !enabled.is_boolean() {
            return None;
        }
        out.insert("enabled".to_owned(), enabled.clone());
    }
    if let Some(codemode) = map.get("codemode") {
        if !codemode.is_boolean() {
            return None;
        }
        out.insert("codemode".to_owned(), codemode.clone());
    }
    if let Some(timeout) = map.get("timeout") {
        if !is_positive_int(timeout) {
            return None;
        }
        out.insert("timeout".to_owned(), timeout.clone());
    }
    Some(Value::Object(out))
}

fn decode_oauth(value: &Value) -> Option<()> {
    if value == &Value::Bool(false) {
        return Some(());
    }
    let map = as_object(value)?;
    for key in ["clientId", "clientSecret", "scope", "redirectUri"] {
        if map.get(key).is_some_and(|item| !item.is_string()) {
            return None;
        }
    }
    if let Some(port) = map.get("callbackPort")
        && !port.as_f64().is_some_and(|number| {
            checks::is_int(number)
                && checks::is_greater_than_or_equal_to(number, 1.0)
                && checks::is_less_than_or_equal_to(number, 65535.0)
        })
    {
        return None;
    }
    Some(())
}

pub fn migrate_providers(value: Option<&Value>, diagnostics: &mut Vec<Diagnostic>) -> Vec<(String, Value)> {
    let Some(value) = value else { return Vec::new() };
    let Some(providers) = as_object(value) else {
        invalid(&["provider".to_owned()], diagnostics);
        return Vec::new();
    };
    let mut candidates: Vec<(String, String, Value)> = Vec::new();
    for (name, raw) in providers {
        let path = vec!["provider".to_owned(), name.clone()];
        diagnose_provider_unsupported(raw, &path, diagnostics);
        if invalid_provider_overlays(raw, &path, diagnostics) {
            continue;
        }
        let Some(info) = decode_provider(raw) else {
            invalid(&path, diagnostics);
            continue;
        };
        let destination = provider_id(name).to_owned();
        let Some(provider) = canonical::<ConfigEntryDocumentInfoProvidersValue>(&migrate_provider(name, &info)) else {
            invalid(&path, diagnostics);
            continue;
        };
        candidates.push((name.clone(), destination, provider));
    }

    let current: Vec<String> = candidates
        .iter()
        .filter(|(name, destination, _)| name == destination)
        .map(|(_, destination, _)| destination.clone())
        .collect();
    let mut result = Vec::new();
    for (name, destination, provider) in candidates {
        if name != destination && current.contains(&destination) {
            continue;
        }
        set_entry(&mut result, &destination, provider);
    }
    result
}

fn decode_provider(value: &Value) -> Option<Value> {
    let map = as_object(value)?;
    let mut out = Map::new();
    for (key, item) in map {
        let decoded = match key.as_str() {
            "api" | "name" | "id" | "npm" => {
                if !item.is_string() {
                    return None;
                }
                item.clone()
            }
            "env" | "whitelist" | "blacklist" => {
                string_array(item)?;
                item.clone()
            }
            "options" => decode_provider_options(item)?,
            "models" => decode_models(item)?,
            _ => item.clone(),
        };
        out.insert(key.clone(), decoded);
    }
    Some(Value::Object(out))
}

fn decode_provider_options(value: &Value) -> Option<Value> {
    let map = as_object(value)?;
    let mut out = Map::new();
    for key in PROVIDER_OPTION_KEYS {
        let Some(item) = map.get(key) else { continue };
        if !valid_provider_option(key, item) {
            return None;
        }
        out.insert(key.to_owned(), item.clone());
    }
    for (key, item) in map {
        if PROVIDER_OPTION_KEYS.contains(&key.as_str()) {
            continue;
        }
        out.insert(key.clone(), item.clone());
    }
    Some(Value::Object(out))
}

fn valid_provider_option(key: &str, item: &Value) -> bool {
    match key {
        "apiKey" | "baseURL" | "enterpriseUrl" => item.is_string(),
        "setCacheKey" => item.is_boolean(),
        "timeout" | "headerTimeout" => is_positive_int(item) || item == &Value::Bool(false),
        "chunkTimeout" => is_positive_int(item),
        _ => true,
    }
}

fn decode_models(value: &Value) -> Option<Value> {
    let models = as_object(value)?;
    let mut out = Map::new();
    for (name, model) in models {
        out.insert(name.clone(), decode_model(model)?);
    }
    Some(Value::Object(out))
}

fn decode_model(value: &Value) -> Option<Value> {
    let map = as_object(value)?;
    let mut out = Map::new();
    for (key, item) in map {
        let decoded = match key.as_str() {
            "id" | "name" | "family" | "release_date" => {
                if !item.is_string() {
                    return None;
                }
                item.clone()
            }
            "attachment" | "reasoning" | "temperature" | "tool_call" | "experimental" => {
                if !item.is_boolean() {
                    return None;
                }
                item.clone()
            }
            "interleaved" => {
                decode_interleaved(item)?;
                item.clone()
            }
            "cost" => {
                decode_cost(item)?;
                item.clone()
            }
            "limit" => {
                decode_limit(item)?;
                item.clone()
            }
            "modalities" => {
                decode_modalities(item)?;
                item.clone()
            }
            "status" => {
                if !MODEL_STATUSES.contains(&item.as_str()?) {
                    return None;
                }
                item.clone()
            }
            "provider" => {
                let provider = as_object(item)?;
                for key in ["npm", "api"] {
                    if provider.get(key).is_some_and(|value| !value.is_string()) {
                        return None;
                    }
                }
                item.clone()
            }
            "options" => {
                as_object(item)?;
                item.clone()
            }
            "headers" => {
                string_record(item)?;
                item.clone()
            }
            "variants" => decode_variants(item)?,
            _ => item.clone(),
        };
        out.insert(key.clone(), decoded);
    }
    Some(Value::Object(out))
}

fn decode_variants(value: &Value) -> Option<Value> {
    let variants = as_object(value)?;
    let mut out = Map::new();
    for (id, variant) in variants {
        out.insert(id.clone(), decode_variant(variant)?);
    }
    Some(Value::Object(out))
}

fn decode_variant(value: &Value) -> Option<Value> {
    let map = as_object(value)?;
    let mut out = Map::new();
    if let Some(disabled) = map.get("disabled") {
        if !disabled.is_boolean() {
            return None;
        }
        out.insert("disabled".to_owned(), disabled.clone());
    }
    for (key, item) in map {
        if key == "disabled" {
            continue;
        }
        out.insert(key.clone(), item.clone());
    }
    Some(Value::Object(out))
}

fn decode_interleaved(value: &Value) -> Option<()> {
    if value.is_boolean() || value.is_string() {
        return Some(());
    }
    let map = as_object(value)?;
    if !map.get("field").is_some_and(Value::is_string) {
        return None;
    }
    Some(())
}

fn decode_cost(value: &Value) -> Option<()> {
    let map = as_object(value)?;
    for key in ["input", "output"] {
        if !map.get(key).is_some_and(is_finite) {
            return None;
        }
    }
    for key in ["cache_read", "cache_write"] {
        if map.get(key).is_some_and(|item| !is_finite(item)) {
            return None;
        }
    }
    if let Some(over) = map.get("context_over_200k") {
        let over = as_object(over)?;
        for key in ["input", "output"] {
            if !over.get(key).is_some_and(is_finite) {
                return None;
            }
        }
        for key in ["cache_read", "cache_write"] {
            if over.get(key).is_some_and(|item| !is_finite(item)) {
                return None;
            }
        }
    }
    Some(())
}

fn decode_limit(value: &Value) -> Option<()> {
    let map = as_object(value)?;
    for key in ["context", "input", "output"] {
        if map.get(key).is_some_and(|item| !is_finite(item)) {
            return None;
        }
    }
    Some(())
}

fn decode_modalities(value: &Value) -> Option<()> {
    let map = as_object(value)?;
    for key in ["input", "output"] {
        if let Some(items) = map.get(key) {
            let Value::Array(items) = items else { return None };
            for item in items {
                if !MODEL_MODALITIES.contains(&item.as_str()?) {
                    return None;
                }
            }
        }
    }
    Some(())
}

fn migrate_provider(source: &str, info: &Value) -> Value {
    if source == "azure-cognitive-services" {
        return migrate_azure_provider(info);
    }
    if source == "google-vertex-anthropic" {
        return migrate_google_vertex_provider(info);
    }
    migrate_standard_provider(info)
}

struct ProviderOverlays {
    settings: Map<String, Value>,
    headers: Option<Map<String, Value>>,
    body: Option<Map<String, Value>>,
}

fn provider_options(options: &Map<String, Value>) -> ProviderOverlays {
    let mut settings = Map::new();
    for (key, value) in options {
        if key == "headers" || key == "body" {
            continue;
        }
        settings.insert(key.clone(), value.clone());
    }
    let headers = options.get("headers").and_then(as_object).map(|headers| {
        headers.iter().filter(|(_, value)| value.is_string()).map(|(key, value)| (key.clone(), value.clone())).collect()
    });
    let body = options.get("body").and_then(as_object).cloned();
    ProviderOverlays { settings, headers, body }
}

fn migrate_standard_provider(info: &Value) -> Value {
    let map = as_object(info).expect("decoded legacy provider");
    let options = map.get("options").and_then(as_object);
    let set_cache_key = options.and_then(|options| options.get("setCacheKey")).and_then(Value::as_bool);
    let mut rest = options.cloned().unwrap_or_default();
    rest.shift_remove("setCacheKey");
    let overlays = provider_options(&rest);

    let mut out = Map::new();
    if let Some(name) = map.get("name") {
        out.insert("name".to_owned(), name.clone());
    }
    if let Some(env) = map.get("env") {
        out.insert("env".to_owned(), env.clone());
    }
    let npm = map.get("npm").and_then(Value::as_str);
    if let Some(npm) = npm.filter(|value| !value.is_empty()) {
        out.insert("package".to_owned(), Value::String(package_specifier(npm)));
    }
    if let Some(set_cache_key) = set_cache_key {
        out.insert(
            "compatibility".to_owned(),
            object(vec![("supportsPromptCacheKey".to_owned(), Value::Bool(set_cache_key))]),
        );
    }
    let settings = if let Some(api) = map.get("api").and_then(Value::as_str).filter(|value| !value.is_empty()) {
        let mut settings = overlays.settings.clone();
        let base_url = settings.get("baseURL").and_then(Value::as_str).filter(|value| !value.is_empty()).unwrap_or(api);
        settings.insert("baseURL".to_owned(), Value::String(base_url.to_owned()));
        Some(settings)
    } else if options.is_some() {
        Some(overlays.settings.clone())
    } else {
        None
    };
    if let Some(settings) = settings {
        out.insert("settings".to_owned(), Value::Object(reorder_map(settings)));
    }
    if options.is_some() {
        if let Some(headers) = overlays.headers {
            out.insert("headers".to_owned(), Value::Object(reorder_map(headers)));
        }
        if let Some(body) = overlays.body {
            out.insert("body".to_owned(), Value::Object(reorder_map(body)));
        }
    }
    if let Some(Value::Object(models)) = map.get("models") {
        let mapped = models.iter().map(|(name, model)| (name.clone(), migrate_model(model, npm))).collect();
        out.insert("models".to_owned(), Value::Object(reorder_map(mapped)));
    }
    Value::Object(out)
}

fn migrate_azure_provider(info: &Value) -> Value {
    let map = as_object(info).expect("decoded legacy provider");
    let Value::Object(mut migrated) = migrate_standard_provider(info) else { unreachable!() };
    if let Some(Value::Array(env)) = migrated.get("env").cloned() {
        let env =
            env.into_iter().filter(|value| value.as_str() != Some("AZURE_COGNITIVE_SERVICES_RESOURCE_NAME")).collect();
        migrated.insert("env".to_owned(), Value::Array(env));
    }
    let compatible = map.get("npm").and_then(Value::as_str) == Some("@ai-sdk/openai-compatible");
    let api = map.get("api").and_then(Value::as_str).is_some_and(|api| !api.is_empty());
    if !compatible || api {
        return Value::Object(migrated);
    }
    let mut settings = match migrated.get("settings") {
        Some(Value::Object(settings)) => settings.clone(),
        _ => Map::new(),
    };
    settings.insert(
        "baseURL".to_owned(),
        Value::String(
            "https://${AZURE_COGNITIVE_SERVICES_RESOURCE_NAME}.cognitiveservices.azure.com/openai".to_owned(),
        ),
    );
    migrated.insert("settings".to_owned(), Value::Object(reorder_map(settings)));
    Value::Object(migrated)
}

fn migrate_google_vertex_provider(info: &Value) -> Value {
    let Value::Object(mut migrated) = migrate_standard_provider(info) else { unreachable!() };
    let package = match migrated.get("package").and_then(Value::as_str) {
        Some(package) if !package.is_empty() => package.to_owned(),
        _ => aisdk("@ai-sdk/google-vertex/anthropic"),
    };
    migrated.shift_remove("package");
    if let Some(Value::Object(models)) = migrated.get("models").cloned() {
        let mapped = models
            .into_iter()
            .map(|(name, model)| {
                let Value::Object(mut model) = model else { return (name, model) };
                let has_package = model.get("package").and_then(Value::as_str).is_some_and(|value| !value.is_empty());
                if !has_package {
                    model.insert("package".to_owned(), Value::String(package.clone()));
                }
                (name, Value::Object(model))
            })
            .collect();
        migrated.insert("models".to_owned(), Value::Object(reorder_map(mapped)));
    }
    Value::Object(migrated)
}

fn migrate_model(info: &Value, npm: Option<&str>) -> Value {
    let map = as_object(info).expect("decoded legacy model");
    let options = map.get("options").and_then(as_object);
    let disable_thinking_block_binding = options.is_some_and(|options| {
        block_binding_disabled(options, "thinking") || block_binding_disabled(options, "reasoningConfig")
    });
    let options = options.map(|options| {
        let mut options = options.clone();
        if disable_thinking_block_binding {
            for key in ["thinking", "reasoningConfig"] {
                let Some(Value::Object(entry)) = options.get(key).cloned() else { continue };
                if entry.get("blockBinding") != Some(&Value::Bool(false)) {
                    continue;
                }
                let mut rest = entry.clone();
                rest.shift_remove("blockBinding");
                if rest.is_empty() {
                    options.shift_remove(key);
                } else {
                    options.insert(key.to_owned(), Value::Object(rest));
                }
            }
        }
        options
    });

    let provider_npm = map.get("provider").and_then(as_object).and_then(|provider| provider.get("npm"));
    let compatible = provider_npm.and_then(Value::as_str).or(npm) == Some("@ai-sdk/openai-compatible");
    let migrated = options.as_ref().map(|options| split_model_options(options, compatible));
    let settings = migrated.as_ref().map(|(settings, _)| settings.clone());

    let compatibility = match map.get("interleaved") {
        Some(Value::String(reasoning_field)) => {
            Some(object(vec![("reasoningField".to_owned(), Value::String(reasoning_field.clone()))]))
        }
        Some(Value::Object(interleaved)) => interleaved
            .get("field")
            .and_then(Value::as_str)
            .map(|field| object(vec![("reasoningField".to_owned(), Value::String(field.to_owned()))])),
        _ => None,
    };
    let compatibility = if disable_thinking_block_binding {
        let mut compatibility = match compatibility {
            Some(Value::Object(compatibility)) => compatibility,
            _ => Map::new(),
        };
        compatibility.insert("supportsThinkingBlockBinding".to_owned(), Value::Bool(false));
        Some(Value::Object(reorder_map(compatibility)))
    } else {
        compatibility
    };

    let costs = map.get("cost").and_then(as_object).map(|cost| {
        let mut items = vec![migrate_cost(cost)];
        if let Some(over) = cost.get("context_over_200k").and_then(as_object) {
            let mut tiered = migrate_cost(over);
            if let Value::Object(fields) = &mut tiered {
                fields.insert(
                    "tier".to_owned(),
                    object(vec![
                        ("type".to_owned(), Value::String("context".to_owned())),
                        ("size".to_owned(), Value::Number(200_000.into())),
                    ]),
                );
            }
            items.push(tiered);
        }
        Value::Array(items)
    });

    let modalities = map.get("modalities").and_then(as_object);
    let capabilities = if map.contains_key("tool_call")
        || modalities.is_some_and(|modalities| modalities.contains_key("input") || modalities.contains_key("output"))
    {
        let mut capabilities = Map::new();
        if let Some(tools) = map.get("tool_call") {
            capabilities.insert("tools".to_owned(), tools.clone());
        }
        if let Some(input) = modalities.and_then(|modalities| modalities.get("input")) {
            capabilities.insert("input".to_owned(), input.clone());
        }
        if let Some(output) = modalities.and_then(|modalities| modalities.get("output")) {
            capabilities.insert("output".to_owned(), output.clone());
        }
        Some(Value::Object(reorder_map(capabilities)))
    } else {
        None
    };

    let variants = map.get("variants").and_then(as_object).map(|variants| {
        Value::Array(
            variants
                .iter()
                .map(|(id, options)| {
                    let (settings, body) =
                        split_model_options(as_object(options).expect("decoded variant"), compatible);
                    let mut item = Map::new();
                    item.insert("id".to_owned(), Value::String(id.clone()));
                    item.insert("settings".to_owned(), Value::Object(reorder_map(settings)));
                    if let Some(body) = body {
                        item.insert("body".to_owned(), Value::Object(reorder_map(body)));
                    }
                    Value::Object(item)
                })
                .collect(),
        )
    });

    let limit = map.get("limit").and_then(as_object).map(|limit| {
        let mut out = Map::new();
        for key in ["context", "input", "output"] {
            if let Some(value) = limit.get(key)
                && let Some(number) = value.as_f64().and_then(|number| serde_json::Number::from_f64(js_int(number)))
            {
                out.insert(key.to_owned(), Value::Number(number));
            }
        }
        Value::Object(reorder_map(out))
    });

    let mut out = Map::new();
    if let Some(id) = map.get("id") {
        out.insert("modelID".to_owned(), id.clone());
    }
    for key in ["family", "name"] {
        if let Some(value) = map.get(key) {
            out.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(compatibility) = compatibility {
        out.insert("compatibility".to_owned(), compatibility);
    }
    if let Some(npm) = map
        .get("provider")
        .and_then(as_object)
        .and_then(|provider| provider.get("npm"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        out.insert("package".to_owned(), Value::String(package_specifier(npm)));
    }
    let provider_api = map.get("provider").and_then(as_object).and_then(|provider| provider.get("api"));
    let settings = match provider_api.and_then(Value::as_str).filter(|value| !value.is_empty()) {
        Some(api) => {
            let mut settings = settings.unwrap_or_default();
            settings.insert("baseURL".to_owned(), Value::String(api.to_owned()));
            Some(settings)
        }
        None => settings,
    };
    if let Some(settings) = settings {
        out.insert("settings".to_owned(), Value::Object(reorder_map(settings)));
    }
    if let Some(capabilities) = capabilities {
        out.insert("capabilities".to_owned(), capabilities);
    }
    if let Some(headers) = map.get("headers") {
        out.insert("headers".to_owned(), headers.clone());
    }
    if let Some((_, body)) = migrated
        && let Some(body) = body
    {
        out.insert("body".to_owned(), Value::Object(reorder_map(body)));
    }
    if let Some(variants) = variants {
        out.insert("variants".to_owned(), variants);
    }
    if let Some(costs) = costs {
        out.insert("cost".to_owned(), costs);
    }
    if map.get("status").and_then(Value::as_str) == Some("deprecated") {
        out.insert("disabled".to_owned(), Value::Bool(true));
    }
    if let Some(limit) = limit {
        out.insert("limit".to_owned(), limit);
    }
    Value::Object(out)
}

fn block_binding_disabled(options: &Map<String, Value>, key: &str) -> bool {
    options.get(key).and_then(as_object).and_then(|entry| entry.get("blockBinding")).and_then(Value::as_bool)
        == Some(false)
}

fn migrate_cost(cost: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    for key in ["input", "output"] {
        if let Some(value) = cost.get(key) {
            out.insert(key.to_owned(), value.clone());
        }
    }
    let mut cache = Map::new();
    if let Some(read) = cost.get("cache_read") {
        cache.insert("read".to_owned(), read.clone());
    }
    if let Some(write) = cost.get("cache_write") {
        cache.insert("write".to_owned(), write.clone());
    }
    out.insert("cache".to_owned(), Value::Object(reorder_map(cache)));
    Value::Object(reorder_map(out))
}

fn split_model_options(
    options: &Map<String, Value>,
    compatible: bool,
) -> (Map<String, Value>, Option<Map<String, Value>>) {
    if !compatible {
        return (options.clone(), None);
    }
    let mut settings = Map::new();
    let mut body = Map::new();
    for (key, value) in options {
        if COMPATIBLE_KEYS.contains(&key.as_str()) {
            settings.insert(key.clone(), value.clone());
        } else {
            body.insert(key.clone(), value.clone());
        }
    }
    (settings, if body.is_empty() { None } else { Some(body) })
}

fn package_specifier(npm: &str) -> String {
    let name = match npm.strip_prefix("@opencode-ai/ai/") {
        Some(rest) => format!("@opencode/ai/{rest}"),
        None => match npm.strip_prefix("aisdk:") {
            Some(rest) => rest.to_owned(),
            None => npm.to_owned(),
        },
    };
    if name.starts_with("@opencode/ai/") { name } else { aisdk(&name) }
}

fn aisdk(value: &str) -> String {
    if value.starts_with("aisdk:") { value.to_owned() } else { format!("aisdk:{value}") }
}

fn diagnose_provider_unsupported(value: &Value, path: &[String], diagnostics: &mut Vec<Diagnostic>) {
    let Some(provider) = as_object(value) else { return };
    for key in UNSUPPORTED_PROVIDER {
        unsupported_if_present(provider, key, &push_path(path, key), diagnostics);
    }
    let Some(Value::Object(models)) = provider.get("models") else { return };
    for (name, model) in models {
        let Some(model) = as_object(model) else { continue };
        let model_path = push_path(&push_path(path, "models"), name);
        for key in UNSUPPORTED_MODEL {
            unsupported_if_present(model, key, &push_path(&model_path, key), diagnostics);
        }
        if model.contains_key("status") && model.get("status").and_then(Value::as_str) != Some("deprecated") {
            unsupported_if_present(model, "status", &push_path(&model_path, "status"), diagnostics);
        }
        if model.get("interleaved").is_some_and(Value::is_boolean) {
            unsupported_if_present(model, "interleaved", &push_path(&model_path, "interleaved"), diagnostics);
        }
    }
}

fn invalid_provider_overlays(value: &Value, path: &[String], diagnostics: &mut Vec<Diagnostic>) -> bool {
    let Some(provider) = as_object(value) else { return false };
    let Some(options) = provider.get("options").and_then(as_object) else { return false };
    let headers_invalid = options.get("headers").is_some_and(|headers| string_record(headers).is_none());
    let body_invalid = options.get("body").is_some_and(|body| as_object(body).is_none());
    if headers_invalid {
        invalid(&push_path(&push_path(path, "options"), "headers"), diagnostics);
    }
    if body_invalid {
        invalid(&push_path(&push_path(path, "options"), "body"), diagnostics);
    }
    headers_invalid || body_invalid
}

pub fn migrate_tools(value: Option<&Value>, diagnostics: &mut Vec<Diagnostic>) -> Vec<Value> {
    let Some(value) = value else { return Vec::new() };
    let Some(tools) = as_object(value) else {
        invalid(&["tools".to_owned()], diagnostics);
        return Vec::new();
    };
    let mut rules = Vec::new();
    for (action, raw) in tools {
        let Some(enabled) = raw.as_bool() else {
            invalid(&["tools".to_owned(), action.clone()], diagnostics);
            continue;
        };
        rules.push(rule_object(normalize_action(action), "*", if enabled { "allow" } else { "deny" }));
    }
    rules
}

pub fn migrate_permissions(value: Option<&Value>, diagnostics: &mut Vec<Diagnostic>) -> Vec<Value> {
    let Some(value) = value else { return Vec::new() };
    if let Value::String(action) = value {
        let path = ["permission".to_owned()];
        if !is_action(action) {
            invalid(&path, diagnostics);
            return Vec::new();
        }
        return vec![rule_object("*", "*", action)];
    }
    let Some(permissions) = as_object(value) else {
        invalid(&["permission".to_owned()], diagnostics);
        return Vec::new();
    };
    let mut rules = Vec::new();
    for (action, raw) in permissions {
        if let Value::String(effect) = raw {
            let path = ["permission".to_owned(), action.clone()];
            if !is_action(effect) {
                invalid(&path, diagnostics);
                continue;
            }
            rules.push(rule_object(normalize_action(action), "*", effect));
            continue;
        }
        let Some(resources) = as_object(raw) else {
            invalid(&["permission".to_owned(), action.clone()], diagnostics);
            continue;
        };
        for (index, (resource, effect)) in resources.iter().enumerate() {
            let path = ["permission".to_owned(), action.clone(), index.to_string()];
            let Some(effect) = effect.as_str().filter(|effect| is_action(effect)) else {
                invalid(&path, diagnostics);
                continue;
            };
            rules.push(rule_object(normalize_action(action), resource, effect));
        }
    }
    rules
}

pub fn diagnose_selection_map(value: Option<&Value>, path: &[String], diagnostics: &mut Vec<Diagnostic>) {
    let Some(Value::Object(entries)) = value else { return };
    for (name, entry) in entries {
        let Some(entry) = as_object(entry) else { continue };
        diagnose_selection(entry, &push_path(path, name), diagnostics);
    }
}

pub fn diagnose_agent_unsupported(value: Option<&Value>, path: &[String], diagnostics: &mut Vec<Diagnostic>) {
    let Some(Value::Object(agents)) = value else { return };
    for (name, agent) in agents {
        let Some(agent) = as_object(agent) else { continue };
        let agent_path = push_path(path, name);
        unsupported_if_present(agent, "name", &push_path(&agent_path, "name"), diagnostics);
        diagnose_selection(agent, &agent_path, diagnostics);
    }
}

fn diagnose_selection(value: &Map<String, Value>, path: &[String], diagnostics: &mut Vec<Diagnostic>) {
    let model_valid =
        value.get("model").and_then(Value::as_str).is_some_and(|model| model_selection(Some(model), None).is_some());
    if value.contains_key("model") && value.get("model").is_some_and(Value::is_string) && !model_valid {
        diagnostics.push(Diagnostic {
            kind: super::DiagnosticKind::Unsupported,
            path: push_path(path, "model"),
            message: super::UNSUPPORTED_MODEL_MESSAGE.to_owned(),
        });
    }
    let variant_unsupported = value
        .get("variant")
        .and_then(Value::as_str)
        .is_some_and(|variant| !model_valid || variant.is_empty() || variant.contains('#'));
    if value.get("variant").is_some_and(Value::is_string) && variant_unsupported {
        diagnostics.push(Diagnostic {
            kind: super::DiagnosticKind::Unsupported,
            path: push_path(path, "variant"),
            message: super::UNSUPPORTED_VARIANT_MESSAGE.to_owned(),
        });
    }
}

pub fn normalize_action(action: &str) -> &str {
    match action {
        "write" | "patch" | "apply_patch" => "edit",
        "task" => "subagent",
        "bash" => "shell",
        _ => action,
    }
}

fn rule_object(action: &str, resource: &str, effect: &str) -> Value {
    object(vec![
        ("action".to_owned(), Value::String(action.to_owned())),
        ("resource".to_owned(), Value::String(resource.to_owned())),
        ("effect".to_owned(), Value::String(effect.to_owned())),
    ])
}

fn permission_rule_values(entries: &[Value]) -> Vec<Value> {
    let mut rules = Vec::new();
    for entry in entries {
        let Value::Array(pair) = entry else { continue };
        let [Value::String(key), rule] = pair.as_slice() else { continue };
        let action = normalize_action(key);
        if let Value::String(effect) = rule {
            rules.push(rule_object(action, "*", effect));
            continue;
        }
        if let Value::Object(resources) = rule {
            for (resource, effect) in resources {
                let Some(effect) = effect.as_str() else { continue };
                rules.push(rule_object(action, resource, effect));
            }
        }
    }
    rules
}

fn as_object(value: &Value) -> Option<&Map<String, Value>> {
    match value {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn is_finite(value: &Value) -> bool {
    value.as_f64().is_some_and(f64::is_finite)
}

fn is_positive_int(value: &Value) -> bool {
    value.as_f64().is_some_and(|number| checks::is_int(number) && checks::is_greater_than(number, 0.0))
}

fn is_action(value: &str) -> bool {
    matches!(value, "ask" | "allow" | "deny")
}

fn is_agent_color(value: &Value) -> bool {
    let Some(text) = value.as_str() else { return false };
    if THEME_COLORS.contains(&text) {
        return true;
    }
    let Some(digits) = text.strip_prefix('#') else { return false };
    digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn string_array(value: &Value) -> Option<Vec<Value>> {
    match value {
        Value::Array(items) if items.iter().all(Value::is_string) => Some(items.clone()),
        _ => None,
    }
}

fn string_record(value: &Value) -> Option<Map<String, Value>> {
    let map = as_object(value)?;
    if !map.values().all(Value::is_string) {
        return None;
    }
    Some(map.clone())
}

fn boolean_entries(value: &Value) -> Option<Vec<(String, bool)>> {
    let map = as_object(value)?;
    map.iter().map(|(key, item)| Some((key.clone(), item.as_bool()?))).collect()
}

fn permission_entries(value: &Value) -> Option<Vec<(String, Value)>> {
    match value {
        Value::String(action) => is_action(action).then(|| vec![("*".to_owned(), value.clone())]),
        Value::Object(map) => {
            for (key, rule) in map {
                match rule {
                    Value::String(action) if is_action(action) => {}
                    Value::Object(resources) => {
                        for effect in resources.values() {
                            if !effect.as_str().is_some_and(is_action) {
                                return None;
                            }
                        }
                    }
                    _ => return None,
                }
                if ACTION_ONLY.contains(&key.as_str()) && !rule.is_string() {
                    return None;
                }
            }
            Some(map.iter().map(|(key, rule)| (key.clone(), rule.clone())).collect())
        }
        _ => None,
    }
}

fn entries(entries: &[(String, Value)]) -> Value {
    Value::Array(
        entries.iter().map(|(key, value)| Value::Array(vec![Value::String(key.clone()), value.clone()])).collect(),
    )
}

fn set_entry(entries: &mut Vec<(String, Value)>, key: &str, value: Value) {
    if let Some(entry) = entries.iter_mut().find(|(existing, _)| existing == key) {
        entry.1 = value;
        return;
    }
    entries.push((key.to_owned(), value));
}

fn js_int(value: f64) -> f64 {
    value.trunc().clamp(-9_007_199_254_740_991.0, 9_007_199_254_740_991.0)
}
