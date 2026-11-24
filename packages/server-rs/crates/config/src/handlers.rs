use std::ffi::OsStr;
use std::path::Path;

use opencode_contract::generated::ConfigShellOption;

use crate::entry::Entry;
use crate::paths::GlobalPaths;
use crate::service::{ConfigService, ConfigUpdateError};
use crate::shell;

pub fn config_get(config: &ConfigService) -> opencode_contract::generated::operations::config_get::Response {
    config.entries().iter().map(Entry::to_wire).collect()
}

pub fn config_shells(
    paths: &GlobalPaths,
    gitbash: Option<&Path>,
) -> opencode_contract::generated::operations::config_shells::Response {
    config_shells_with(&paths.bin, gitbash, Path::new("/etc/shells"), std::env::var_os("PATH").as_deref())
}

pub fn config_shells_with(
    bin: &Path,
    gitbash: Option<&Path>,
    shells: &Path,
    path: Option<&OsStr>,
) -> opencode_contract::generated::operations::config_shells::Response {
    shell::list(shells, path, bin, gitbash)
        .into_iter()
        .map(|item| ConfigShellOption { path: item.path, name: item.name, acceptable: item.acceptable })
        .collect()
}

pub async fn experimental_config_update(
    config: &ConfigService,
    body: opencode_contract::generated::operations::experimental_config_update::Body,
) -> Result<(), ConfigUpdateError> {
    config.update(&body).await
}
