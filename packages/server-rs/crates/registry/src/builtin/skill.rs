use opencode_config::entry::Entry;
use opencode_contract::generated::{AbsolutePath, ConfigEntryDocumentInfoPluginsItem, SkillID, SkillInfo, SkillName};

use crate::generated;
use crate::skill::SkillEditor;

#[derive(Clone)]
pub struct AppInfo {
    pub version: String,
    pub channel: String,
}

pub const OPENCODE_DESCRIPTION: &str = "Use this skill for any question about OpenCode itself, including how OpenCode works, using or configuring it, migrating from V1 to V2, troubleshooting it, developing plugins or integrations, using the OpenCode SDK, clients, server, or API, and contributing to the OpenCode codebase. Also use it for OpenCode agents, commands, skills, tools, permissions, MCP servers, providers, models, themes, keybinds, formatters, the CLI, TUI, desktop app, and web app.";

pub const REPORT_DESCRIPTION: &str = "Use when the user wants to report an opencode issue or bug. Collect standard diagnostics, add user-specific reproduction context, and publish the issue with GitHub CLI.";

pub fn configured_plugins(entries: &[Entry]) -> Vec<String> {
    let mut plugins: Vec<String> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Document(document) => document.info.plugins.as_deref(),
            Entry::Directory { .. } => None,
        })
        .flatten()
        .map(|plugin| match plugin {
            ConfigEntryDocumentInfoPluginsItem::String(value) => value.clone(),
            ConfigEntryDocumentInfoPluginsItem::Object(value) => value.package.clone(),
        })
        .collect();
    plugins.sort();
    plugins
}

pub fn skill(editor: &mut SkillEditor<'_>, app: &AppInfo, env: &dyn Fn(&str) -> Option<String>, entries: &[Entry]) {
    editor.add(
        SkillInfo {
            id: SkillID("opencode".to_string()),
            name: SkillName("OpenCode".to_string()),
            description: Some(OPENCODE_DESCRIPTION.to_string()),
            autoinvoke: None,
            path: AbsolutePath("skill://opencode/SKILL.md".to_string()),
            content: generated::OPENCODE_SKILL.to_string(),
        },
        None,
    );
    editor.add(
        SkillInfo {
            id: SkillID("report".to_string()),
            name: SkillName("Report".to_string()),
            description: Some(REPORT_DESCRIPTION.to_string()),
            autoinvoke: None,
            path: AbsolutePath("skill://report/SKILL.md".to_string()),
            content: report(app, env, entries),
        },
        None,
    );
}

pub fn report(app: &AppInfo, env: &dyn Fn(&str) -> Option<String>, entries: &[Entry]) -> String {
    let plugins = configured_plugins(entries);
    let active = if plugins.is_empty() { "None found in config".to_string() } else { plugins.join(", ") };
    [
        generated::REPORT_SKILL.to_string(),
        String::new(),
        "## Runtime Diagnostics Snapshot".to_string(),
        String::new(),
        "These values were captured when the built-in report skill was registered. Verify them before publishing."
            .to_string(),
        String::new(),
        format!("- opencode version: {}", app.version),
        format!("- install/channel: {}", app.channel),
        os_line(env),
        format!("- Terminal: {}", terminal(env)),
        format!("- Shell: {}", shell(env)),
        format!("- Active plugins: {}", active),
    ]
    .join("\n")
}

fn os_line(env: &dyn Fn(&str) -> Option<String>) -> String {
    let emulated = platform() == "win32"
        && arch() == "x64"
        && env("PROCESSOR_IDENTIFIER").is_some_and(|value| value.starts_with("ARM"));
    format!(
        "- OS: {} {} ({} {}{})",
        os_type(),
        release(),
        platform(),
        arch(),
        if emulated { ", emulated on ARM64" } else { "" }
    )
}

fn os_type() -> String {
    match std::env::consts::OS {
        "linux" => "Linux".to_string(),
        "macos" => "Darwin".to_string(),
        "windows" => "Windows_NT".to_string(),
        "freebsd" => "FreeBSD".to_string(),
        "openbsd" => "OpenBSD".to_string(),
        "netbsd" => "NetBSD".to_string(),
        "solaris" | "illumos" => "SunOS".to_string(),
        "android" => "Android".to_string(),
        "ios" => "Darwin".to_string(),
        other => capitalize(other),
    }
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn platform() -> &'static str {
    match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        "windows" => "win32",
        "freebsd" => "freebsd",
        "openbsd" => "openbsd",
        "netbsd" => "netbsd",
        "solaris" | "illumos" => "sunos",
        "android" => "android",
        "ios" => "darwin",
        other => other,
    }
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        "arm" => "arm",
        "powerpc64" | "powerpc64le" => "ppc64",
        "s390x" => "s390x",
        "riscv64" => "riscv64",
        "loongarch64" => "loong64",
        other => other,
    }
}

fn release() -> String {
    if std::env::consts::OS != "linux" {
        return "unknown".to_string();
    }
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn terminal(env: &dyn Fn(&str) -> Option<String>) -> String {
    let values: Vec<String> = ["TERM_PROGRAM", "TERM", "COLORTERM"]
        .into_iter()
        .filter_map(|key| env(key).filter(|value| !value.is_empty()).map(|value| format!("{key}={value}")))
        .collect();
    if values.is_empty() {
        return "Unavailable: terminal environment variables are not set".to_string();
    }
    values.join(", ")
}

fn shell(env: &dyn Fn(&str) -> Option<String>) -> String {
    env("SHELL")
        .or_else(|| env("ComSpec"))
        .or_else(|| env("COMSPEC"))
        .unwrap_or_else(|| "Unavailable: shell environment variable is not set".to_string())
}
