use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use opencode_config::entry::{Document, Entry, Origin};
use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::NoKv;
use opencode_config::service::{Compatibility, ConfigService, ServiceDeps};
use opencode_config::wellknown::WellKnown;
use opencode_contract::codec::{self, Decode};
use opencode_contract::generated::{
    AbsolutePath, ConfigEntryDocumentInfo, PermissionEffect, PermissionRule, SkillID, SkillInfo, SkillName,
};
use opencode_registry::builtin;
use opencode_registry::config;
use opencode_registry::events::RegistryEvents;
use opencode_registry::instructions::Observed;
use opencode_registry::skill::file::{ParseResult, parse};
use opencode_registry::skill::guidance::{self, Summary};
use opencode_registry::skill::scan;
use opencode_registry::skill::{EVENT, SkillService, available, is_uri, prepare, to_model_output, uri};

use crate::TempDir;

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<&'static str>>,
}

impl RegistryEvents for Recorder {
    fn updated(&self, r#type: &'static str) {
        self.events.lock().expect("the event lock is not poisoned").push(r#type);
    }
}

struct NoEvents;

impl RegistryEvents for NoEvents {
    fn updated(&self, _type: &'static str) {}
}

fn updates(recorder: &Recorder) -> Vec<&'static str> {
    recorder.events.lock().expect("the event lock is not poisoned").clone()
}

fn info(id: &str) -> SkillInfo {
    SkillInfo {
        id: SkillID(id.to_string()),
        name: SkillName(id.to_string()),
        description: None,
        autoinvoke: None,
        path: AbsolutePath(format!("/skills/{id}/SKILL.md")),
        content: "body".to_string(),
    }
}

fn described(id: &str, description: &str) -> SkillInfo {
    SkillInfo { description: Some(description.to_string()), ..info(id) }
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create the skill directory");
    }
    fs::write(path, content).expect("write the skill file");
}

fn parsed_autoinvoke(disable: &str) -> Option<bool> {
    let content = format!("---\ndisable-model-invocation: {disable}\n---\nbody");
    match parse(Path::new("/skills/example"), Path::new("/skills/example/SKILL.md"), &content) {
        ParseResult::Parsed(skill) => skill.autoinvoke,
        _ => panic!("the skill parses"),
    }
}

#[test]
fn parses_disable_model_invocation_values() {
    assert_eq!(parsed_autoinvoke("true"), Some(false));
    assert_eq!(parsed_autoinvoke("yes"), Some(false));
    assert_eq!(parsed_autoinvoke("false"), None);
    assert_eq!(parsed_autoinvoke("maybe"), None);
}

#[test]
fn metadata_autoinvoke_overrides_disable_model_invocation() {
    let enabled = "---\ndisable-model-invocation: true\nmetadata:\n  opencode/autoinvoke: true\n---\nbody";
    match parse(Path::new("/skills/example"), Path::new("/skills/example/SKILL.md"), enabled) {
        ParseResult::Parsed(skill) => assert_eq!(skill.autoinvoke, Some(true)),
        _ => panic!("the skill parses"),
    }

    let disabled = "---\ndisable-model-invocation: false\nmetadata:\n  opencode/autoinvoke: false\n---\nbody";
    match parse(Path::new("/skills/example"), Path::new("/skills/example/SKILL.md"), disabled) {
        ParseResult::Parsed(skill) => assert_eq!(skill.autoinvoke, Some(false)),
        _ => panic!("the skill parses"),
    }

    let ignored = "---\nmetadata:\n  opencode/autoinvoke: maybe\n---\nbody";
    match parse(Path::new("/skills/example"), Path::new("/skills/example/SKILL.md"), ignored) {
        ParseResult::Parsed(skill) => assert_eq!(skill.autoinvoke, None),
        _ => panic!("the skill parses"),
    }
}

#[test]
fn derives_nested_and_loose_skill_ids() {
    let nested = parse(
        Path::new("/skills/outer/inner"),
        Path::new("/skills/outer/inner/SKILL.md"),
        "---\ndescription: Deep skill\n---\nbody",
    );
    match nested {
        ParseResult::Parsed(skill) => {
            assert_eq!(skill.id.0, "inner");
            assert_eq!(skill.name.0, "inner");
            assert_eq!(skill.description.as_deref(), Some("Deep skill"));
            assert_eq!(skill.path.0, "/skills/outer/inner/SKILL.md");
            assert_eq!(skill.content, "body");
        }
        _ => panic!("the skill parses"),
    }

    let loose = parse(Path::new("/skills"), Path::new("/skills/foo.md"), "body");
    match loose {
        ParseResult::Parsed(skill) => {
            assert_eq!(skill.id.0, "foo");
            assert_eq!(skill.name.0, "foo");
            assert_eq!(skill.description, None);
        }
        _ => panic!("the skill parses"),
    }

    let named = parse(Path::new("/skills/named"), Path::new("/skills/named/SKILL.md"), "---\nname: Custom\n---\nbody");
    match named {
        ParseResult::Parsed(skill) => assert_eq!(skill.name.0, "Custom"),
        _ => panic!("the skill parses"),
    }
}

#[test]
fn skips_markdown_with_broken_frontmatter() {
    let content = "---\ndescription: foo: bar\nmetadata: [\n---\n# broken";
    assert!(matches!(
        parse(Path::new("/skills/broken"), Path::new("/skills/broken/SKILL.md"), content),
        ParseResult::SkippedMarkdown
    ));
}

#[test]
fn scans_skill_directories_breadth_first() {
    let temp = TempDir::new();
    let root = temp.path().canonicalize().expect("the temp root canonicalizes");
    write(&root.join("loose.md"), "loose");
    write(&root.join("nested").join("SKILL.md"), "nested");
    write(&root.join("nested").join("other.md"), "other");
    write(&root.join(".hidden").join("SKILL.md"), "hidden");
    write(&root.join("node_modules").join("pkg").join("SKILL.md"), "module");
    let mut deep = root.clone();
    for index in 1..=7 {
        deep = deep.join(format!("d{index}"));
    }
    write(&deep.join("SKILL.md"), "deep");

    let files = scan::scan(&root);
    let expected = vec![root.join("loose.md"), root.join("nested").join("SKILL.md")];
    assert_eq!(files, expected);
}

#[tokio::test]
async fn adds_sorts_and_filters_skill_files() {
    let service = SkillService::new(Arc::new(Recorder::default()));
    let mut files = IndexMap::new();
    files.insert("b.md".to_string(), "b".to_string());
    files.insert("SKILL.md".to_string(), "body".to_string());
    files.insert("a.md".to_string(), "a".to_string());
    let _registration = service.transform(move |editor| editor.add(info("demo"), Some(&files))).await;

    let stored = service.files("demo").await.expect("the state is readable").expect("the skill has files");
    let keys: Vec<&str> = stored.keys().map(String::as_str).collect();
    assert_eq!(keys, ["a.md", "b.md"]);
    assert_eq!(stored.get("a.md").map(String::as_str), Some("a"));

    let _only_skill = service
        .transform(|editor| {
            let mut files = IndexMap::new();
            files.insert("SKILL.md".to_string(), "body".to_string());
            editor.add(info("demo"), Some(&files));
        })
        .await;
    assert!(service.files("demo").await.expect("the state is readable").is_none());

    let _with_files = service
        .transform(|editor| {
            let mut files = IndexMap::new();
            files.insert("a.md".to_string(), "a".to_string());
            editor.add(info("demo"), Some(&files));
        })
        .await;
    assert!(service.files("demo").await.expect("the state is readable").is_some());
    let _cleared = service.transform(|editor| editor.add(info("demo"), None)).await;
    assert!(service.files("demo").await.expect("the state is readable").is_none());
}

#[tokio::test]
async fn updates_only_existing_skills_and_restores_the_id() {
    let service = SkillService::new(Arc::new(Recorder::default()));
    let _registration = service.transform(|editor| editor.add(info("demo"), None)).await;

    let _missing = service
        .transform(|editor| editor.update("missing", |skill| skill.name = SkillName("Changed".to_string())))
        .await;
    assert!(service.get("missing").await.expect("the state is readable").is_none());

    let _renamed = service
        .transform(|editor| {
            editor.update("demo", |skill| {
                skill.name = SkillName("Renamed".to_string());
                skill.id = SkillID("other".to_string());
            });
        })
        .await;
    assert!(service.get("other").await.expect("the state is readable").is_none());
    let stored = service.get("demo").await.expect("the state is readable").expect("the skill exists");
    assert_eq!(stored.id.0, "demo");
    assert_eq!(stored.name.0, "Renamed");
}

#[tokio::test]
async fn removes_skills_and_their_files() {
    let service = SkillService::new(Arc::new(Recorder::default()));
    let mut files = IndexMap::new();
    files.insert("a.md".to_string(), "a".to_string());
    let _registration = service
        .transform(move |editor| {
            editor.add(info("demo"), Some(&files));
            editor.add(info("other"), None);
        })
        .await;

    let _removal = service.transform(|editor| editor.remove("demo")).await;
    assert!(service.get("demo").await.expect("the state is readable").is_none());
    assert!(service.files("demo").await.expect("the state is readable").is_none());
    let ids: Vec<String> =
        service.list().await.expect("the state is readable").into_iter().map(|skill| skill.id.0).collect();
    assert_eq!(ids, ["other"]);
}

#[tokio::test]
async fn replacing_a_skill_keeps_its_list_position() {
    let service = SkillService::new(Arc::new(Recorder::default()));
    let _registration = service
        .transform(|editor| {
            editor.add(info("a"), None);
            editor.add(info("b"), None);
            editor.add(info("a"), None);
        })
        .await;

    let ids: Vec<String> =
        service.list().await.expect("the state is readable").into_iter().map(|skill| skill.id.0).collect();
    assert_eq!(ids, ["a", "b"]);
}

#[tokio::test]
async fn publishes_one_update_event_per_projection_change() {
    let recorder = Arc::new(Recorder::default());
    let service = SkillService::new(recorder.clone());
    let _first = service.transform(|editor| editor.add(info("demo"), None)).await;
    assert_eq!(updates(&recorder), [EVENT]);
    let _second = service.transform(|editor| editor.add(info("demo"), None)).await;
    assert_eq!(updates(&recorder), [EVENT]);
}

#[test]
fn filters_skills_by_the_last_matching_rule() {
    let skills = [info("build"), info("deploy")];
    let rules = [
        PermissionRule { action: "skill".to_string(), resource: "*".to_string(), effect: PermissionEffect::Deny },
        PermissionRule { action: "skill".to_string(), resource: "build".to_string(), effect: PermissionEffect::Allow },
    ];
    let allowed: Vec<String> = available(&skills, &rules).into_iter().map(|skill| skill.id.0).collect();
    assert_eq!(allowed, ["build"]);

    let rules = [
        PermissionRule { action: "skill".to_string(), resource: "*".to_string(), effect: PermissionEffect::Deny },
        PermissionRule { action: "skill".to_string(), resource: "build".to_string(), effect: PermissionEffect::Allow },
        PermissionRule { action: "skill".to_string(), resource: "build".to_string(), effect: PermissionEffect::Deny },
    ];
    assert!(available(&skills, &rules).is_empty());

    let allowed: Vec<String> = available(&skills, &[]).into_iter().map(|skill| skill.id.0).collect();
    assert_eq!(allowed, ["build", "deploy"]);
}

#[test]
fn prepares_virtual_skill_files() {
    let mut skill = info("demo");
    skill.path = AbsolutePath("skill://demo".to_string());
    let mut files = IndexMap::new();
    for index in 1..=12 {
        files.insert(format!("file{index:02}.md"), String::new());
    }
    files.insert("SKILL.md".to_string(), "body".to_string());

    let prepared = prepare(&skill, Some(&files));
    assert_eq!(prepared.directory, "skill://demo");
    assert!(prepared.output.contains("<file>skill://demo/file01.md</file>"));
    assert!(prepared.output.contains("<file>skill://demo/file10.md</file>"));
    assert!(!prepared.output.contains("file11.md"));
    assert!(!prepared.output.contains("SKILL.md"));
}

#[test]
fn prepares_real_skill_directories() {
    let temp = TempDir::new();
    let root = temp.path().canonicalize().expect("the temp root canonicalizes");
    write(&root.join("SKILL.md"), "body");
    write(&root.join("notes.md"), "notes");
    write(&root.join("scripts").join("run.sh"), "sh");
    write(&root.join("nested").join("SKILL.md"), "nested");

    let mut skill = info("demo");
    skill.path = AbsolutePath(root.join("SKILL.md").to_string_lossy().into_owned());
    let prepared = prepare(&skill, None);
    let files: Vec<String> =
        prepared.output.lines().filter(|line| line.starts_with("<file>")).map(str::to_string).collect();
    let expected = vec![
        format!("<file>{}</file>", root.join("notes.md").display()),
        format!("<file>{}</file>", root.join("scripts").join("run.sh").display()),
    ];
    assert_eq!(prepared.directory, root.to_string_lossy());
    assert_eq!(files, expected);
}

#[test]
fn builds_skill_uris_and_model_output() {
    assert_eq!(uri("demo", None), "skill://demo");
    assert_eq!(uri("demo", Some("a.md")), "skill://demo/a.md");
    assert!(is_uri("skill://demo"));
    assert!(!is_uri("demo"));

    let mut skill = info("demo");
    skill.path = AbsolutePath("/skills/demo/SKILL.md".to_string());
    skill.content = " body \n".to_string();
    let output = to_model_output(&skill, &["/skills/demo/a.md".to_string()]);
    assert!(output.contains("# Skill: demo"));
    assert!(output.contains("Base directory for this skill: /skills/demo"));
    assert!(output.contains("<file>/skills/demo/a.md</file>"));
    assert!(output.contains("body\n\nBase directory"));
}

#[test]
fn summarizes_described_invocable_skills_in_id_order() {
    let mut manual = described("manual", "Manual guidance");
    manual.autoinvoke = Some(false);
    let skills = [
        described("beta", "Beta guidance"),
        described("denied", "Denied guidance"),
        described("alpha", "Alpha guidance"),
        manual,
        info("hidden"),
    ];
    let rules = [
        PermissionRule { action: "skill".to_string(), resource: "*".to_string(), effect: PermissionEffect::Deny },
        PermissionRule { action: "skill".to_string(), resource: "beta".to_string(), effect: PermissionEffect::Allow },
        PermissionRule { action: "skill".to_string(), resource: "alpha".to_string(), effect: PermissionEffect::Allow },
    ];

    assert_eq!(
        guidance::summaries(&skills, &rules),
        [
            Summary { id: "alpha".to_string(), name: "alpha".to_string(), description: "Alpha guidance".to_string() },
            Summary { id: "beta".to_string(), name: "beta".to_string(), description: "Beta guidance".to_string() },
        ]
    );
}

#[test]
fn renders_available_skill_guidance() {
    let summaries = [Summary {
        id: "effect".to_string(),
        name: "Effect".to_string(),
        description: "Build applications with Effect".to_string(),
    }];

    assert_eq!(
        guidance::render(&summaries),
        [
            "Skills provide specialized instructions and workflows for specific tasks.",
            "Use the skill tool to load a skill when a task matches its description.",
            "The user may also invoke a skill directly. When that happens, its instructions appear in the conversation as a <skill_content> block, the same shape the skill tool returns. A skill that is already present this way does not need to be invoked again.",
            "<available_skills>",
            "  <skill>",
            "    <id>effect</id>",
            "    <name>Effect</name>",
            "    <description>Build applications with Effect</description>",
            "  </skill>",
            "</available_skills>",
        ]
        .join("\n")
    );
}

#[test]
fn skips_skill_guidance_without_summaries() {
    assert_eq!(guidance::text(&[info("hidden")], &[]), "");
    let denied =
        [PermissionRule { action: "skill".to_string(), resource: "*".to_string(), effect: PermissionEffect::Deny }];
    assert_eq!(guidance::text(&[described("alpha", "Alpha guidance")], &denied), "");
}

#[tokio::test]
async fn reads_skill_guidance_from_a_service() {
    let service = SkillService::new(Arc::new(Recorder::default()));
    let _registration = service
        .transform(|editor| {
            editor.add(described("beta", "Beta guidance"), None);
            editor.add(described("alpha", "Alpha guidance"), None);
        })
        .await;

    let source = guidance::source(service.clone(), Vec::new());
    assert_eq!(source.key().as_str(), "core/skill-guidance");

    let value = match source.read().await {
        Observed::Value(value) => value,
        other => panic!("the skill guidance is available: {other:?}"),
    };
    let items = value.as_array().expect("the skill guidance is an array");
    let ids: Vec<&str> = items.iter().map(|item| item["id"].as_str().expect("the skill id is a string")).collect();
    assert_eq!(ids, ["alpha", "beta"]);
    assert_eq!(items[0]["name"], "alpha");
    assert_eq!(items[0]["description"], "Alpha guidance");
    assert!(source.initial(&value).expect("the skill guidance renders").contains("<name>alpha</name>"));

    let _cleared = service
        .transform(|editor| {
            editor.remove("alpha");
            editor.remove("beta");
        })
        .await;
    assert_eq!(source.read().await, Observed::Removed);
}

#[tokio::test]
async fn skills_corpus_matches_expected_output() {
    let path = format!("{}/tests/corpus/skills.json", env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(&path).expect("corpus file is readable");
    let cases: Vec<serde_json::Value> = serde_json::from_str(&text).expect("corpus is a JSON array");
    assert_eq!(cases.len(), 4, "corpus case count");

    for case in &cases {
        let name = case["name"].as_str().expect("case name is a string");
        let temp = TempDir::new();
        let root = temp.path().canonicalize().expect("the temp root canonicalizes");
        let root_string = root.to_string_lossy().into_owned();

        for (relative, content) in case["files"].as_object().expect("case files are an object") {
            write(&root.join(relative), content.as_str().expect("fixture content is a string"));
        }
        let home = root.join(case["home"].as_str().expect("case home is a string"));
        let directory = root.join(case["directory"].as_str().expect("case directory is a string"));
        fs::create_dir_all(&home).expect("create the case home");
        fs::create_dir_all(&directory).expect("create the case directory");

        let mut entries = Vec::new();
        if let Some(integration) = case.get("integration") {
            let info = ConfigEntryDocumentInfo::decode(integration, &mut codec::Path::new())
                .expect("the integration document decodes");
            entries.push(Entry::Document(Document { path: None, info, origin: Origin::Managed }));
        }
        for item in case["directories"].as_array().expect("case directories are an array") {
            let item = item.as_str().expect("a case directory is a string");
            entries.push(Entry::Directory { path: root.join(item) });
        }
        for value in case["documents"].as_array().expect("case documents are an array") {
            let info =
                ConfigEntryDocumentInfo::decode(value, &mut codec::Path::new()).expect("a config document decodes");
            entries.push(Entry::Document(Document { path: None, info, origin: Origin::File }));
        }

        let compatibility = case.get("compatibility").map(|value| Compatibility {
            claude: value["claude"]
                .as_array()
                .expect("the claude roots are an array")
                .iter()
                .map(|item| root.join(item.as_str().expect("a claude root is a string")))
                .collect(),
            agents: value["agents"]
                .as_array()
                .expect("the agents roots are an array")
                .iter()
                .map(|item| root.join(item.as_str().expect("an agents root is a string")))
                .collect(),
        });
        let app = builtin::skill::AppInfo {
            version: case["app"]["version"].as_str().expect("the app version is a string").to_string(),
            channel: case["app"]["channel"].as_str().expect("the app channel is a string").to_string(),
        };

        let service = SkillService::new(Arc::new(NoEvents));
        let _builtins = service
            .transform({
                let app = app.clone();
                let entries = entries.clone();
                move |editor| builtin::skill::skill(editor, &app, &|_: &str| -> Option<String> { None }, &entries)
            })
            .await;
        let _config_skills = service
            .transform({
                let entries = entries.clone();
                let environment = config::skill::SkillEnvironment { directory: directory.clone(), home: home.clone() };
                move |editor| config::skill::apply(&entries, &environment, editor)
            })
            .await;
        let _compatibility = match &compatibility {
            Some(compatibility) => {
                let compatibility = compatibility.clone();
                Some(service.transform(move |editor| config::compatibility::apply(&compatibility, editor)).await)
            }
            None => None,
        };
        let _policy = service.transform(move |editor| config::policy::apply(&entries, editor)).await;

        let mut actual = service.list().await.expect("the skill state is readable");
        for skill in &mut actual {
            if skill.id.0 == "report" {
                skill.content = scrub_report(&skill.content);
            }
        }
        let mut expected: Vec<SkillInfo> = case["list"]
            .as_array()
            .expect("the expected list is an array")
            .iter()
            .map(|item| SkillInfo::decode(item, &mut codec::Path::new()).expect("an expected skill decodes"))
            .collect();
        for skill in &mut expected {
            skill.path.0 = skill.path.0.replace("{{root}}", &root_string);
        }
        assert_eq!(actual, expected, "case {name}: list");

        let expected_guidance =
            case["guidance"].as_str().expect("the expected guidance is a string").replace("{{root}}", &root_string);
        assert_eq!(guidance::text(&actual, &[]), expected_guidance, "case {name}: guidance");
    }
}

fn scrub_report(content: &str) -> String {
    const PREFIXES: [&str; 5] = ["- opencode version: ", "- install/channel: ", "- OS: ", "- Terminal: ", "- Shell: "];
    content
        .split('\n')
        .map(|line| match PREFIXES.into_iter().find(|prefix| line.starts_with(prefix)) {
            Some(prefix) => format!("{prefix}<value>"),
            None => line.to_string(),
        })
        .collect::<Vec<String>>()
        .join("\n")
}

#[test]
fn report_skill_prints_its_diagnostic_lines() {
    let info = ConfigEntryDocumentInfo::decode(
        &serde_json::json!({
            "plugins": ["package-plugin", "-disabled", "local.ts", {"package": "package-plugin", "options": {}}]
        }),
        &mut codec::Path::new(),
    )
    .expect("the plugins document decodes");
    let entries = [Entry::Document(Document { path: None, info, origin: Origin::File })];
    let env = |name: &str| -> Option<String> {
        match name {
            "TERM_PROGRAM" => Some("iTerm.app".to_string()),
            "COLORTERM" => Some("truecolor".to_string()),
            "SHELL" => Some("/bin/zsh".to_string()),
            _ => None,
        }
    };
    let app = builtin::skill::AppInfo { version: "9.9.9".to_string(), channel: "test".to_string() };
    let report = builtin::skill::report(&app, &env, &entries);

    for line in [
        "- opencode version: 9.9.9",
        "- install/channel: test",
        "- Terminal: TERM_PROGRAM=iTerm.app, COLORTERM=truecolor",
        "- Shell: /bin/zsh",
        "- Active plugins: -disabled, local.ts, package-plugin, package-plugin",
    ] {
        assert!(report.lines().any(|candidate| candidate == line), "the report prints {line}");
    }
    let os = report.lines().rfind(|line| line.starts_with("- OS: ")).expect("the report prints an OS line");
    assert!(os.ends_with(')'), "the OS line ends with its platform group");
    assert!(os.contains(" ("), "the OS line names its platform group");
    assert!(report.contains("## Runtime Diagnostics Snapshot"), "the report prints its snapshot heading");
}

async fn wait_for_skill_description(skills: &SkillService, id: &str, description: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let info = skills.get(id).await.expect("the skill state is readable");
            if info.and_then(|info| info.description).as_deref() == Some(description) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the config skill reloads");
}

#[tokio::test]
async fn config_skill_register_reloads_when_the_file_changes() {
    let temp = TempDir::new();
    let root = temp.path().canonicalize().expect("the temp root canonicalizes");
    let project = root.join("project");
    let home = root.join("home");
    let global = root.join("global");
    for directory in [&project, &home, &global] {
        fs::create_dir_all(directory).expect("create the workspace directory");
    }
    let file = project.join(".opencode/skills/deploy/SKILL.md");
    write(&file, "---\nname: Deploy\ndescription: First\n---\n# Deploy");

    let paths = GlobalPaths::from_vars(|name| match name {
        "HOME" => Some(home.clone().into_os_string()),
        "OPENCODE_CONFIG_DIR" => Some(global.clone().into_os_string()),
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

    let skills = SkillService::new(Arc::new(NoEvents));
    let _handle =
        config::skill::register(&config, &skills, config::skill::SkillEnvironment { directory: project, home }).await;

    wait_for_skill_description(&skills, "deploy", "First").await;
    write(&file, "---\nname: Deploy\ndescription: Second\n---\n# Deploy");
    wait_for_skill_description(&skills, "deploy", "Second").await;
}

#[tokio::test]
async fn config_compatibility_and_policy_load_skill_roots() {
    let temp = TempDir::new();
    let root = temp.path().canonicalize().expect("the temp root canonicalizes");
    write(&root.join("claude/skills/review/SKILL.md"), "---\nname: Review\ndescription: Claude review\n---\n# Review");
    write(&root.join("agents/skills/check/SKILL.md"), "---\nname: Check\ndescription: Agent check\n---\n# Check");

    let compatibility = Compatibility { claude: vec![root.join("claude")], agents: vec![root.join("agents")] };
    let ids: Vec<String> = config::compatibility::load(&compatibility).into_iter().map(|skill| skill.id.0).collect();
    assert_eq!(ids, ["review", "check"], "compatibility skills load in root order");

    let info = ConfigEntryDocumentInfo::decode(
        &serde_json::json!({
            "policies": [
                {"action": "skill.use", "resource": "*", "effect": "deny"},
                {"action": "skill.use", "resource": "review", "effect": "allow"}
            ]
        }),
        &mut codec::Path::new(),
    )
    .expect("the policy document decodes");
    let entries = [Entry::Document(Document { path: None, info, origin: Origin::File })];
    let service = SkillService::new(Arc::new(NoEvents));
    let _registration = service
        .transform({
            let compatibility = compatibility.clone();
            move |editor| {
                config::compatibility::apply(&compatibility, editor);
                config::policy::apply(&entries, editor);
            }
        })
        .await;
    let ids: Vec<String> =
        service.list().await.expect("the skill state is readable").into_iter().map(|skill| skill.id.0).collect();
    assert_eq!(ids, ["review"], "policy removes the denied compatibility skill");

    let absolute = root.join("absolute");
    let info = ConfigEntryDocumentInfo::decode(
        &serde_json::json!({ "skills": ["~/shared", "./relative", absolute.to_string_lossy()] }),
        &mut codec::Path::new(),
    )
    .expect("the sources document decodes");
    let entries = [
        Entry::Directory { path: root.join("dir") },
        Entry::Document(Document { path: None, info, origin: Origin::File }),
    ];
    let paths = config::skill::sources(
        &entries,
        &config::skill::SkillEnvironment { directory: root.join("project"), home: root.join("home") },
    );
    let expected = vec![
        root.join("dir/skill"),
        root.join("dir/skills"),
        root.join("home/shared"),
        root.join("project/relative"),
        absolute,
    ];
    assert_eq!(paths, expected, "skill sources resolve directories and configured roots");
}
