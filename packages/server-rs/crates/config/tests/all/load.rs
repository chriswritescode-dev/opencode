use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use opencode_config::discovery::{Sources, discover};
use opencode_config::entry::{Document, Origin};
use opencode_config::load::{FlagInputs, LoadDeps, load};
use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::paths::GlobalPaths;
use opencode_config::policy;
use opencode_config::seams::{
    BoxFuture, CredentialLookup, CredentialValue, NoKv, WellKnownEntry, WellKnownError, WellKnownSource,
};
use opencode_contract::codec::{Decode, Encode, decode_json, encode_json};
use opencode_contract::generated::{ConfigEntryDocumentInfo, ConfigEntryDocumentInfoPoliciesItemAction};
use opencode_util::temp::TempDir;
use serde_json::{Map, Value, json};

use crate::support::{area, entries_view, relative};

#[tokio::test]
async fn every_load_case_matches_the_pinned_typescript() {
    let cases = area("load");
    assert_eq!(cases.len(), 28, "expected 28 load cases, found {}", cases.len());
    for case in &cases {
        verify(case).await;
    }
}

async fn verify(case: &crate::support::Case) {
    let temp = TempDir::new().unwrap();
    let root = opencode_util::path::realpath(temp.path()).unwrap();

    for directory in case.value["directories"].as_array().cloned().unwrap_or_default() {
        std::fs::create_dir_all(root.join(directory.as_str().unwrap())).unwrap();
    }
    for (path, content) in case.value["files"].as_object().cloned().unwrap_or_default() {
        let target = root.join(&path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&target, content.as_str().unwrap()).unwrap();
    }

    let global = &case.value["global"];
    let home = root.join(global["home"].as_str().unwrap());
    let config_directory = root.join(global["config"].as_str().unwrap());
    let paths = GlobalPaths::from_vars(|name| match name {
        "HOME" => Some(home.clone().into_os_string()),
        "OPENCODE_CONFIG_DIR" => Some(config_directory.clone().into_os_string()),
        _ => None,
    })
    .unwrap();

    let options = config_options(&root, &case.value["options"]);
    let location_value = &case.value["location"];
    let location = ConfigLocation {
        directory: root.join(location_value["directory"].as_str().unwrap()),
        project_id: location_value["project_id"].as_str().unwrap().to_owned(),
    };
    let sources = discover(&options, &location, &paths);

    let flags_value = &case.value["flags"];
    let flags = FlagInputs {
        channel: flags_value.get("channel").and_then(Value::as_str).unwrap_or("unknown").to_owned(),
        overrides: flags_value.get("overrides").and_then(Value::as_str).map(str::to_owned),
    };

    let credentials = corpus_credentials(&case.value["wellknown"]);
    let wellknown = corpus_wellknown(&case.value["wellknown"]);
    let integrations: Vec<Document> =
        case.value["integrations"].as_array().cloned().unwrap_or_default().iter().map(integration_document).collect();
    let deps = LoadDeps {
        credentials: &credentials,
        wellknown: &wellknown,
        kv: &NoKv,
        integrations: &integrations,
        home: &home,
    };

    let loaded = load(&sources, &options, &location, &deps, flags).await;

    assert_eq!(json!(entries_view(&root, &loaded.entries)), case.value["entries"], "{} entries", case.name);
    assert_eq!(
        json!(compatibility_view(&root, &loaded.sources)),
        case.value["compatibility"],
        "{} compatibility",
        case.name
    );

    let statements = policy::statements(&loaded.entries);
    let statement_values: Vec<Value> = statements
        .iter()
        .map(|statement| {
            json!({
                "action": encode_value(&statement.action),
                "resource": statement.resource.as_str(),
                "effect": encode_value(&statement.effect),
                "message": statement.message.as_str(),
            })
        })
        .collect();
    assert_eq!(json!(statement_values), case.value["statements"], "{} statements", case.name);

    let decisions: Vec<Value> = case.value["policy"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            let action = ConfigEntryDocumentInfoPoliciesItemAction::decode(
                &Value::String(item["action"].as_str().unwrap().to_owned()),
                &mut opencode_contract::codec::Path::new(),
            )
            .unwrap();
            let resource = item["resource"].as_str().unwrap();
            json!({
                "action": encode_value(&action),
                "resource": resource,
                "effect": encode_value(&policy::decision(&statements, action, resource)),
            })
        })
        .collect();
    assert_eq!(json!(decisions), case.value["decisions"], "{} decisions", case.name);
}

struct CorpusCredentials {
    credentials: Vec<(String, CredentialValue)>,
    get: Map<String, Value>,
}

impl CredentialLookup for CorpusCredentials {
    fn latest(&self, integration_id: &str) -> Option<CredentialValue> {
        self.credentials.iter().rev().find(|(candidate, _)| candidate == integration_id).map(|(_, value)| value.clone())
    }

    fn get(&self, credential_id: &str) -> Option<CredentialValue> {
        self.get.get(credential_id).map(credential_value)
    }
}

struct CorpusWellKnown {
    entries: Vec<WellKnownEntry>,
    resolve: Option<Vec<Value>>,
    resolve_error: bool,
    cached: Vec<Value>,
}

impl WellKnownSource for CorpusWellKnown {
    fn entries<'a>(&'a self) -> BoxFuture<'a, Vec<WellKnownEntry>> {
        Box::pin(async move { self.entries.clone() })
    }

    fn resolve<'a>(
        &'a self,
        _entry: &'a WellKnownEntry,
        _variables: &'a BTreeMap<String, String>,
    ) -> BoxFuture<'a, Result<Vec<Value>, WellKnownError>> {
        Box::pin(async move {
            if self.resolve_error {
                let error: WellKnownError = Box::new(std::io::Error::other("resolve failed"));
                return Err(error);
            }
            Ok(self.resolve.clone().unwrap_or_default())
        })
    }

    fn cached<'a>(&'a self, _origin: &'a str) -> BoxFuture<'a, Vec<Value>> {
        Box::pin(async move { self.cached.clone() })
    }

    fn clear_cached<'a>(&'a self, _origin: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

fn corpus_credentials(wellknown: &Value) -> CorpusCredentials {
    let credentials = wellknown["credentials"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|credential| (credential["integrationID"].as_str().unwrap().to_owned(), credential_value(credential)))
        .collect();
    CorpusCredentials { credentials, get: wellknown["get"].as_object().cloned().unwrap_or_default() }
}

fn corpus_wellknown(wellknown: &Value) -> CorpusWellKnown {
    let entries = wellknown["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| WellKnownEntry {
            origin: entry["origin"].as_str().unwrap().to_owned(),
            integration_id: entry["integrationID"].as_str().unwrap().to_owned(),
            manifest: entry["manifest"].clone(),
        })
        .collect();
    CorpusWellKnown {
        entries,
        resolve: wellknown["resolve"].as_array().cloned(),
        resolve_error: wellknown["resolveError"].as_bool().unwrap_or(false),
        cached: wellknown["cached"].as_array().cloned().unwrap_or_default(),
    }
}

fn credential_value(value: &Value) -> CredentialValue {
    CredentialValue {
        key: value.get("key").and_then(Value::as_str).map(str::to_owned),
        metadata: value.get("metadata").and_then(Value::as_object).cloned().unwrap_or_default(),
    }
}

fn integration_document(value: &Value) -> Document {
    Document {
        path: None,
        info: decode_json::<ConfigEntryDocumentInfo>(&value["info"].to_string()).unwrap(),
        origin: Origin::Integration {
            integration_id: value["integrationID"].as_str().unwrap().to_owned(),
            connection: value["connection"].clone(),
            organization: value.get("organization").and_then(Value::as_str).map(str::to_owned),
        },
    }
}

fn config_options(root: &Path, value: &Value) -> ConfigOptions {
    let system = match value.get("system") {
        Some(Value::String(path)) => Some(SystemOption::Path(root.join(path))),
        _ => Some(SystemOption::Disabled),
    };
    ConfigOptions {
        project: value.get("project").and_then(Value::as_bool),
        global: value.get("global").and_then(Value::as_bool),
        system,
        claude: value.get("claude").and_then(Value::as_bool),
        file: value.get("file").and_then(Value::as_str).map(|path| root.join(path)),
        content: value.get("content").and_then(Value::as_str).map(str::to_owned),
    }
}

fn compatibility_view(root: &Path, sources: &Sources) -> Value {
    json!({
        "claude": directory_list(root, &sources.claude),
        "agents": directory_list(root, &sources.agents),
    })
}

fn directory_list(root: &Path, paths: &[PathBuf]) -> Vec<String> {
    paths.iter().filter(|path| path.is_dir()).map(|path| relative(root, path)).collect()
}

fn encode_value<T: Encode>(value: &T) -> Value {
    serde_json::from_str(&encode_json(value)).expect("the wire encoding is JSON")
}
