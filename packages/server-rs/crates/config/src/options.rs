use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SystemOption {
    Disabled,
    Path(PathBuf),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigOptions {
    pub project: Option<bool>,
    pub global: Option<bool>,
    pub system: Option<SystemOption>,
    pub claude: Option<bool>,
    pub file: Option<PathBuf>,
    pub content: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigLocation {
    pub directory: PathBuf,
    pub project_id: String,
}

impl ConfigOptions {
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Self {
        let project_disable = var("OPENCODE_CONFIG_PROJECT_DISABLE").or_else(|| var("OPENCODE_DISABLE_PROJECT_CONFIG"));
        let claude_disable = truthy(var("OPENCODE_DISABLE_CLAUDE_CODE").as_deref())
            || truthy(var("OPENCODE_DISABLE_CLAUDE_CODE_SKILLS").as_deref());
        Self {
            project: Some(!truthy(project_disable.as_deref())),
            global: None,
            system: None,
            claude: Some(!claude_disable),
            file: var("OPENCODE_CONFIG").map(PathBuf::from),
            content: var("OPENCODE_CONFIG_CONTENT").map(|value| value.to_string_lossy().into_owned()),
        }
    }
}

pub fn truthy(value: Option<&OsStr>) -> bool {
    let Some(value) = value.and_then(OsStr::to_str) else {
        return false;
    };
    value == "1" || value.eq_ignore_ascii_case("true")
}
