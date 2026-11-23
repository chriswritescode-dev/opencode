use std::sync::Arc;

use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::NoKv;
use opencode_config::service::{ConfigService, ServiceDeps};
use opencode_config::wellknown::WellKnown;
use opencode_contract::generated::requests::ConfigPatch;
use opencode_util::temp::TempDir;
use serde_json::json;

use crate::support::{area, entries_view};

#[tokio::test]
async fn every_update_case_matches_the_pinned_typescript() {
    let cases = area("update");
    assert_eq!(cases.len(), 18, "expected 18 update cases, found {}", cases.len());
    for case in &cases {
        verify(case).await;
    }
}

async fn verify(case: &crate::support::Case) {
    let temp = TempDir::new().unwrap();
    let root = opencode_util::path::realpath(temp.path()).unwrap();
    let project = root.join("project");
    let home = root.join("home");
    let global = root.join("global");
    for directory in [&project, &home, &global] {
        std::fs::create_dir_all(directory).unwrap();
    }
    for (file, content) in case.value["files"].as_object().cloned().unwrap_or_default() {
        std::fs::write(global.join(&file), content.as_str().unwrap()).unwrap();
    }

    let paths = GlobalPaths::from_vars(|name| match name {
        "HOME" => Some(home.clone().into_os_string()),
        "OPENCODE_CONFIG_DIR" => Some(global.clone().into_os_string()),
        _ => None,
    })
    .unwrap();
    let options = ConfigOptions {
        project: Some(true),
        global: None,
        system: Some(SystemOption::Disabled),
        claude: Some(false),
        file: None,
        content: None,
    };
    let location = ConfigLocation { directory: project, project_id: "global".to_owned() };
    let deps = ServiceDeps {
        credentials: Arc::new(()),
        wellknown: Arc::new(WellKnown::new(Arc::new(NoKv))),
        channel: "unknown".to_owned(),
        flag_overrides: None,
    };
    let service = ConfigService::start(options, location, paths, deps).await;

    service.update(&ConfigPatch { shell: case.value["patch"]["shell"].as_str().map(str::to_owned) }).await;

    let written = root.join(case.value["file"].as_str().unwrap());
    let text = std::fs::read_to_string(&written).unwrap();
    assert_eq!(text, case.value["text"].as_str().unwrap(), "{} text", case.name);

    service.reload().await;
    assert_eq!(json!(entries_view(&root, &service.entries())), case.value["entries"], "{} entries", case.name);
}
