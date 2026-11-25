use std::time::{SystemTime, UNIX_EPOCH};

use opencode_util::time::Zone;

use crate::instructions::{Key, List, combine, fixed};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Environment {
    pub directory: String,
    pub project_directory: String,
    pub vcs: Option<String>,
    pub tmp: String,
}

pub fn platform() -> &'static str {
    platform_name(std::env::consts::OS)
}

fn platform_name(os: &str) -> &str {
    match os {
        "macos" | "ios" | "tvos" | "watchos" | "visionos" => "darwin",
        "windows" => "win32",
        "solaris" | "illumos" => "sunos",
        other => other,
    }
}

pub fn now_millis() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_millis() as i64)
}

pub fn date_string(now_ms: i64) -> String {
    Zone::Local.at(now_ms).map(|date| date.date_string()).unwrap_or_default()
}

pub fn environment(environment: &Environment) -> String {
    [
        "<env>".to_string(),
        format!("  Working directory: {}", environment.directory),
        format!("  Workspace root folder: {}", environment.project_directory),
        format!(
            "  Is directory a git repo: {}",
            if environment.vcs.as_deref() == Some("git") { "yes" } else { "no" }
        ),
        format!("  Platform: {}", platform()),
        format!(
            "  Prefer {} over generic system temporary directories such as /tmp; it is pre-created and approved for external access.",
            environment.tmp
        ),
        "</env>".to_string(),
    ]
    .join("\n")
}

pub fn environment_text(environment: &Environment) -> String {
    format!(
        "Here is some useful information about the environment you are running in:\n{}",
        self::environment(environment)
    )
}

pub fn load(environment: &Environment, now_ms: i64) -> List {
    combine([
        vec![fixed(
            Key::new("core/date").expect("the built-in instruction key is valid"),
            serde_json::Value::String(date_string(now_ms)),
            |value| value.as_str().map(|date| format!("Today's date: {date}")),
        )],
        vec![fixed(
            Key::new("core/environment").expect("the built-in instruction key is valid"),
            serde_json::Value::String(self::environment(environment)),
            |value| {
                value.as_str().map(|environment| {
                    format!("Here is some useful information about the environment you are running in:\n{environment}")
                })
            },
        )],
    ])
    .expect("the built-in instruction keys are distinct")
}

#[cfg(test)]
mod tests {
    use super::platform_name;

    #[test]
    fn instructions_platform_matches_node() {
        assert_eq!(platform_name("macos"), "darwin");
        assert_eq!(platform_name("ios"), "darwin");
        assert_eq!(platform_name("windows"), "win32");
        assert_eq!(platform_name("solaris"), "sunos");
        assert_eq!(platform_name("linux"), "linux");
    }
}
