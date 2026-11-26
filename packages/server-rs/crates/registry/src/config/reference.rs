use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use opencode_config::entry::Entry;
use opencode_config::observer::{self, ObserverHandle};
use opencode_config::service::ConfigService;
use opencode_contract::generated::ConfigEntryDocumentInfoReferencesValue;
use opencode_location::state::Registration;
use opencode_util::path::Flavor;
use opencode_vcs::repository::is_js_whitespace;

use crate::reference::{GitSource, LocalSource, ReferenceEditor, ReferenceService, Source};

#[derive(Clone, Debug)]
pub struct ReferenceEnvironment {
    pub directory: PathBuf,
    pub home: PathBuf,
}

pub fn valid_alias(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| c == '/' || c == '`' || c == ',' || is_js_whitespace(c))
}

pub fn apply(entries: &[Entry], environment: &ReferenceEnvironment, editor: &mut ReferenceEditor<'_>) {
    let home = environment.home.to_string_lossy();
    for entry in entries {
        let Entry::Document(document) = entry else {
            continue;
        };
        let Some(references) = document.info.references.as_ref() else {
            continue;
        };
        let directory = directory(document.path.as_deref(), environment);
        for (name, entry) in &references.0 {
            if !valid_alias(name) {
                continue;
            }
            editor.add(name.clone(), source(entry, &directory, &home));
        }
    }
}

pub struct ConfigReferenceHandle {
    _registration: Registration,
    _observer: ObserverHandle,
}

pub async fn register(
    config: &ConfigService,
    references: &ReferenceService,
    environment: ReferenceEnvironment,
) -> ConfigReferenceHandle {
    let loaded = Arc::new(RwLock::new(Vec::new()));
    let observer = observer::observe(config, {
        let loaded = loaded.clone();
        let references = references.clone();
        move |entries| {
            *loaded.write().expect("the config reference lock is never poisoned") = entries.to_vec();
            let references = references.clone();
            tokio::spawn(async move { references.reload().await });
        }
    });
    *loaded.write().expect("the config reference lock is never poisoned") = config.entries();
    let registration = references
        .transform({
            let loaded = loaded.clone();
            move |editor| {
                let entries = loaded.read().expect("the config reference lock is never poisoned");
                apply(&entries, &environment, editor);
            }
        })
        .await;
    ConfigReferenceHandle { _registration: registration, _observer: observer }
}

fn directory(path: Option<&Path>, environment: &ReferenceEnvironment) -> String {
    match path.and_then(Path::parent) {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
        Some(_) => ".".to_string(),
        None => environment.directory.to_string_lossy().into_owned(),
    }
}

fn source(entry: &ConfigEntryDocumentInfoReferencesValue, directory: &str, home: &str) -> Source {
    match entry {
        ConfigEntryDocumentInfoReferencesValue::String(value) if is_local_string(value) => {
            Source::Local(LocalSource { path: local_path(directory, home, value), description: None, hidden: None })
        }
        ConfigEntryDocumentInfoReferencesValue::String(value) => {
            Source::Git(GitSource { repository: value.clone(), branch: None, description: None, hidden: None })
        }
        ConfigEntryDocumentInfoReferencesValue::Object(git) => Source::Git(GitSource {
            repository: git.repository.clone(),
            branch: git.branch.clone(),
            description: git.description.clone(),
            hidden: git.hidden,
        }),
        ConfigEntryDocumentInfoReferencesValue::Object2(local) => Source::Local(LocalSource {
            path: local_path(directory, home, &local.path),
            description: local.description.clone(),
            hidden: local.hidden,
        }),
    }
}

fn is_local_string(value: &str) -> bool {
    value.starts_with('.') || value.starts_with('/') || value.starts_with('~')
}

fn local_path(directory: &str, home: &str, value: &str) -> String {
    if let Some(rest) = value.strip_prefix("~/") {
        return Flavor::NATIVE.join(&[home, rest]);
    }
    if Flavor::NATIVE.is_absolute(value) {
        return value.to_string();
    }
    Flavor::NATIVE.resolve(&[directory, value])
}
