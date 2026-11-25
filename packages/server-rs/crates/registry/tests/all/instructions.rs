use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::Full;
use hyper::Request;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::NoKv;
use opencode_config::service::{ConfigService, ServiceDeps};
use opencode_config::wellknown::WellKnown;
use opencode_registry::config::instruction::{Discovered, InstructionEnvironment, discover, fetch_remote, register};
use opencode_registry::events::RegistryEvents;
use opencode_registry::instructions::builtins::{self, Environment};
use opencode_registry::instructions::discovery::{File, InstructionDiscovery, InstructionOptions};
use opencode_registry::instructions::{Observed, canonical_json, hash, render_initial};
use serde_json::Value;
use tokio::net::TcpListener;

struct NoEvents;

impl RegistryEvents for NoEvents {
    fn updated(&self, _type: &'static str) {}
}

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<&'static str>>,
}

impl RegistryEvents for Recorder {
    fn updated(&self, r#type: &'static str) {
        self.events.lock().expect("the event lock is not poisoned").push(r#type);
    }
}

#[tokio::test]
async fn instructions_corpus_matches_expected_output() {
    let path = format!("{}/tests/corpus/instructions.json", env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(&path).expect("corpus file is readable");
    let cases: Vec<Value> = serde_json::from_str(&text).expect("corpus is a JSON array");
    assert!(!cases.is_empty(), "corpus case count");

    for case in &cases {
        let name = case["name"].as_str().expect("case name is a string");
        match case["kind"].as_str().expect("case kind is a string") {
            "discovery" => assert_discovery(case, name).await,
            "environment" => assert_environment(case, name),
            "canonical" => assert_canonical(case, name),
            other => panic!("case {name}: unknown kind {other}"),
        }
    }
}

async fn assert_discovery(case: &Value, name: &str) {
    let temp = crate::TempDir::new();
    let root = temp.path().canonicalize().expect("the temp root canonicalizes");
    let substitute = |value: &str| value.replace("{{root}}", &root.to_string_lossy());

    if let Some(files) = case["files"].as_object() {
        for (relative, content) in files {
            let target = root.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).expect("create the fixture file directory");
            }
            fs::write(&target, content.as_str().expect("fixture content is a string")).expect("write the fixture file");
        }
    }

    let options = InstructionOptions {
        project: case["options"]["project"].as_bool().expect("case project option is a boolean"),
        global: case["options"]["global"].as_bool().expect("case global option is a boolean"),
        claude: case["options"]["claude"].as_bool().expect("case claude option is a boolean"),
    };
    let environment = InstructionEnvironment {
        directory: PathBuf::from(substitute(case["directory"].as_str().expect("case directory is a string"))),
        project_directory: PathBuf::from(substitute(case["project"].as_str().expect("case project is a string"))),
        home: PathBuf::from(substitute(case["home"].as_str().expect("case home is a string"))),
        config_directory: PathBuf::from(substitute(case["config"].as_str().expect("case config is a string"))),
    };
    let instructions = case["instructions"]
        .as_array()
        .expect("case instructions are an array")
        .iter()
        .map(|source| substitute(source.as_str().expect("case instruction source is a string")))
        .collect::<Vec<_>>();

    let discovered = discover(&environment, &options, &instructions, &HashMap::new());
    let discovery = InstructionDiscovery::new(Arc::new(NoEvents), options);
    let _registration = discovery
        .transform(move |editor| match &discovered {
            Discovered::Available(files) => {
                for file in files {
                    editor.add(file.clone());
                }
            }
            Discovered::Unavailable => editor.unavailable(),
        })
        .await;

    let listed = discovery.list().await.expect("state reads");
    match &case["list"] {
        Value::String(value) => {
            assert_eq!(value, "unavailable", "case {name}: list marker");
            assert_eq!(listed, None, "case {name}: list");
        }
        Value::Array(items) => {
            let expected = items
                .iter()
                .map(|item| File {
                    path: substitute(item["path"].as_str().expect("file path is a string")),
                    content: item["content"].as_str().expect("file content is a string").to_string(),
                })
                .collect::<Vec<_>>();
            assert_eq!(listed, Some(expected), "case {name}: list");
        }
        other => panic!("case {name}: unexpected list {other}"),
    }

    let sources = discovery.load().await.expect("state reads");
    let mut values = serde_json::Map::new();
    for source in &sources {
        if let Observed::Value(value) = source.read().await {
            values.insert(source.key().as_str().to_string(), value);
        }
    }
    let text = render_initial(&sources, &values).into_iter().map(|part| part.text).collect::<Vec<_>>().join("\n\n");
    assert_eq!(text, substitute(case["text"].as_str().expect("case text is a string")), "case {name}: text");
}

fn assert_environment(case: &Value, name: &str) {
    let environment = &case["environment"];
    let input = Environment {
        directory: environment["directory"].as_str().expect("environment directory is a string").to_string(),
        project_directory: environment["projectDirectory"]
            .as_str()
            .expect("environment project directory is a string")
            .to_string(),
        vcs: environment["vcs"].as_str().map(str::to_string),
        tmp: environment["tmp"].as_str().expect("environment tmp is a string").to_string(),
    };
    assert_eq!(builtins::environment(&input), case["raw"].as_str().expect("case raw is a string"), "case {name}: raw");
    assert_eq!(
        builtins::environment_text(&input),
        case["text"].as_str().expect("case text is a string"),
        "case {name}: text"
    );
}

#[tokio::test]
async fn builtins_date_source_reads_the_clock_on_every_read() {
    let clock = Arc::new(AtomicI64::new(0));
    let source = builtins::date_source(Arc::new({
        let clock = clock.clone();
        move || clock.fetch_add(86_400_000, Ordering::Relaxed)
    }));
    assert_eq!(source.key().as_str(), "core/date");
    let first = source.read().await;
    let second = source.read().await;
    match (first, second) {
        (Observed::Value(first), Observed::Value(second)) => assert_ne!(first, second),
        other => panic!("the date source observes values: {other:?}"),
    }
}

fn assert_canonical(case: &Value, name: &str) {
    let values = case["values"].as_array().expect("case values are an array");
    let canonical = case["canonical"].as_array().expect("case canonical values are an array");
    let hashes = case["hashes"].as_array().expect("case hashes are an array");
    assert_eq!(values.len(), canonical.len(), "case {name}: canonical count");
    assert_eq!(values.len(), hashes.len(), "case {name}: hash count");
    for (index, value) in values.iter().enumerate() {
        assert_eq!(
            canonical_json(value),
            canonical[index].as_str().expect("canonical value is a string"),
            "case {name}: canonical {index}"
        );
        assert_eq!(hash(value), hashes[index].as_str().expect("hash is a string"), "case {name}: hash {index}");
    }
}

#[tokio::test]
async fn config_instructions_fetch_urls_and_keep_the_last_good_copy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the instruction server");
    let address = listener.local_addr().expect("the instruction server address");
    let server = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            tokio::spawn(async move {
                let service = service_fn(|request: Request<Incoming>| async move {
                    let ok = request.uri().path() == "/ok";
                    let body = Full::new(if ok { Bytes::from_static(b"remote") } else { Bytes::new() });
                    let status = if ok { 200 } else { 500 };
                    Ok::<_, std::convert::Infallible>(
                        hyper::Response::builder().status(status).body(body).expect("the response builds"),
                    )
                });
                let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
            });
        }
    });

    let ok = format!("http://{address}/ok");
    let fail = format!("http://{address}/fail");
    let missing = format!("http://{address}/missing");
    let previous = HashMap::from([(fail.clone(), File { path: fail.clone(), content: "old".to_string() })]);
    let sources = vec![ok.clone(), fail.clone(), missing.clone()];
    let remote = fetch_remote(&previous, &sources).await;

    assert_eq!(
        remote.get(&ok),
        Some(&File { path: ok.clone(), content: "remote".to_string() }),
        "the reachable url is fetched"
    );
    assert_eq!(
        remote.get(&fail),
        Some(&File { path: fail.clone(), content: "old".to_string() }),
        "the failing url keeps its last good copy"
    );
    assert!(!remote.contains_key(&missing), "a url that never loaded stays absent");

    server.abort();
}

#[tokio::test]
async fn config_instructions_reload_when_a_project_file_changes() {
    let temp = crate::TempDir::new();
    let root = temp.path().canonicalize().expect("the temp root canonicalizes");
    let project = root.join("project");
    let home = root.join("home");
    let config_directory = root.join("global");
    for directory in [&project, &home, &config_directory] {
        fs::create_dir_all(directory).expect("create the workspace directory");
    }
    let agents = project.join("AGENTS.md");
    fs::write(&agents, "first").expect("write the first instruction");

    let paths = GlobalPaths::from_vars(|name| match name {
        "HOME" => Some(home.clone().into_os_string()),
        "OPENCODE_CONFIG_DIR" => Some(config_directory.clone().into_os_string()),
        _ => None,
    })
    .expect("the global paths resolve");
    let options = ConfigOptions {
        project: Some(true),
        global: Some(false),
        system: Some(SystemOption::Disabled),
        claude: Some(false),
        file: None,
        content: None,
    };
    let location = ConfigLocation { directory: project.clone(), project_id: "test".to_owned() };
    let deps = ServiceDeps {
        credentials: Arc::new(()),
        wellknown: Arc::new(WellKnown::new(Arc::new(NoKv))),
        channel: "local".to_owned(),
        flag_overrides: None,
    };
    let config = ConfigService::start(options, location, paths, deps).await;

    let discovery = InstructionDiscovery::new(
        Arc::new(NoEvents),
        InstructionOptions { project: true, global: false, claude: false },
    );
    let _handle = register(
        &config,
        &discovery,
        InstructionEnvironment { directory: project.clone(), project_directory: project, home, config_directory },
        Arc::new(|_| None),
    )
    .await;

    wait_for_instruction_content(&discovery, "first").await;
    fs::write(&agents, "second").expect("rewrite the instruction");
    wait_for_instruction_content(&discovery, "second").await;
}

async fn wait_for_instruction_content(discovery: &InstructionDiscovery, content: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let files = discovery.list().await.expect("state reads");
            if files.is_some_and(|files| files.iter().any(|file| file.content == content)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the instruction file reloads");
}

#[tokio::test]
async fn instructions_discovery_emits_the_updated_event() {
    let recorder = Arc::new(Recorder::default());
    let discovery = InstructionDiscovery::new(recorder.clone(), InstructionOptions::default());
    let _registration = discovery
        .transform(|editor| {
            editor.add(File { path: "/repo/AGENTS.md".to_string(), content: "body".to_string() });
        })
        .await;

    assert_eq!(*recorder.events.lock().expect("the event lock is not poisoned"), ["instruction-discovery.updated"]);
    assert_eq!(
        discovery.list().await.expect("state reads"),
        Some(vec![File { path: "/repo/AGENTS.md".to_string(), content: "body".to_string() }])
    );
}

#[test]
fn instructions_options_from_vars_respect_the_claude_switches() {
    let disable_prompt =
        InstructionOptions::from_vars(|name| (name == "OPENCODE_DISABLE_CLAUDE_CODE_PROMPT").then(|| "1".to_string()));
    assert!(!disable_prompt.claude);
    assert!(disable_prompt.project);
    assert!(disable_prompt.global);

    let disable_all =
        InstructionOptions::from_vars(|name| (name == "OPENCODE_DISABLE_CLAUDE_CODE").then(|| "true".to_string()));
    assert!(!disable_all.claude);

    let enabled = InstructionOptions::from_vars(|_| None);
    assert!(enabled.claude);
}

#[test]
fn instructions_canonical_json_is_key_order_independent() {
    let first = serde_json::json!({ "b": 2, "a": 1, "c": { "z": 1, "y": [3, 2, 1] } });
    let second = serde_json::json!({ "c": { "y": [3, 2, 1], "z": 1 }, "a": 1, "b": 2 });

    assert_eq!(canonical_json(&first), canonical_json(&second));
    assert_eq!(hash(&first), hash(&second));
    assert_ne!(canonical_json(&serde_json::json!({ "a": 1 })), canonical_json(&serde_json::json!({ "a": 2 })));
}
