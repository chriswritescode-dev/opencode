use std::path::Path;

use opencode_config::entry::{Document, Entry, Origin};
use opencode_contract::codec::{JsonMap, decode_json};
use opencode_contract::generated::{
    ConfigEntryDocumentInfo, ConfigEntryDocumentInfoPluginsItem, ConfigEntryDocumentInfoPluginsItemObject,
};
use opencode_plugin::generation::Source;
use opencode_plugin::host::Entrypoints;
use opencode_plugin::operations::{
    AddOperation, Operation, configured, discover, parse, policy_blocks, scan, source_of,
};
use serde_json::{Map, json};

use crate::support::{FakeHost, TempDir, entries};

fn add(target: String) -> Operation {
    Operation::Add(AddOperation { target, options: Map::new(), mtime: None })
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn plugin_document(target: &str) -> String {
    format!(r#"{{"plugins":[{}]}}"#, serde_json::to_string(target).expect("the target is JSON"))
}

#[test]
fn parses_strings_objects_and_removals() {
    assert_eq!(parse(&ConfigEntryDocumentInfoPluginsItem::String("pkg".to_string())), Ok(add("pkg".to_string())));
    assert_eq!(
        parse(&ConfigEntryDocumentInfoPluginsItem::Object(ConfigEntryDocumentInfoPluginsItemObject {
            package: "pkg".to_string(),
            options: Some(JsonMap(vec![("x".to_string(), json!(1))])),
        })),
        Ok(Operation::Add(AddOperation {
            target: "pkg".to_string(),
            options: Map::from_iter([("x".to_string(), json!(1))]),
            mtime: None,
        }))
    );
    assert_eq!(
        parse(&ConfigEntryDocumentInfoPluginsItem::String("-pkg".to_string())),
        Ok(Operation::Remove { target: "pkg".to_string() })
    );
    assert_eq!(
        serde_json::to_string(&Operation::Add(AddOperation {
            target: "pkg".to_string(),
            options: Map::new(),
            mtime: Some(7),
        }))
        .expect("the operation serializes"),
        r#"{"type":"add","target":"pkg","options":{},"mtime":7}"#
    );
}

#[test]
fn rejects_a_bare_minus() {
    assert_eq!(
        parse(&ConfigEntryDocumentInfoPluginsItem::String("-".to_string())),
        Err("Plugin remove operation requires a target".to_string())
    );
}

#[test]
fn discovers_plugin_and_plugins_directories_sorted() {
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.path().join("plugin")).expect("create the plugin directory");
    std::fs::create_dir_all(dir.path().join("plugins")).expect("create the plugins directory");
    let first = dir.path().join("plugin").join("a.ts");
    let second = dir.path().join("plugins").join("b.js");
    std::fs::write(&first, "").expect("write the plugin file");
    std::fs::write(&second, "").expect("write the second plugin file");

    assert_eq!(discover(dir.path()), vec![first, second]);
}

#[test]
fn discovery_keeps_ts_js_directories_and_linked_targets() {
    let dir = TempDir::new();
    let plugin = dir.path().join("plugin");
    std::fs::create_dir_all(plugin.join("nested")).expect("create the nested directory");
    std::fs::write(plugin.join("a.ts"), "").expect("write the ts file");
    std::fs::write(plugin.join("b.js"), "").expect("write the js file");
    std::fs::write(plugin.join("c.tsx"), "").expect("write the tsx file");
    std::fs::write(plugin.join("notes.txt"), "").expect("write the notes file");

    assert_eq!(discover(dir.path()), vec![plugin.join("a.ts"), plugin.join("b.js"), plugin.join("nested")]);

    #[cfg(unix)]
    {
        let target = dir.path().join("target.ts");
        std::fs::write(&target, "").expect("write the linked target");
        std::os::unix::fs::symlink(&target, plugin.join("link.ts")).expect("link the ts file");
        std::os::unix::fs::symlink(plugin.join("nested"), plugin.join("linked")).expect("link the directory");
        std::os::unix::fs::symlink(&target, plugin.join("link.txt")).expect("link the text file");

        assert_eq!(
            discover(dir.path()),
            vec![
                plugin.join("a.ts"),
                plugin.join("b.js"),
                plugin.join("link.ts"),
                plugin.join("linked"),
                plugin.join("nested"),
            ]
        );
    }
}

#[test]
fn resolves_file_urls_and_relative_targets_against_the_document() {
    let dir = TempDir::new();
    let config = dir.path().join("config").join("config.json");
    let documents = entries(
        &[(config.to_str().expect("the temp path is UTF-8"), r#"{"plugins":["./local","../shared/./x","pkg"]}"#)],
        &[],
    );

    assert_eq!(
        configured(&documents, dir.path()),
        vec![
            add(text(&dir.path().join("config").join("local"))),
            add(text(&dir.path().join("shared").join("x"))),
            add("pkg".to_string()),
        ]
    );

    let from_url = dir.path().join("from-url");
    let url = url::Url::from_file_path(&from_url).expect("the temp path is absolute").to_string();
    let config = dir.path().join("config").join("url.json");
    let documents = entries(&[(config.to_str().expect("the temp path is UTF-8"), &plugin_document(&url))], &[]);

    assert_eq!(configured(&documents, dir.path()), vec![add(text(&from_url))]);

    let location = dir.path().join("location");
    let document = Entry::Document(Document {
        path: None,
        info: decode_json::<ConfigEntryDocumentInfo>(r#"{"plugins":["./here"]}"#).expect("the document decodes"),
        origin: Origin::File,
    });

    assert_eq!(configured(&[document], &location), vec![add(text(&location.join("here")))]);
}

#[test]
fn drops_a_configured_file_path() {
    let dir = TempDir::new();
    let file = dir.path().join("file.ts");
    std::fs::write(&file, "").expect("write the plugin file");
    let config = dir.path().join("config.json");
    let documents = entries(&[(config.to_str().expect("the temp path is UTF-8"), &plugin_document(&text(&file)))], &[]);

    assert!(configured(&documents, dir.path()).is_empty());
}

#[tokio::test]
async fn explicit_config_follows_discovered_operations() {
    let dir = TempDir::new();
    let discovered = dir.path().join("plugin").join("a.ts");
    std::fs::create_dir_all(dir.path().join("plugin")).expect("create the plugin directory");
    std::fs::write(&discovered, "").expect("write the plugin file");
    let config = dir.path().join("config.json");
    let documents =
        entries(&[(config.to_str().expect("the temp path is UTF-8"), r#"{"plugins":["pkg"]}"#)], &[dir.path()]);

    let operations = scan(&documents, dir.path(), None).await;

    assert_eq!(operations.len(), 2);
    match &operations[0] {
        Operation::Add(operation) => {
            assert_eq!(operation.target, text(&discovered));
            assert!(operation.mtime.is_some());
        }
        Operation::Remove { target } => panic!("the discovered operation is an add to {target}"),
    }
    assert_eq!(operations[1], add("pkg".to_string()));
}

#[tokio::test]
async fn stamps_mtime_from_entrypoints_and_package_json() {
    let dir = TempDir::new();
    let local = dir.path().join("local");
    std::fs::create_dir_all(&local).expect("create the plugin directory");
    let server = local.join("server.ts");
    let package = local.join("package.json");
    std::fs::write(&server, "").expect("write the server entry");
    std::fs::write(&package, "{}").expect("write the package file");
    std::fs::File::options()
        .write(true)
        .open(&server)
        .expect("open the server entry")
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000))
        .expect("set the server mtime");
    std::fs::File::options()
        .write(true)
        .open(&package)
        .expect("open the package file")
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(2_000))
        .expect("set the package mtime");
    let config = dir.path().join("config.json");
    let documents =
        entries(&[(config.to_str().expect("the temp path is UTF-8"), &plugin_document(&text(&local)))], &[]);
    let host = FakeHost::new().resolving(
        local.as_path(),
        Entrypoints {
            server: Some(url::Url::from_file_path(&server).expect("the server path is absolute").to_string()),
            ..Default::default()
        },
    );

    let operations = scan(&documents, dir.path(), Some(&host)).await;

    assert_eq!(
        operations,
        vec![Operation::Add(AddOperation { target: text(&local), options: Map::new(), mtime: Some(2_000_000) })]
    );
}

#[tokio::test]
async fn drops_a_directory_without_server_or_gui_entry() {
    let dir = TempDir::new();
    let local = dir.path().join("local");
    std::fs::create_dir_all(&local).expect("create the plugin directory");
    let tui = local.join("tui.ts");
    std::fs::write(&tui, "").expect("write the tui entry");
    let config = dir.path().join("config.json");
    let documents =
        entries(&[(config.to_str().expect("the temp path is UTF-8"), &plugin_document(&text(&local)))], &[]);
    let host = FakeHost::new().resolving(
        local.as_path(),
        Entrypoints {
            tui: Some(url::Url::from_file_path(&tui).expect("the tui path is absolute").to_string()),
            ..Default::default()
        },
    );

    assert!(scan(&documents, dir.path(), Some(&host)).await.is_empty());
    assert!(scan(&documents, dir.path(), None).await.is_empty());
}

#[tokio::test]
async fn drops_an_entrypoint_outside_its_directory() {
    let dir = TempDir::new();
    let local = dir.path().join("local");
    std::fs::create_dir_all(&local).expect("create the plugin directory");
    let outside = dir.path().join("outside.ts");
    std::fs::write(&outside, "").expect("write the outside entry");
    let config = dir.path().join("config.json");
    let documents =
        entries(&[(config.to_str().expect("the temp path is UTF-8"), &plugin_document(&text(&local)))], &[]);
    let host = FakeHost::new().resolving(
        local.as_path(),
        Entrypoints {
            server: Some(url::Url::from_file_path(&outside).expect("the outside path is absolute").to_string()),
            ..Default::default()
        },
    );

    assert!(scan(&documents, dir.path(), Some(&host)).await.is_empty());
}

#[tokio::test]
async fn never_calls_the_host_without_local_directories() {
    let dir = TempDir::new();
    let config = dir.path().join("config.json");
    let documents = entries(&[(config.to_str().expect("the temp path is UTF-8"), r#"{"plugins":["pkg@1.0.0"]}"#)], &[]);
    let host = FakeHost::new();

    let operations = scan(&documents, dir.path(), Some(&host)).await;

    assert_eq!(operations, vec![add("pkg@1.0.0".to_string())]);
    assert!(host.calls().is_empty());
}

#[test]
fn policy_blocks_versioned_package_by_name() {
    let dir = TempDir::new();
    let config = dir.path().join("config.json");
    let documents = entries(
        &[(
            config.to_str().expect("the temp path is UTF-8"),
            r#"{"policies":[{"action":"plugin.use","resource":"pkg","effect":"deny"}]}"#,
        )],
        &[],
    );

    assert!(policy_blocks(&documents, "pkg@1.2.3"));
    assert!(!policy_blocks(&documents, "other@1.2.3"));
}

#[test]
fn policy_last_matching_statement_wins() {
    let dir = TempDir::new();
    let config = dir.path().join("config.json");
    let documents = entries(
        &[(
            config.to_str().expect("the temp path is UTF-8"),
            r#"{"policies":[{"action":"plugin.use","resource":"*","effect":"deny"},{"action":"plugin.use","resource":"pkg","effect":"allow"}]}"#,
        )],
        &[],
    );

    assert!(!policy_blocks(&documents, "pkg@1"));
    assert!(policy_blocks(&documents, "other@1"));
}

#[test]
fn source_of_picks_local_and_package() {
    let dir = TempDir::new();
    let local = dir.path().join("local");

    assert_eq!(source_of(&text(&local), None), Source::Local { path: text(&local) });
    assert_eq!(
        source_of("pkg", Some("1.2.3")),
        Source::Package {
            target: "pkg".to_string(),
            version: Some("1.2.3".to_string()),
            outdated: false,
            updating: false,
        }
    );
}
