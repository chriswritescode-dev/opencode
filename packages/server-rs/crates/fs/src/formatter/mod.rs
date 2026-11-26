pub mod builtins;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, RwLock};

use futures_util::future::BoxFuture;
use opencode_config::entry::Entry;
use opencode_config::observer::ObserverHandle;
use opencode_config::service::ConfigService;
use opencode_contract::generated::{ConfigEntryDocumentInfoFormatter, ConfigEntryDocumentInfoFormatterObjectValue};

use crate::access::Target;
use crate::write::FormatHook;

pub struct Formatter {
    context: Arc<builtins::Context>,
    formatters: Arc<RwLock<Vec<Arc<builtins::Info>>>>,
    observer: Mutex<Option<ObserverHandle>>,
}

impl Formatter {
    pub fn new(
        directory: impl Into<PathBuf>,
        worktree: impl Into<PathBuf>,
        bin: impl Into<PathBuf>,
        vars: impl Fn(&str) -> Option<OsString> + Send + Sync + 'static,
    ) -> Self {
        Self {
            context: Arc::new(builtins::Context {
                directory: directory.into(),
                worktree: worktree.into(),
                bin: bin.into(),
                vars: Arc::new(vars),
            }),
            formatters: Arc::new(RwLock::new(Vec::new())),
            observer: Mutex::new(None),
        }
    }

    pub fn configure(&self, entries: &[Entry]) {
        *self.formatters.write().expect("the formatter lock is not poisoned") = resolve(&self.context, entries);
    }

    pub fn observe_config(&self, config: &ConfigService) {
        self.configure(&config.entries());
        let context = self.context.clone();
        let formatters = self.formatters.clone();
        let observer = opencode_config::observer::observe(config, move |entries| {
            *formatters.write().expect("the formatter lock is not poisoned") = resolve(&context, entries);
        });
        *self.observer.lock().expect("the formatter observer is not poisoned") = Some(observer);
    }

    pub async fn file(&self, target: &Target) -> bool {
        let extension = extension(&target.absolute);
        let matching: Vec<Arc<builtins::Info>> = self
            .formatters
            .read()
            .expect("the formatter lock is not poisoned")
            .iter()
            .filter(|formatter| formatter.extensions.contains(&extension))
            .cloned()
            .collect();

        for formatter in matching {
            let Some(command) = formatter.command().await else { continue };
            let Some(program) = command.first() else { continue };
            let args: Vec<String> =
                command.iter().map(|argument| argument.replace("$FILE", &target.absolute)).collect();
            log::info!("formatting file {} with {}", target.absolute, args.join(" "));
            let status = tokio::process::Command::new(program)
                .args(&args[1..])
                .current_dir(&self.context.directory)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .envs(formatter.environment.iter().map(|(key, value)| (key, value)))
                .status()
                .await;
            match status {
                Ok(status) if status.success() => return true,
                Ok(status) => log::error!("formatter exited unsuccessfully: {} ({status})", args.join(" ")),
                Err(error) => log::error!("failed to format file {}: {error}", target.absolute),
            }
        }
        false
    }
}

impl FormatHook for Formatter {
    fn file<'a>(&'a self, target: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(Formatter::file(self, target))
    }
}

enum Configured {
    Bool(bool),
    Object(Vec<(String, Partial)>),
}

#[derive(Default)]
struct Partial {
    disabled: Option<bool>,
    command: Option<Vec<String>>,
    environment: Vec<(String, String)>,
    extensions: Option<Vec<String>>,
}

fn resolve(context: &Arc<builtins::Context>, entries: &[Entry]) -> Vec<Arc<builtins::Info>> {
    let Some(configured) = resolve_entries(entries) else { return Vec::new() };
    let builtins: Vec<Arc<builtins::Info>> = builtins::make(context.clone()).into_iter().map(Arc::new).collect();
    match configured {
        Configured::Bool(false) => Vec::new(),
        Configured::Bool(true) => builtins,
        Configured::Object(items) => {
            let mut formatters = builtins;
            for (name, entry) in items {
                if entry.disabled == Some(true) {
                    formatters.retain(|formatter| formatter.name != name);
                    continue;
                }
                let builtin = formatters.iter().find(|formatter| formatter.name == name).cloned();
                let extensions = entry
                    .extensions
                    .unwrap_or_else(|| builtin.as_ref().map_or_else(Vec::new, |builtin| builtin.extensions.clone()));
                let environment = merge_environment(builtin.as_deref(), entry.environment);
                let check = match (&builtin, entry.command) {
                    (Some(builtin), None) => builtin.check.clone(),
                    (_, command) => constant(command),
                };
                let formatter = Arc::new(builtins::Info::new(name.clone(), extensions, environment, check));
                match formatters.iter().position(|item| item.name == name) {
                    Some(index) => formatters[index] = formatter,
                    None => formatters.push(formatter),
                }
            }
            formatters
        }
    }
}

fn resolve_entries(entries: &[Entry]) -> Option<Configured> {
    let mut result: Option<Configured> = None;
    for entry in entries {
        let Entry::Document(document) = entry else { continue };
        let Some(formatter) = &document.info.formatter else { continue };
        match formatter {
            ConfigEntryDocumentInfoFormatter::Bool(value) => result = Some(Configured::Bool(*value)),
            ConfigEntryDocumentInfoFormatter::Object(map) => {
                let mut merged = match result.take() {
                    Some(Configured::Object(items)) => items,
                    _ => Vec::new(),
                };
                for (name, item) in &map.0 {
                    merge(&mut merged, name, item);
                }
                result = Some(Configured::Object(merged));
            }
        }
    }
    result
}

fn merge(merged: &mut Vec<(String, Partial)>, name: &str, item: &ConfigEntryDocumentInfoFormatterObjectValue) {
    if !merged.iter().any(|(key, _)| key == name) {
        merged.push((name.to_owned(), Partial::default()));
    }
    let Some((_, target)) = merged.iter_mut().find(|(key, _)| key == name) else { return };
    if let Some(disabled) = item.disabled {
        target.disabled = Some(disabled);
    }
    if let Some(command) = &item.command {
        target.command = Some(command.clone());
    }
    if let Some(extensions) = &item.extensions {
        target.extensions = Some(extensions.clone());
    }
    if let Some(environment) = &item.environment {
        for (key, value) in &environment.0 {
            match target.environment.iter_mut().find(|(existing, _)| existing == key) {
                Some((_, slot)) => *slot = value.clone(),
                None => target.environment.push((key.clone(), value.clone())),
            }
        }
    }
}

fn merge_environment(builtin: Option<&builtins::Info>, overrides: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut merged = builtin.map_or_else(Vec::new, |builtin| builtin.environment.clone());
    for (key, value) in overrides {
        match merged.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, slot)) => *slot = value,
            None => merged.push((key, value)),
        }
    }
    merged
}

fn constant(command: Option<Vec<String>>) -> builtins::Check {
    Arc::new(move || {
        let command = command.clone();
        Box::pin(async move { command })
    })
}

fn extension(path: &str) -> String {
    match std::path::Path::new(path).extension() {
        Some(extension) if !extension.is_empty() => format!(".{}", extension.to_string_lossy()),
        _ => String::new(),
    }
}
