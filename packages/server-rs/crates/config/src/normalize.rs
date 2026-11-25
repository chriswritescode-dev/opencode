mod v1;

use serde_json::{Map, Value};

use opencode_contract::codec::{Decode, Encode, encode_json};
use opencode_contract::generated::{
    ConfigEntryDocumentInfoAgentsValue, ConfigEntryDocumentInfoCommandsValue, ConfigEntryDocumentInfoCompactionBuffer,
    ConfigEntryDocumentInfoCompactionKeep, ConfigEntryDocumentInfoEnterprise, ConfigEntryDocumentInfoExperimental,
    ConfigEntryDocumentInfoExperimentalImageGenerate, ConfigEntryDocumentInfoExperimentalVideoGenerate,
    ConfigEntryDocumentInfoFormatter, ConfigEntryDocumentInfoFormatterObjectValue, ConfigEntryDocumentInfoLsp,
    ConfigEntryDocumentInfoLspObjectValue, ConfigEntryDocumentInfoMcpServersValue, ConfigEntryDocumentInfoMcpTimeout,
    ConfigEntryDocumentInfoMedia, ConfigEntryDocumentInfoModelObject, ConfigEntryDocumentInfoModelString,
    ConfigEntryDocumentInfoPluginsItem, ConfigEntryDocumentInfoPoliciesItem, ConfigEntryDocumentInfoProvidersValue,
    ConfigEntryDocumentInfoReferencesValue, ConfigEntryDocumentInfoShare, ConfigEntryDocumentInfoToolOutput,
    ConfigEntryDocumentInfoUpdate, ConfigEntryDocumentInfoWebsearch, ConfigWorktree, PermissionRule,
};

const UNSUPPORTED_MESSAGE: &str = "omitted unsupported legacy setting";
const INVALID_MESSAGE: &str = "skipped malformed recognized value";
const CONFLICT_MESSAGE: &str = "retained native value over legacy value";
const REMOVED_AUTOMATIONS_MESSAGE: &str = "Use the top-level automations setting. Automations are enabled by default.";
const ENABLED_ONLY_MCP_MESSAGE: &str = "omitted enabled-only legacy MCP entry";
const REJECTED_ROOT_MESSAGE: &str = "rejected configuration because its root is not an object";
const UNSUPPORTED_MODEL_MESSAGE: &str = "omitted unsupported legacy model reference";
const UNSUPPORTED_VARIANT_MESSAGE: &str = "omitted unsupported legacy model variant";

const UNSUPPORTED_TOP_LEVEL: [&str; 4] = ["logLevel", "server", "subagent_depth", "layout"];
const UNSUPPORTED_EXPERIMENTAL: [&str; 5] =
    ["disable_paste_summary", "batch_tool", "openTelemetry", "primary_tools", "continue_loop_on_deny"];
const EXPERIMENTAL_PREFIX: [&str; 3] = ["portable_shell_scanner", "codemode_vm", "all_flags"];
const EXPERIMENTAL_SUFFIX: [&str; 6] =
    ["video_generate", "part_rows", "image_generate", "subagent_depth", "location_idle_timeout", "policies"];
const NATIVE_ATOMIC: [&str; 14] = [
    "$schema",
    "shell",
    "default_agent",
    "share",
    "enterprise",
    "username",
    "timezone",
    "automations",
    "snapshots",
    "media",
    "tool_output",
    "websearch",
    "worktree",
    "warming",
];
const MCP_TIMEOUT_FIELDS: [&str; 3] = ["startup", "catalog", "execution"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticKind {
    Conflict,
    Invalid,
    Removed,
    Unsupported,
}

impl DiagnosticKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conflict => "conflict",
            Self::Invalid => "invalid",
            Self::Removed => "removed",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub kind: DiagnosticKind,
    pub path: Vec<String>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Normalized {
    Normalized { encoded: Map<String, Value>, diagnostics: Vec<Diagnostic> },
    Rejected { diagnostics: Vec<Diagnostic> },
}

pub fn normalize(input: &Value) -> Normalized {
    let mut root = input.clone();
    reorder_value(&mut root);
    let Value::Object(input) = &root else {
        return Normalized::Rejected {
            diagnostics: vec![Diagnostic {
                kind: DiagnosticKind::Invalid,
                path: vec!["$".to_owned()],
                message: REJECTED_ROOT_MESSAGE.to_owned(),
            }],
        };
    };

    let mut diagnostics = Vec::new();
    let mut encoded = Map::new();

    for key in UNSUPPORTED_TOP_LEVEL {
        unsupported_if_present(input, key, &[key.to_owned()], &mut diagnostics);
    }

    let legacy_snapshots = input
        .get("snapshot")
        .and_then(|value| decode_encoded::<bool>(value, &["snapshot".to_owned()], &mut diagnostics));
    let legacy_update = input
        .get("autoupdate")
        .and_then(|value| decode_legacy_autoupdate(value, &["autoupdate".to_owned()], &mut diagnostics));
    let native_update = input.get("update").and_then(|value| {
        decode_encoded::<ConfigEntryDocumentInfoUpdate>(value, &["update".to_owned()], &mut diagnostics)
    });
    let legacy_share = match input.get("autoshare") {
        Some(value) => match decode_encoded::<bool>(value, &["autoshare".to_owned()], &mut diagnostics) {
            Some(Value::Bool(true)) => Some(Value::String("auto".to_owned())),
            _ => None,
        },
        None => None,
    };
    let legacy_media = input.get("attachment").and_then(|value| {
        decode_encoded::<ConfigEntryDocumentInfoMedia>(value, &["attachment".to_owned()], &mut diagnostics)
    });

    if let Some(value) = legacy_media {
        encoded.insert("media".to_owned(), value);
    }
    if let Some(value) = legacy_snapshots {
        encoded.insert("snapshots".to_owned(), value);
    }
    let migrated_update = match &legacy_update {
        Some(Value::Bool(false)) => Some(Value::String("disable".to_owned())),
        Some(Value::String(text)) if text == "notify" => Some(Value::String("notify".to_owned())),
        Some(Value::Bool(true)) => Some(Value::String("auto".to_owned())),
        _ => None,
    };
    if let Some(value) = prefer(migrated_update, native_update, &["update".to_owned()], &mut diagnostics) {
        encoded.insert("update".to_owned(), value);
    }
    if let Some(value) = legacy_share {
        encoded.insert("share".to_owned(), value);
    }

    let legacy_references = decode_map(
        input.get("reference"),
        &["reference".to_owned()],
        &mut diagnostics,
        decode_encoded::<ConfigEntryDocumentInfoReferencesValue>,
    );
    let native_references = decode_map(
        input.get("references"),
        &["references".to_owned()],
        &mut diagnostics,
        decode_encoded::<ConfigEntryDocumentInfoReferencesValue>,
    );
    merge_map(
        &mut encoded,
        "references",
        &legacy_references,
        &native_references,
        is_object(input.get("reference")) || is_object(input.get("references")),
        &mut diagnostics,
    );

    let legacy_commands = map_values(
        &map_from_pairs(migrate_commands(input.get("command"), &mut diagnostics)),
        &["command".to_owned()],
        &mut diagnostics,
        canonical_command,
    );
    diagnose_selection_map(input.get("command"), &["command".to_owned()], &mut diagnostics);
    let native_commands =
        decode_map(input.get("commands"), &["commands".to_owned()], &mut diagnostics, decode_command_entry);
    merge_map(
        &mut encoded,
        "commands",
        &legacy_commands,
        &native_commands,
        is_object(input.get("command")) || is_object(input.get("commands")),
        &mut diagnostics,
    );

    let legacy_agents = map_values(
        &map_from_pairs(migrate_agents(input.get("agent"), &mut diagnostics)),
        &["agent".to_owned()],
        &mut diagnostics,
        canonical_agent,
    );
    let legacy_small_model = input
        .get("small_model")
        .and_then(|value| decode_encoded::<String>(value, &["small_model".to_owned()], &mut diagnostics));
    let migrated_small_model = migrate_small_model(legacy_small_model.as_ref(), &mut diagnostics);
    if is_non_empty_string(legacy_small_model.as_ref()) && migrated_small_model.is_none() {
        diagnostics.push(Diagnostic {
            kind: DiagnosticKind::Unsupported,
            path: vec!["small_model".to_owned()],
            message: UNSUPPORTED_MODEL_MESSAGE.to_owned(),
        });
    }
    let legacy_agents = match &migrated_small_model {
        Some(model) => with_default_model(&legacy_agents, model),
        None => legacy_agents,
    };
    let mode_agents = map_values(
        &map_from_pairs(migrate_mode_agents(input.get("mode"), &mut diagnostics)),
        &["mode".to_owned()],
        &mut diagnostics,
        canonical_agent,
    );
    let migrated_agents = merge_maps(&legacy_agents, &mode_agents, &["agents".to_owned()], &mut diagnostics);
    let renamed_agents = input.get("agents").map(rename_agent_permissions);
    let native_agents =
        decode_map(renamed_agents.as_ref(), &["agents".to_owned()], &mut diagnostics, decode_agent_entry);
    diagnose_agent_unsupported(input.get("agent"), &["agent".to_owned()], &mut diagnostics);
    diagnose_agent_unsupported(input.get("mode"), &["mode".to_owned()], &mut diagnostics);
    merge_map(
        &mut encoded,
        "agents",
        &migrated_agents,
        &native_agents,
        migrated_small_model.is_some()
            || is_object(input.get("agent"))
            || is_object(input.get("mode"))
            || is_object(input.get("agents")),
        &mut diagnostics,
    );

    let legacy_providers = map_from_pairs(migrate_providers(input.get("provider"), &mut diagnostics));
    let native_providers = decode_map(
        input.get("providers"),
        &["providers".to_owned()],
        &mut diagnostics,
        decode_encoded::<ConfigEntryDocumentInfoProvidersValue>,
    );
    merge_map(
        &mut encoded,
        "providers",
        &legacy_providers,
        &native_providers,
        is_object(input.get("provider")) || is_object(input.get("providers")),
        &mut diagnostics,
    );

    let mut permissions = migrate_tools(input.get("tools"), &mut diagnostics);
    permissions.extend(migrate_permissions(input.get("permission"), &mut diagnostics));
    let renamed_permissions = input.get("permissions").map(rename_tools);
    permissions.extend(decode_list(
        renamed_permissions.as_ref(),
        &["permissions".to_owned()],
        &mut diagnostics,
        decode_encoded::<PermissionRule>,
    ));
    if !permissions.is_empty() || is_array(input.get("permissions")) {
        encoded.insert("permissions".to_owned(), Value::Array(permissions));
    }

    let legacy_plugins = decode_list(input.get("plugin"), &["plugin".to_owned()], &mut diagnostics, decode_plugin_spec);
    let native_plugins = decode_list(
        input.get("plugins"),
        &["plugins".to_owned()],
        &mut diagnostics,
        decode_encoded::<ConfigEntryDocumentInfoPluginsItem>,
    );
    if !legacy_plugins.is_empty()
        || !native_plugins.is_empty()
        || is_array(input.get("plugin"))
        || is_array(input.get("plugins"))
    {
        let mut plugins = legacy_plugins;
        plugins.extend(native_plugins);
        encoded.insert("plugins".to_owned(), Value::Array(plugins));
    }

    normalize_skills(input, &mut encoded, &mut diagnostics);
    normalize_mcp(input, &mut encoded, &mut diagnostics);
    normalize_compaction(input, &mut encoded, &mut diagnostics);
    normalize_policies(input, &mut encoded, &mut diagnostics);
    normalize_experimental(input, &mut encoded, &mut diagnostics);
    normalize_watcher(input, &mut encoded, &mut diagnostics);
    normalize_formatter(input, &mut encoded, &mut diagnostics);
    normalize_lsp(input, &mut encoded, &mut diagnostics);

    if let Some(value) = input.get("model")
        && let Some(model) = decode_model_selection(value, &["model".to_owned()], &mut diagnostics, true)
    {
        encoded.insert("model".to_owned(), model);
    }

    for key in NATIVE_ATOMIC {
        let Some(value) = input.get(key) else { continue };
        let path = [key.to_owned()];
        let Some(decoded) = decode_native_atomic(key, value, &path, &mut diagnostics) else { continue };
        overlay(&mut encoded, key, decoded, &path, &mut diagnostics);
    }

    let instructions = decode_list(
        input.get("instructions"),
        &["instructions".to_owned()],
        &mut diagnostics,
        decode_encoded::<String>,
    );
    if !instructions.is_empty() || is_array(input.get("instructions")) {
        encoded.insert("instructions".to_owned(), Value::Array(instructions));
    }

    Normalized::Normalized { encoded, diagnostics }
}

fn normalize_skills(input: &Map<String, Value>, encoded: &mut Map<String, Value>, diagnostics: &mut Vec<Diagnostic>) {
    let Some(skills) = input.get("skills") else { return };
    match skills {
        Value::Array(_) => {
            let decoded = decode_list(Some(skills), &["skills".to_owned()], diagnostics, decode_encoded::<String>);
            encoded.insert("skills".to_owned(), Value::Array(decoded));
        }
        Value::Object(skills) => {
            let mut decoded = decode_list(
                skills.get("paths"),
                &["skills".to_owned(), "paths".to_owned()],
                diagnostics,
                decode_encoded::<String>,
            );
            decoded.extend(decode_list(
                skills.get("urls"),
                &["skills".to_owned(), "urls".to_owned()],
                diagnostics,
                decode_encoded::<String>,
            ));
            encoded.insert("skills".to_owned(), Value::Array(decoded));
        }
        _ => invalid(&["skills".to_owned()], diagnostics),
    }
}

fn normalize_mcp(input: &Map<String, Value>, encoded: &mut Map<String, Value>, diagnostics: &mut Vec<Diagnostic>) {
    let mut legacy_servers = Map::new();
    let mut native_servers = Map::new();
    let mut timeout = Map::new();

    if let Some(Value::Object(experimental)) = input.get("experimental")
        && let Some(value) = experimental.get("mcp_timeout")
    {
        let path = ["experimental".to_owned(), "mcp_timeout".to_owned()];
        if let Some(decoded) = decode_mcp_timeout_leaf(value, "startup", &path, diagnostics) {
            timeout.insert("catalog".to_owned(), decoded.clone());
            timeout.insert("execution".to_owned(), decoded);
        }
    }

    if let Some(mcp) = input.get("mcp") {
        if !matches!(mcp, Value::Object(_)) {
            invalid(&["mcp".to_owned()], diagnostics);
        }
        if let Value::Object(mcp) = mcp {
            for (name, value) in mcp {
                let path = ["mcp".to_owned(), name.clone()];
                if is_enabled_only_mcp(value) {
                    diagnostics.push(Diagnostic {
                        kind: DiagnosticKind::Unsupported,
                        path: path.to_vec(),
                        message: ENABLED_ONLY_MCP_MESSAGE.to_owned(),
                    });
                    continue;
                }
                if name == "servers" && !is_direct_legacy_mcp(value) {
                    let expanded = expand_server_timeouts(value);
                    let servers = decode_map(
                        Some(&expanded),
                        &path,
                        diagnostics,
                        decode_encoded::<ConfigEntryDocumentInfoMcpServersValue>,
                    );
                    for (key, server) in servers {
                        native_servers.insert(key, server);
                    }
                    continue;
                }
                if name == "timeout" && !is_direct_legacy_mcp(value) {
                    let migrated = match value {
                        Value::Number(_) => migrate_mcp_timeout(value),
                        _ => value.clone(),
                    };
                    normalize_mcp_timeout(&migrated, &mut timeout, &path, diagnostics);
                    continue;
                }
                if let Some(server) = migrate_legacy_mcp(value, &path, diagnostics) {
                    legacy_servers.insert(name.clone(), server);
                }
            }
        }
    }

    let servers = merge_maps(&legacy_servers, &native_servers, &["mcp".to_owned(), "servers".to_owned()], diagnostics);
    if servers.is_empty() && timeout.is_empty() {
        if let Some(Value::Object(mcp)) = input.get("mcp")
            && mcp.is_empty()
        {
            encoded.insert("mcp".to_owned(), Value::Object(Map::new()));
        }
        return;
    }
    let mut mcp = Map::new();
    if !timeout.is_empty() {
        mcp.insert("timeout".to_owned(), Value::Object(timeout));
    }
    if !servers.is_empty() {
        mcp.insert("servers".to_owned(), Value::Object(servers));
    }
    encoded.insert("mcp".to_owned(), Value::Object(mcp));
}

fn expand_server_timeouts(value: &Value) -> Value {
    let Value::Object(servers) = value else { return value.clone() };
    let mut result = Map::new();
    for (name, server) in servers {
        let Value::Object(server) = server else {
            result.insert(name.clone(), server.clone());
            continue;
        };
        let Some(Value::Number(timeout)) = server.get("timeout") else {
            result.insert(name.clone(), Value::Object(server.clone()));
            continue;
        };
        let mut updated = server.clone();
        updated.insert("timeout".to_owned(), migrate_mcp_timeout(&Value::Number(timeout.clone())));
        result.insert(name.clone(), Value::Object(updated));
    }
    Value::Object(reorder_map(result))
}

fn migrate_mcp_timeout(value: &Value) -> Value {
    object(vec![
        ("startup".to_owned(), value.clone()),
        ("catalog".to_owned(), value.clone()),
        ("execution".to_owned(), value.clone()),
    ])
}

fn normalize_mcp_timeout(
    value: &Value,
    timeout: &mut Map<String, Value>,
    path: &[String],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Value::Object(object) = value else {
        invalid(path, diagnostics);
        return;
    };
    let recognized: Vec<&str> = MCP_TIMEOUT_FIELDS.iter().copied().filter(|key| own(object, key)).collect();
    if !object.is_empty() && recognized.is_empty() {
        invalid(path, diagnostics);
        return;
    }
    for key in recognized {
        let Some(field) = object.get(key) else { continue };
        let field_path = push_path(path, key);
        if let Some(leaf) = decode_mcp_timeout_leaf(field, key, &field_path, diagnostics) {
            overlay(timeout, key, leaf, &field_path, diagnostics);
        }
    }
}

fn decode_mcp_timeout_leaf(
    value: &Value,
    key: &str,
    path: &[String],
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Value> {
    decode_one_key::<ConfigEntryDocumentInfoMcpTimeout>(key, value, path, diagnostics)
}

fn normalize_compaction(
    input: &Map<String, Value>,
    encoded: &mut Map<String, Value>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(compaction) = input.get("compaction") else { return };
    let Value::Object(compaction) = compaction else {
        invalid(&["compaction".to_owned()], diagnostics);
        return;
    };
    unsupported_if_present(compaction, "tail_turns", &["compaction".to_owned(), "tail_turns".to_owned()], diagnostics);
    unsupported_if_present(compaction, "prune", &["compaction".to_owned(), "prune".to_owned()], diagnostics);

    let mut result = Map::new();
    for key in ["auto", "background"] {
        let Some(value) = compaction.get(key) else { continue };
        let path = ["compaction".to_owned(), key.to_owned()];
        if let Some(decoded) = decode_encoded::<bool>(value, &path, diagnostics) {
            result.insert(key.to_owned(), decoded);
        }
    }

    let legacy_tokens = compaction.get("preserve_recent_tokens").and_then(|value| {
        decode_one_key::<ConfigEntryDocumentInfoCompactionKeep>(
            "tokens",
            value,
            &["compaction".to_owned(), "preserve_recent_tokens".to_owned()],
            diagnostics,
        )
    });
    let native_keep = match compaction.get("keep") {
        Some(Value::Object(keep)) => Some(keep),
        _ => None,
    };
    if own(compaction, "keep") && native_keep.is_none() {
        invalid(&["compaction".to_owned(), "keep".to_owned()], diagnostics);
    }
    let native_tokens = native_keep.and_then(|keep| {
        keep.get("tokens").and_then(|value| {
            decode_one_key::<ConfigEntryDocumentInfoCompactionKeep>(
                "tokens",
                value,
                &["compaction".to_owned(), "keep".to_owned(), "tokens".to_owned()],
                diagnostics,
            )
        })
    });
    if let Some(tokens) = prefer(
        legacy_tokens,
        native_tokens,
        &["compaction".to_owned(), "keep".to_owned(), "tokens".to_owned()],
        diagnostics,
    ) {
        result.insert("keep".to_owned(), object(vec![("tokens".to_owned(), tokens)]));
    }

    let legacy_buffer = compaction.get("reserved").and_then(|value| {
        decode_encoded::<ConfigEntryDocumentInfoCompactionBuffer>(
            value,
            &["compaction".to_owned(), "reserved".to_owned()],
            diagnostics,
        )
    });
    let native_buffer = compaction.get("buffer").and_then(|value| {
        decode_encoded::<ConfigEntryDocumentInfoCompactionBuffer>(
            value,
            &["compaction".to_owned(), "buffer".to_owned()],
            diagnostics,
        )
    });
    if let Some(buffer) =
        prefer(legacy_buffer, native_buffer, &["compaction".to_owned(), "buffer".to_owned()], diagnostics)
    {
        result.insert("buffer".to_owned(), buffer);
    }

    if !result.is_empty() || compaction.is_empty() {
        encoded.insert("compaction".to_owned(), Value::Object(result));
    }
}

fn normalize_policies(input: &Map<String, Value>, encoded: &mut Map<String, Value>, diagnostics: &mut Vec<Diagnostic>) {
    let mut generated = Vec::new();
    let enabled = decode_provider_list(input, "enabled_providers", diagnostics);
    if enabled.present && (!enabled.non_empty || !enabled.values.is_empty()) {
        generated.push(policy("provider.use", "*", "deny"));
        for resource in &enabled.values {
            generated.push(policy("provider.use", provider_id(resource), "allow"));
        }
    }
    let disabled = decode_provider_list(input, "disabled_providers", diagnostics);
    for resource in &disabled.values {
        generated.push(policy("provider.use", provider_id(resource), "deny"));
    }

    let experimental = match input.get("experimental") {
        Some(Value::Object(experimental)) => decode_list(
            experimental.get("policies"),
            &["experimental".to_owned(), "policies".to_owned()],
            diagnostics,
            decode_policy,
        ),
        _ => Vec::new(),
    };
    let native = decode_list(input.get("policies"), &["policies".to_owned()], diagnostics, decode_policy);
    let experimental_policies = matches!(input.get("experimental"), Some(Value::Object(experimental)) if is_array(experimental.get("policies")));
    if !generated.is_empty()
        || !experimental.is_empty()
        || !native.is_empty()
        || is_array(input.get("policies"))
        || experimental_policies
    {
        let mut policies = generated;
        policies.extend(experimental);
        policies.extend(native);
        encoded.insert("policies".to_owned(), Value::Array(policies));
    }
}

struct ProviderList {
    present: bool,
    non_empty: bool,
    values: Vec<String>,
}

fn decode_provider_list(input: &Map<String, Value>, key: &str, diagnostics: &mut Vec<Diagnostic>) -> ProviderList {
    let Some(value) = input.get(key) else {
        return ProviderList { present: false, non_empty: false, values: Vec::new() };
    };
    if !matches!(value, Value::Array(_)) {
        invalid(&[key.to_owned()], diagnostics);
        return ProviderList { present: true, non_empty: true, values: Vec::new() };
    }
    let decoded = decode_list(Some(value), &[key.to_owned()], diagnostics, decode_encoded::<String>);
    ProviderList {
        present: true,
        non_empty: !matches!(value, Value::Array(items) if items.is_empty()),
        values: decoded
            .into_iter()
            .filter_map(|value| match value {
                Value::String(text) => Some(text),
                _ => None,
            })
            .collect(),
    }
}

fn decode_policy(value: &Value, path: &[String], diagnostics: &mut Vec<Diagnostic>) -> Option<Value> {
    let encoded = decode_encoded::<ConfigEntryDocumentInfoPoliciesItem>(value, path, diagnostics)?;
    Some(transform_policy(encoded))
}

fn transform_policy(encoded: Value) -> Value {
    let Value::Object(mut map) = encoded else { return encoded };
    if map.get("action").and_then(Value::as_str) != Some("integration.use") {
        return Value::Object(map);
    }
    let Some(resource) = map.get("resource").and_then(Value::as_str).map(str::to_owned) else {
        return Value::Object(map);
    };
    let Some(separator) = resource.find(':') else { return Value::Object(map) };
    let action = match &resource[..separator] {
        "mcp" => "mcp.use",
        "skill" => "skill.use",
        "plugin" => "plugin.use",
        _ => return Value::Object(map),
    };
    map.insert("action".to_owned(), Value::String(action.to_owned()));
    map.insert("resource".to_owned(), Value::String(resource[separator + 1..].to_owned()));
    Value::Object(map)
}

fn policy(action: &str, resource: &str, effect: &str) -> Value {
    object(vec![
        ("action".to_owned(), Value::String(action.to_owned())),
        ("resource".to_owned(), Value::String(resource.to_owned())),
        ("effect".to_owned(), Value::String(effect.to_owned())),
    ])
}

fn normalize_experimental(
    input: &Map<String, Value>,
    encoded: &mut Map<String, Value>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut result = Map::new();
    if let Some(experimental) = input.get("experimental") {
        match experimental {
            Value::Object(experimental) => {
                for key in UNSUPPORTED_EXPERIMENTAL {
                    unsupported_if_present(
                        experimental,
                        key,
                        &["experimental".to_owned(), key.to_owned()],
                        diagnostics,
                    );
                }
                for key in ["automations", "schedules"] {
                    if !own(experimental, key) {
                        continue;
                    }
                    diagnostics.push(Diagnostic {
                        kind: DiagnosticKind::Removed,
                        path: vec!["experimental".to_owned(), key.to_owned()],
                        message: REMOVED_AUTOMATIONS_MESSAGE.to_owned(),
                    });
                }
                for key in experimental_fields() {
                    if key == "policies" || !own(experimental, key) {
                        continue;
                    }
                    let Some(value) = experimental.get(key) else { continue };
                    let path = ["experimental".to_owned(), key.to_owned()];
                    if let Some(decoded) = decode_experimental_field(key, value, &path, diagnostics) {
                        result.insert(key.to_owned(), decoded);
                    }
                }
            }
            _ => invalid(&["experimental".to_owned()], diagnostics),
        }
    }
    let empty_experimental =
        matches!(input.get("experimental"), Some(Value::Object(experimental)) if experimental.is_empty());
    if !result.is_empty() || empty_experimental {
        encoded.insert("experimental".to_owned(), Value::Object(result));
    }
}

fn experimental_fields() -> Vec<&'static str> {
    let mut fields = EXPERIMENTAL_PREFIX.to_vec();
    for key in crate::generated::flags::REGISTRY.iter().map(|(key, _)| *key) {
        if !fields.contains(&key) {
            fields.push(key);
        }
    }
    for key in EXPERIMENTAL_SUFFIX {
        if !fields.contains(&key) {
            fields.push(key);
        }
    }
    fields
}

fn decode_experimental_field(
    key: &str,
    value: &Value,
    path: &[String],
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Value> {
    match key {
        "video_generate" => {
            let encoded = decode_encoded::<ConfigEntryDocumentInfoExperimentalVideoGenerate>(value, path, diagnostics)?;
            Some(rewrite_model_field(encoded))
        }
        "image_generate" => {
            let encoded = decode_encoded::<ConfigEntryDocumentInfoExperimentalImageGenerate>(value, path, diagnostics)?;
            Some(rewrite_model_field(encoded))
        }
        "subagent_depth" => {
            decode_one_key::<ConfigEntryDocumentInfoExperimental>("subagent_depth", value, path, diagnostics)
        }
        "location_idle_timeout" => decode_location_idle_timeout(value, path, diagnostics),
        _ => decode_encoded::<bool>(value, path, diagnostics),
    }
}

fn decode_location_idle_timeout(value: &Value, path: &[String], diagnostics: &mut Vec<Diagnostic>) -> Option<Value> {
    match value {
        Value::Bool(false) => Some(Value::Bool(false)),
        Value::String(text) => match duration_string(text) {
            Some(canonical) => Some(Value::String(canonical)),
            None => {
                invalid(path, diagnostics);
                None
            }
        },
        _ => {
            invalid(path, diagnostics);
            None
        }
    }
}

fn normalize_watcher(input: &Map<String, Value>, encoded: &mut Map<String, Value>, diagnostics: &mut Vec<Diagnostic>) {
    let Some(watcher) = input.get("watcher") else { return };
    let Value::Object(watcher) = watcher else {
        invalid(&["watcher".to_owned()], diagnostics);
        return;
    };
    let ignore = decode_list(
        watcher.get("ignore"),
        &["watcher".to_owned(), "ignore".to_owned()],
        diagnostics,
        decode_encoded::<String>,
    );
    let value = if !ignore.is_empty() || is_array(watcher.get("ignore")) {
        object(vec![("ignore".to_owned(), Value::Array(ignore))])
    } else {
        Value::Object(Map::new())
    };
    encoded.insert("watcher".to_owned(), value);
}

fn normalize_formatter(
    input: &Map<String, Value>,
    encoded: &mut Map<String, Value>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(formatter) = input.get("formatter") else { return };
    if matches!(formatter, Value::Bool(_)) {
        if let Some(decoded) =
            decode_encoded::<ConfigEntryDocumentInfoFormatter>(formatter, &["formatter".to_owned()], diagnostics)
        {
            encoded.insert("formatter".to_owned(), decoded);
        }
        return;
    }
    let entries = decode_map(
        Some(formatter),
        &["formatter".to_owned()],
        diagnostics,
        decode_encoded::<ConfigEntryDocumentInfoFormatterObjectValue>,
    );
    if let Value::Object(formatter) = formatter
        && (formatter.is_empty() || !entries.is_empty())
    {
        encoded.insert("formatter".to_owned(), Value::Object(entries));
    }
}

fn normalize_lsp(input: &Map<String, Value>, encoded: &mut Map<String, Value>, diagnostics: &mut Vec<Diagnostic>) {
    let Some(lsp) = input.get("lsp") else { return };
    if matches!(lsp, Value::Bool(_)) {
        if let Some(decoded) = decode_encoded::<ConfigEntryDocumentInfoLsp>(lsp, &["lsp".to_owned()], diagnostics) {
            encoded.insert("lsp".to_owned(), decoded);
        }
        return;
    }
    let entries = decode_map(
        Some(lsp),
        &["lsp".to_owned()],
        diagnostics,
        decode_encoded::<ConfigEntryDocumentInfoLspObjectValue>,
    );
    if let Value::Object(lsp) = lsp
        && (lsp.is_empty() || !entries.is_empty())
    {
        encoded.insert("lsp".to_owned(), Value::Object(entries));
    }
}

fn decode_native_atomic(key: &str, value: &Value, path: &[String], diagnostics: &mut Vec<Diagnostic>) -> Option<Value> {
    match key {
        "$schema" | "shell" | "default_agent" | "username" | "timezone" => {
            decode_encoded::<String>(value, path, diagnostics)
        }
        "share" => decode_encoded::<ConfigEntryDocumentInfoShare>(value, path, diagnostics),
        "enterprise" => decode_encoded::<ConfigEntryDocumentInfoEnterprise>(value, path, diagnostics),
        "automations" | "snapshots" => decode_encoded::<bool>(value, path, diagnostics),
        "media" => decode_encoded::<ConfigEntryDocumentInfoMedia>(value, path, diagnostics),
        "tool_output" => decode_encoded::<ConfigEntryDocumentInfoToolOutput>(value, path, diagnostics),
        "websearch" => decode_encoded::<ConfigEntryDocumentInfoWebsearch>(value, path, diagnostics),
        "worktree" => decode_worktree(value, path, diagnostics),
        "warming" => decode_warming(value, path, diagnostics),
        _ => None,
    }
}

fn decode_worktree(value: &Value, path: &[String], diagnostics: &mut Vec<Diagnostic>) -> Option<Value> {
    let encoded = decode_encoded::<ConfigWorktree>(value, path, diagnostics)?;
    let Value::Object(mut map) = encoded else {
        invalid(path, diagnostics);
        return None;
    };
    let Some(Value::String(directory)) = map.shift_remove("directory") else {
        invalid(path, diagnostics);
        return None;
    };
    let trimmed = opencode_contract::checks::trim(&directory);
    if trimmed.is_empty() {
        invalid(path, diagnostics);
        return None;
    }
    map.insert("directory".to_owned(), Value::String(trimmed.to_owned()));
    Some(Value::Object(map))
}

fn decode_warming(value: &Value, path: &[String], diagnostics: &mut Vec<Diagnostic>) -> Option<Value> {
    if matches!(value, Value::Bool(_)) {
        return Some(value.clone());
    }
    let Value::Object(warming) = value else {
        invalid(path, diagnostics);
        return None;
    };
    let mut result = Map::new();
    if let Some(prompt) = warming.get("prompt") {
        let Value::String(prompt) = prompt else {
            invalid(path, diagnostics);
            return None;
        };
        result.insert("prompt".to_owned(), Value::String(prompt.clone()));
    }
    if let Some(interval) = warming.get("interval") {
        let Value::String(text) = interval else {
            invalid(path, diagnostics);
            return None;
        };
        if text == "auto" {
            result.insert("interval".to_owned(), Value::String("auto".to_owned()));
        } else {
            let Some(canonical) = duration_string(text) else {
                invalid(path, diagnostics);
                return None;
            };
            result.insert("interval".to_owned(), Value::String(canonical));
        }
    }
    if let Some(duration) = warming.get("duration") {
        let Value::String(text) = duration else {
            invalid(path, diagnostics);
            return None;
        };
        let Some(canonical) = duration_string(text) else {
            invalid(path, diagnostics);
            return None;
        };
        result.insert("duration".to_owned(), Value::String(canonical));
    }
    Some(Value::Object(result))
}

fn decode_agent_entry(value: &Value, path: &[String], diagnostics: &mut Vec<Diagnostic>) -> Option<Value> {
    let value = drop_invalid_color(value);
    let encoded = decode_encoded::<ConfigEntryDocumentInfoAgentsValue>(&value, path, diagnostics)?;
    Some(rewrite_model_field(encoded))
}

fn decode_command_entry(value: &Value, path: &[String], diagnostics: &mut Vec<Diagnostic>) -> Option<Value> {
    let encoded = decode_encoded::<ConfigEntryDocumentInfoCommandsValue>(value, path, diagnostics)?;
    Some(rewrite_model_field(encoded))
}

fn canonical_agent(value: &Value) -> Option<Value> {
    Some(rewrite_model_field(canonical::<ConfigEntryDocumentInfoAgentsValue>(value)?))
}

fn canonical_command(value: &Value) -> Option<Value> {
    Some(rewrite_model_field(canonical::<ConfigEntryDocumentInfoCommandsValue>(value)?))
}

fn drop_invalid_color(value: &Value) -> Value {
    let Value::Object(map) = value else { return value.clone() };
    let Some(color) = map.get("color") else { return value.clone() };
    if is_valid_color(color) {
        return value.clone();
    }
    let mut copy = map.clone();
    copy.shift_remove("color");
    Value::Object(copy)
}

fn is_valid_color(value: &Value) -> bool {
    let Some(text) = value.as_str() else { return false };
    let Some(digits) = text.strip_prefix('#') else { return false };
    digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn decode_model_selection(
    value: &Value,
    path: &[String],
    diagnostics: &mut Vec<Diagnostic>,
    rename_provider: bool,
) -> Option<Value> {
    match value {
        Value::String(text) => {
            decode_encoded::<ConfigEntryDocumentInfoModelString>(value, path, diagnostics)?;
            let encoded = model_selection_from_string(text)?;
            Some(if rename_provider { rename_model_provider(encoded) } else { encoded })
        }
        Value::Object(model) => {
            decode_encoded::<ConfigEntryDocumentInfoModelObject>(value, path, diagnostics)?;
            let provider = model.get("providerID").and_then(Value::as_str)?;
            let id = model.get("model").and_then(Value::as_str)?;
            let variant = model.get("variant").and_then(Value::as_str);
            Some(model_object(if rename_provider { provider_id(provider) } else { provider }, id, variant))
        }
        _ => {
            invalid(path, diagnostics);
            None
        }
    }
}

fn rename_model_provider(encoded: Value) -> Value {
    let Value::Object(mut map) = encoded else { return encoded };
    let Some(Value::String(provider)) = map.get("providerID").cloned() else { return Value::Object(map) };
    map.insert("providerID".to_owned(), Value::String(provider_id(&provider).to_owned()));
    Value::Object(map)
}

fn model_selection_from_string(input: &str) -> Option<Value> {
    let provider_end = input.find('/')?;
    if provider_end == 0 {
        return None;
    }
    let provider = input.get(..provider_end)?;
    if provider.contains('#') {
        return None;
    }
    let rest = input.get(provider_end + 1..)?;
    let (id, variant) = match rest.find('#') {
        Some(index) => (rest.get(..index)?, Some(rest.get(index + 1..)?)),
        None => (rest, None),
    };
    if id.is_empty() {
        return None;
    }
    if let Some(variant) = variant
        && (variant.is_empty() || variant.contains('#'))
    {
        return None;
    }
    Some(model_object(provider, id, variant))
}

fn model_object(provider: &str, id: &str, variant: Option<&str>) -> Value {
    let mut pairs = vec![
        ("providerID".to_owned(), Value::String(provider.to_owned())),
        ("model".to_owned(), Value::String(id.to_owned())),
    ];
    if let Some(variant) = variant {
        pairs.push(("variant".to_owned(), Value::String(variant.to_owned())));
    }
    object(pairs)
}

fn rewrite_model_field(value: Value) -> Value {
    let Value::Object(mut map) = value else { return value };
    let parsed = match map.get("model") {
        Some(Value::String(text)) => model_selection_from_string(text),
        _ => None,
    };
    if let Some(parsed) = parsed {
        map.insert("model".to_owned(), parsed);
    }
    Value::Object(map)
}

fn with_default_model(agents: &Map<String, Value>, model: &Value) -> Map<String, Value> {
    let mut title = Map::new();
    title.insert("model".to_owned(), model.clone());
    if let Some(Value::Object(existing)) = agents.get("title") {
        for (key, value) in existing {
            title.insert(key.clone(), value.clone());
        }
    }
    let mut agents = agents.clone();
    agents.insert("title".to_owned(), Value::Object(reorder_map(title)));
    agents
}

fn decode_plugin_spec(value: &Value, path: &[String], diagnostics: &mut Vec<Diagnostic>) -> Option<Value> {
    match value {
        Value::String(_) => Some(value.clone()),
        Value::Array(items) => {
            let [package, options] = items.as_slice() else {
                invalid(path, diagnostics);
                return None;
            };
            let (Value::String(package), Value::Object(options)) = (package, options) else {
                invalid(path, diagnostics);
                return None;
            };
            Some(object(vec![
                ("package".to_owned(), Value::String(package.clone())),
                ("options".to_owned(), Value::Object(options.clone())),
            ]))
        }
        _ => {
            invalid(path, diagnostics);
            None
        }
    }
}

fn decode_legacy_autoupdate(value: &Value, path: &[String], diagnostics: &mut Vec<Diagnostic>) -> Option<Value> {
    match value {
        Value::Bool(_) => Some(value.clone()),
        Value::String(text) if text == "notify" => Some(value.clone()),
        _ => {
            invalid(path, diagnostics);
            None
        }
    }
}

fn provider_id(input: &str) -> &str {
    match input {
        "azure-cognitive-services" => "azure",
        "google-vertex-anthropic" => "google-vertex",
        _ => input,
    }
}

fn migrate_commands(value: Option<&Value>, diagnostics: &mut Vec<Diagnostic>) -> Vec<(String, Value)> {
    migrate_entries(value, "command", diagnostics, v1::migrate_command, v1::decode_command)
}

fn migrate_agents(value: Option<&Value>, diagnostics: &mut Vec<Diagnostic>) -> Vec<(String, Value)> {
    migrate_entries(value, "agent", diagnostics, v1::migrate_agent, v1::decode_agent)
}

fn migrate_mode_agents(value: Option<&Value>, diagnostics: &mut Vec<Diagnostic>) -> Vec<(String, Value)> {
    migrate_entries(value, "mode", diagnostics, v1::migrate_agent, |raw| {
        let mut agent = v1::decode_agent(raw)?;
        if let Value::Object(map) = &mut agent {
            map.insert("mode".to_owned(), Value::String("primary".to_owned()));
        }
        Some(agent)
    })
}

fn migrate_entries(
    value: Option<&Value>,
    key: &str,
    diagnostics: &mut Vec<Diagnostic>,
    migrate: fn(&Value) -> Value,
    decode: fn(&Value) -> Option<Value>,
) -> Vec<(String, Value)> {
    let Some(value) = value else { return Vec::new() };
    let Some(entries) = value.as_object() else {
        invalid(&[key.to_owned()], diagnostics);
        return Vec::new();
    };
    let mut result = Vec::new();
    for (name, raw) in entries {
        match decode(raw) {
            Some(decoded) => result.push((name.clone(), migrate(&decoded))),
            None => invalid(&push_path(&[key.to_owned()], name), diagnostics),
        }
    }
    result
}

fn migrate_small_model(value: Option<&Value>, _diagnostics: &mut Vec<Diagnostic>) -> Option<Value> {
    v1::model_selection(value?.as_str(), None)
}

fn migrate_providers(value: Option<&Value>, diagnostics: &mut Vec<Diagnostic>) -> Vec<(String, Value)> {
    v1::migrate_providers(value, diagnostics)
}

fn migrate_tools(value: Option<&Value>, diagnostics: &mut Vec<Diagnostic>) -> Vec<Value> {
    v1::migrate_tools(value, diagnostics)
}

fn migrate_permissions(value: Option<&Value>, diagnostics: &mut Vec<Diagnostic>) -> Vec<Value> {
    v1::migrate_permissions(value, diagnostics)
}

fn migrate_legacy_mcp(value: &Value, path: &[String], diagnostics: &mut Vec<Diagnostic>) -> Option<Value> {
    let Some(info) = v1::decode_mcp(value) else {
        invalid(path, diagnostics);
        return None;
    };
    let Some(server) = v1::migrate_mcp(&info) else {
        invalid(path, diagnostics);
        return None;
    };
    Some(server)
}

fn diagnose_selection_map(value: Option<&Value>, path: &[String], diagnostics: &mut Vec<Diagnostic>) {
    v1::diagnose_selection_map(value, path, diagnostics);
}

fn diagnose_agent_unsupported(value: Option<&Value>, path: &[String], diagnostics: &mut Vec<Diagnostic>) {
    v1::diagnose_agent_unsupported(value, path, diagnostics);
}

fn is_non_empty_string(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::String(text)) if !text.is_empty())
}

fn is_enabled_only_mcp(value: &Value) -> bool {
    let Value::Object(map) = value else { return false };
    !own(map, "type") && matches!(map.get("enabled"), Some(Value::Bool(_)))
}

fn is_direct_legacy_mcp(value: &Value) -> bool {
    let Value::Object(map) = value else { return false };
    matches!(map.get("type").and_then(Value::as_str), Some("local" | "remote"))
}

fn rename_tools(value: &Value) -> Value {
    let Value::Array(items) = value else { return value.clone() };
    Value::Array(
        items
            .iter()
            .map(|item| {
                let Value::Object(entry) = item else { return item.clone() };
                let Some(Value::String(action)) = entry.get("action") else { return item.clone() };
                let mut renamed = entry.clone();
                renamed.insert("action".to_owned(), Value::String(v1::renamed_tool(action).to_owned()));
                Value::Object(renamed)
            })
            .collect(),
    )
}

fn rename_agent_permissions(value: &Value) -> Value {
    let Value::Object(agents) = value else { return value.clone() };
    Value::Object(
        agents
            .iter()
            .map(|(name, agent)| {
                let Value::Object(entry) = agent else { return (name.clone(), agent.clone()) };
                let Some(permissions) = entry.get("permissions") else { return (name.clone(), agent.clone()) };
                let mut renamed = entry.clone();
                renamed.insert("permissions".to_owned(), rename_tools(permissions));
                (name.clone(), Value::Object(renamed))
            })
            .collect(),
    )
}

fn decode_map(
    value: Option<&Value>,
    path: &[String],
    diagnostics: &mut Vec<Diagnostic>,
    decode: fn(&Value, &[String], &mut Vec<Diagnostic>) -> Option<Value>,
) -> Map<String, Value> {
    let Some(value) = value else { return Map::new() };
    let Value::Object(entries) = value else {
        invalid(path, diagnostics);
        return Map::new();
    };
    let mut result = Map::new();
    for (name, raw) in entries {
        let item_path = push_path(path, name);
        if let Some(decoded) = decode(raw, &item_path, diagnostics) {
            result.insert(name.clone(), decoded);
        }
    }
    reorder_map(result)
}

fn decode_list(
    value: Option<&Value>,
    path: &[String],
    diagnostics: &mut Vec<Diagnostic>,
    decode: fn(&Value, &[String], &mut Vec<Diagnostic>) -> Option<Value>,
) -> Vec<Value> {
    let Some(value) = value else { return Vec::new() };
    let Value::Array(items) = value else {
        invalid(path, diagnostics);
        return Vec::new();
    };
    let mut result = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let item_path = push_path(path, &index.to_string());
        if let Some(decoded) = decode(item, &item_path, diagnostics) {
            result.push(decoded);
        }
    }
    result
}

fn decode_encoded<T: Decode + Encode>(
    value: &Value,
    path: &[String],
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Value> {
    let decoded = match T::decode(value, &mut opencode_contract::codec::Path::new()) {
        Ok(decoded) => decoded,
        Err(_) => {
            invalid(path, diagnostics);
            return None;
        }
    };
    let text = encode_json(&decoded);
    match serde_json::from_str::<Value>(&text) {
        Ok(mut encoded) => {
            reorder_value(&mut encoded);
            Some(encoded)
        }
        Err(_) => {
            invalid(path, diagnostics);
            None
        }
    }
}

fn decode_one_key<T: Decode + Encode>(
    key: &str,
    value: &Value,
    path: &[String],
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Value> {
    let mut probe = Map::new();
    probe.insert(key.to_owned(), value.clone());
    let mut encoded = decode_encoded::<T>(&Value::Object(probe), path, diagnostics)?;
    match &mut encoded {
        Value::Object(map) => map.shift_remove(key),
        _ => None,
    }
}

fn canonical<T: Decode + Encode>(value: &Value) -> Option<Value> {
    let decoded = T::decode(value, &mut opencode_contract::codec::Path::new()).ok()?;
    let text = encode_json(&decoded);
    let mut encoded = serde_json::from_str::<Value>(&text).ok()?;
    reorder_value(&mut encoded);
    Some(encoded)
}

fn map_values(
    input: &Map<String, Value>,
    path: &[String],
    diagnostics: &mut Vec<Diagnostic>,
    map: fn(&Value) -> Option<Value>,
) -> Map<String, Value> {
    let mut result = Map::new();
    for (key, value) in input {
        let Some(mapped) = map(value) else {
            invalid(&push_path(path, key), diagnostics);
            continue;
        };
        result.insert(key.clone(), mapped);
    }
    reorder_map(result)
}

fn map_from_pairs(pairs: Vec<(String, Value)>) -> Map<String, Value> {
    reorder_map(pairs.into_iter().collect())
}

fn merge_map(
    target: &mut Map<String, Value>,
    key: &str,
    legacy: &Map<String, Value>,
    native: &Map<String, Value>,
    present: bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let merged = merge_maps(legacy, native, &[key.to_owned()], diagnostics);
    if present {
        target.insert(key.to_owned(), Value::Object(merged));
    }
}

fn merge_maps(
    legacy: &Map<String, Value>,
    native: &Map<String, Value>,
    path: &[String],
    diagnostics: &mut Vec<Diagnostic>,
) -> Map<String, Value> {
    let mut result = legacy.clone();
    for (name, value) in native {
        let differs = result.get(name).is_some_and(|existing| existing != value);
        if differs {
            conflict(&push_path(path, name), diagnostics);
        }
        result.insert(name.clone(), value.clone());
    }
    reorder_map(result)
}

fn overlay(
    target: &mut Map<String, Value>,
    key: &str,
    value: Value,
    path: &[String],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let differs = target.get(key).is_some_and(|existing| *existing != value);
    if differs {
        conflict(path, diagnostics);
    }
    target.insert(key.to_owned(), value);
}

fn prefer(
    legacy: Option<Value>,
    native: Option<Value>,
    path: &[String],
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Value> {
    let Some(native) = native else { return legacy };
    if let Some(legacy) = &legacy
        && *legacy != native
    {
        conflict(path, diagnostics);
    }
    Some(native)
}

fn unsupported_if_present(value: &Map<String, Value>, key: &str, path: &[String], diagnostics: &mut Vec<Diagnostic>) {
    if !own(value, key) {
        return;
    }
    diagnostics.push(Diagnostic {
        kind: DiagnosticKind::Unsupported,
        path: path.to_vec(),
        message: UNSUPPORTED_MESSAGE.to_owned(),
    });
}

fn invalid(path: &[String], diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.push(Diagnostic {
        kind: DiagnosticKind::Invalid,
        path: path.to_vec(),
        message: INVALID_MESSAGE.to_owned(),
    });
}

fn conflict(path: &[String], diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.push(Diagnostic {
        kind: DiagnosticKind::Conflict,
        path: path.to_vec(),
        message: CONFLICT_MESSAGE.to_owned(),
    });
}

fn own(value: &Map<String, Value>, key: &str) -> bool {
    value.contains_key(key)
}

fn is_object(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Object(_)))
}

fn is_array(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Array(_)))
}

fn push_path(path: &[String], segment: &str) -> Vec<String> {
    let mut next = path.to_vec();
    next.push(segment.to_owned());
    next
}

fn object(pairs: Vec<(String, Value)>) -> Value {
    Value::Object(reorder_map(pairs.into_iter().collect()))
}

fn index_key(key: &str) -> Option<u32> {
    let value = key.parse::<u32>().ok()?;
    (value != u32::MAX && value.to_string() == key).then_some(value)
}

fn reorder_map(map: Map<String, Value>) -> Map<String, Value> {
    let mut entries: Vec<(usize, String, Value)> =
        map.into_iter().enumerate().map(|(index, (key, value))| (index, key, value)).collect();
    entries.sort_by_key(|(index, key, _)| match index_key(key) {
        Some(value) => (0u8, value, *index),
        None => (1u8, 0, *index),
    });
    entries.into_iter().map(|(_, key, value)| (key, value)).collect()
}

fn reorder_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for item in map.values_mut() {
                reorder_value(item);
            }
            let taken = std::mem::take(map);
            *map = reorder_map(taken);
        }
        Value::Array(items) => {
            for item in items {
                reorder_value(item);
            }
        }
        _ => {}
    }
}

enum DurationValue {
    Millis(f64),
    Nanos(i128),
    Infinity,
    NegativeInfinity,
}

fn duration_string(input: &str) -> Option<String> {
    match input {
        "Infinity" => return Some("Infinity".to_owned()),
        "-Infinity" => return Some("-Infinity".to_owned()),
        _ => {}
    }
    let (value, unit) = split_duration(input)?;
    let parsed = match unit {
        "nano" | "nanos" => nanos_value(parse_nanos(value, 1)?),
        "micro" | "micros" => nanos_value(parse_nanos(value, 1_000)?),
        "milli" | "millis" => from_millis_string(value, 1)?,
        "second" | "seconds" => from_millis_string(value, 1_000)?,
        "minute" | "minutes" => from_millis_string(value, 60_000)?,
        "hour" | "hours" => from_millis_string(value, 3_600_000)?,
        "day" | "days" => from_millis_string(value, 86_400_000)?,
        "week" | "weeks" => from_millis_string(value, 604_800_000)?,
        _ => return None,
    };
    Some(format_duration(parsed))
}

fn split_duration(input: &str) -> Option<(&str, &str)> {
    let whitespace = input.find(is_js_whitespace)?;
    let value = input.get(..whitespace)?;
    if !is_duration_number(value) {
        return None;
    }
    let unit = input.get(whitespace..)?.trim_start_matches(is_js_whitespace);
    if unit.is_empty() || unit.chars().any(is_js_whitespace) {
        return None;
    }
    Some((value, unit))
}

fn is_js_whitespace(value: char) -> bool {
    matches!(
        value,
        '\u{0009}'
            | '\u{000a}'
            | '\u{000b}'
            | '\u{000c}'
            | '\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    ) || ('\u{2000}'..='\u{200a}').contains(&value)
}

fn is_duration_number(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    let (integer, fraction) = match digits.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (digits, None),
    };
    if integer.is_empty() || !integer.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    match fraction {
        Some(fraction) => !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit()),
        None => true,
    }
}

fn parse_nanos(input: &str, scale: i128) -> Option<i128> {
    let (negative, body) = match input.strip_prefix('-') {
        Some(body) => (true, body),
        None => (false, input),
    };
    let Some(dot) = body.find('.') else {
        let value = body.parse::<i128>().ok()?;
        let signed = if negative { value.checked_neg()? } else { value };
        return signed.checked_mul(scale);
    };
    let integer = body.get(..dot)?.parse::<i128>().ok()?;
    let fraction = body.get(dot + 1..)?.parse::<i128>().ok()?;
    let fractional_scale = pow10(body.len() - dot - 1)?;
    let scaled_integer = integer.checked_mul(fractional_scale)?;
    let combined = scaled_integer.checked_add(fraction)?;
    let scaled = combined.checked_mul(scale)?;
    let signed = if negative { scaled.checked_neg()? } else { scaled };
    divide_ties_away_from_zero(signed, fractional_scale)
}

fn pow10(exponent: usize) -> Option<i128> {
    let mut value: i128 = 1;
    for _ in 0..exponent {
        value = value.checked_mul(10)?;
    }
    Some(value)
}

fn divide_ties_away_from_zero(numerator: i128, denominator: i128) -> Option<i128> {
    let absolute = numerator.checked_abs()?;
    let remainder = absolute % denominator;
    let rounded = absolute / denominator + if remainder.checked_mul(2)? >= denominator { 1 } else { 0 };
    Some(if numerator < 0 { -rounded } else { rounded })
}

fn from_millis_string(input: &str, scale: i128) -> Option<DurationValue> {
    let millis = input.parse::<f64>().ok()? * scale as f64;
    if !millis.is_finite() {
        return Some(if millis > 0.0 { DurationValue::Infinity } else { DurationValue::NegativeInfinity });
    }
    let nanos = parse_nanos(input, scale.checked_mul(1_000_000)?)?;
    match exact_integer(millis) {
        Some(integer) if integer.checked_mul(1_000_000) == Some(nanos) => {
            Some(DurationValue::Millis(if millis == 0.0 { 0.0 } else { millis }))
        }
        _ => Some(nanos_value(nanos)),
    }
}

fn exact_integer(value: f64) -> Option<i128> {
    if !value.is_finite() || value.fract() != 0.0 {
        return None;
    }
    let converted = value as i128;
    if converted as f64 == value { Some(converted) } else { None }
}

fn nanos_value(nanos: i128) -> DurationValue {
    if nanos == 0 { DurationValue::Millis(0.0) } else { DurationValue::Nanos(nanos) }
}

fn format_duration(value: DurationValue) -> String {
    match value {
        DurationValue::Millis(millis) => format!("{} millis", opencode_api_types::js::number_to_string(millis)),
        DurationValue::Nanos(nanos) => format!("{nanos} nanos"),
        DurationValue::Infinity => "Infinity".to_owned(),
        DurationValue::NegativeInfinity => "-Infinity".to_owned(),
    }
}
