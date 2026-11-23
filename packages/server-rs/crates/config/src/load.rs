use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use std::time::Duration;

use opencode_contract::generated::ConfigEntryDocumentInfo;
use serde_json::Value;

use crate::discovery::{NAMES, Sources};
use crate::entry::{Document, Entry, Origin};
use crate::flag;
use crate::options::{ConfigLocation, ConfigOptions};
use crate::parse::parse_info;
use crate::policy::{self, Action, Effect, Statement};
use crate::seams::{CredentialLookup, KvStore, WellKnownSource};
use crate::variable::{Source, SubstituteInput, substitute};

pub struct FlagInputs {
    pub channel: String,
    pub overrides: Option<String>,
}

pub struct LoadDeps<'a> {
    pub credentials: &'a dyn CredentialLookup,
    pub wellknown: &'a dyn WellKnownSource,
    pub kv: &'a dyn KvStore,
    pub integrations: &'a [Document],
    pub home: &'a Path,
}

pub struct Loaded {
    pub entries: Vec<Entry>,
    pub sources: Sources,
    pub references: BTreeSet<PathBuf>,
}

pub async fn load(
    sources: &Sources,
    options: &ConfigOptions,
    location: &ConfigLocation,
    deps: &LoadDeps<'_>,
    flags: FlagInputs,
) -> Loaded {
    let mut references = BTreeSet::new();

    let direct: Vec<Entry> = sources
        .direct
        .iter()
        .filter_map(|path| load_file(path, deps.home, &mut references))
        .map(Entry::Document)
        .collect();
    let explicit: Vec<Entry> = sources
        .explicit
        .iter()
        .filter_map(|path| load_file(path, deps.home, &mut references))
        .map(Entry::Document)
        .collect();
    let content: Vec<Entry> = options
        .content
        .as_deref()
        .and_then(|content| {
            let text = substitute_virtual(
                "OPENCODE_CONFIG_CONTENT",
                &location.directory,
                content,
                None,
                deps.home,
                &mut references,
            );
            parse_info(&text, "OPENCODE_CONFIG_CONTENT")
                .info
                .map(|info| Entry::Document(Document { path: None, info, origin: Origin::File }))
        })
        .into_iter()
        .collect();

    let system_entries = match &sources.system {
        Some(system) if system.present => load_directory(&system.path, deps.home, &mut references),
        _ => Vec::new(),
    };
    #[cfg(target_os = "macos")]
    let managed = {
        let mut managed = system_entries;
        if let Some(preferences) = &sources.preferences {
            managed.extend(load_preferences(preferences, deps.home, &mut references).await);
        }
        managed
    };
    #[cfg(not(target_os = "macos"))]
    let managed = system_entries;
    let managed_policies: Vec<Entry> = managed
        .iter()
        .rev()
        .filter_map(|entry| match entry {
            Entry::Document(document) if document.info.policies.is_some() => {
                Some(Entry::Document(Document { origin: Origin::Managed, ..document.clone() }.policies_only()))
            }
            _ => None,
        })
        .collect();
    let managed_settings: Vec<Entry> = managed
        .into_iter()
        .map(|entry| match entry {
            Entry::Document(mut document) => {
                document.origin = Origin::Managed;
                document.info.policies = None;
                Entry::Document(document)
            }
            Entry::Directory { path } => Entry::Directory { path },
        })
        .collect();

    let global = match &sources.global {
        Some(global) => load_directory(global, deps.home, &mut references),
        None => Vec::new(),
    };
    let project: Vec<Entry> = sources
        .project
        .iter()
        .filter(|root| root.present)
        .flat_map(|root| load_directory(&root.path, deps.home, &mut references))
        .collect();

    let lockdown = policy::statements(&managed_policies);
    let integrated: Vec<Entry> = deps
        .integrations
        .iter()
        .map(|document| {
            let allowed = match &document.origin {
                Origin::Integration { integration_id, connection, .. } => {
                    supplies(&lockdown, integration_id, Some(connection), deps.credentials)
                }
                _ => true,
            };
            if allowed { Entry::Document(document.clone()) } else { Entry::Document(document.clone().policies_only()) }
        })
        .collect();
    let wellknown = load_wellknown(&lockdown, deps, &mut references).await;

    let below = [managed_policies, integrated, wellknown, global, explicit, direct, project].concat();
    let above = [content, managed_settings].concat();
    let infos: Vec<&ConfigEntryDocumentInfo> = below
        .iter()
        .chain(&above)
        .filter_map(|entry| match entry {
            Entry::Document(document) => Some(&document.info),
            Entry::Directory { .. } => None,
        })
        .collect();
    let overlay_enabled = flag::resolve(&infos, &flags.channel, flags.overrides.as_deref())
        .iter()
        .any(|(key, state)| *key == "project_overlay" && state.enabled);
    let overlay = if overlay_enabled {
        crate::discovery::overlay_in(sources.global.as_deref().unwrap_or(Path::new("")), options, location)
    } else {
        None
    };
    let mut effective = sources.clone();
    if let Some(root) = &overlay {
        effective.project.push(root.clone());
    }

    let mut entries = below;
    if let Some(root) = &overlay
        && root.present
    {
        entries.extend(load_directory(&root.path, deps.home, &mut references));
    }
    entries.extend(above);

    Loaded { entries, sources: effective, references }
}

async fn load_wellknown(lockdown: &[Statement], deps: &LoadDeps<'_>, references: &mut BTreeSet<PathBuf>) -> Vec<Entry> {
    let mut documents = Vec::new();
    for entry in deps.wellknown.entries().await {
        let variables = deps.credentials.latest(&entry.integration_id).and_then(|credential| {
            let auth = entry.auth_env()?;
            let key = credential.key?;
            Some(BTreeMap::from([(auth.to_owned(), key)]))
        });
        let Some(variables) = variables else {
            deps.wellknown.clear_cached(&entry.origin).await;
            continue;
        };
        let configs = match deps.wellknown.resolve(&entry, &variables).await {
            Ok(configs) => configs,
            Err(_) => deps.wellknown.cached(&entry.origin).await,
        };
        let allowed = supplies(lockdown, &entry.integration_id, None, deps.credentials);
        for config in configs {
            let text = serde_json::to_string(&config).expect("a JSON value serializes");
            let substituted = substitute_virtual(
                &entry.origin,
                Path::new(&entry.origin),
                &text,
                Some(&variables),
                deps.home,
                references,
            );
            let Some(info) = parse_info(&substituted, &entry.origin).info else { continue };
            let document = Document {
                path: None,
                info,
                origin: Origin::WellKnown { integration_id: entry.integration_id.clone() },
            };
            documents.push(Entry::Document(if allowed { document } else { document.policies_only() }));
        }
    }
    documents
}

fn supplies(
    lockdown: &[Statement],
    integration_id: &str,
    connection: Option<&Value>,
    credentials: &dyn CredentialLookup,
) -> bool {
    let stored = connection
        .filter(|connection| connection.get("type").and_then(Value::as_str) == Some("credential"))
        .and_then(|connection| connection.get("id").and_then(Value::as_str))
        .and_then(|id| credentials.get(id));
    let account = stored.as_ref().and_then(policy::account);
    policy::decision(
        lockdown,
        Action::IntegrationUse,
        &policy::integration_resource(integration_id, account.as_deref()),
    ) != Effect::Deny
}

fn load_file(path: &Path, home: &Path, references: &mut BTreeSet<PathBuf>) -> Option<Document> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    references.insert(path.to_path_buf());
    let substituted =
        substitute(&SubstituteInput { source: Source::Path(path), text, env: None, home }, references, |name| {
            std::env::var(name).ok()
        });
    let info = parse_info(&substituted, &path.to_string_lossy()).info?;
    Some(Document { path: Some(path.to_path_buf()), info, origin: Origin::File })
}

fn load_directory(directory: &Path, home: &Path, references: &mut BTreeSet<PathBuf>) -> Vec<Entry> {
    let mut entries: Vec<Entry> = NAMES
        .iter()
        .filter_map(|name| load_file(&directory.join(name), home, references))
        .map(Entry::Document)
        .collect();
    entries.push(Entry::Directory { path: directory.to_path_buf() });
    entries
}

fn substitute_virtual(
    source: &str,
    dir: &Path,
    text: &str,
    env: Option<&BTreeMap<String, String>>,
    home: &Path,
    references: &mut BTreeSet<PathBuf>,
) -> String {
    substitute(&SubstituteInput { source: Source::Virtual { source, dir }, text, env, home }, references, |name| {
        std::env::var(name).ok()
    })
}

#[cfg(target_os = "macos")]
async fn load_preferences(plist: &Path, home: &Path, references: &mut BTreeSet<PathBuf>) -> Vec<Entry> {
    let output = tokio::process::Command::new("plutil")
        .args(["-convert", "json", "-o", "-"])
        .arg(plist)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(10), output).await else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    references.insert(plist.to_path_buf());
    let text = String::from_utf8_lossy(&output.stdout);
    let stripped = crate::discovery::strip_preference_metadata(&text);
    let source = plist.to_string_lossy();
    let dir = plist.parent().unwrap_or(Path::new("/"));
    let substituted = substitute_virtual(&source, dir, &stripped, None, home, references);
    let Some(info) = parse_info(&substituted, &source).info else { return Vec::new() };
    vec![Entry::Document(Document { path: Some(plist.to_path_buf()), info, origin: Origin::Managed })]
}
