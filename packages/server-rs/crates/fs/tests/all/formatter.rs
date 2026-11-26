use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use opencode_config::entry::{Document, Entry, Origin};
use opencode_contract::codec::{self, Decode};
use opencode_contract::generated::ConfigEntryDocumentInfo;
use opencode_fs::access::{Access, Authorizer, Invocation, ResolveInput, Target};
use opencode_fs::formatter::Formatter;
use opencode_fs::mutation::Mutation;
use opencode_fs::write::{FileWriter, WriteInput};
use opencode_permission::service::{AssertInput, PermissionError};
use serde_json::{Value, json};

use crate::fixture::TempDir;

struct Case {
    _temp: TempDir,
    project: PathBuf,
    bin: PathBuf,
}

impl Case {
    fn new(label: &str) -> Self {
        let temp = TempDir::new(label);
        let project = temp.path().join("project");
        let bin = temp.path().join("bin");
        fs::create_dir_all(&project).expect("creates the project directory");
        fs::create_dir_all(&bin).expect("creates the bin directory");
        Self { _temp: temp, project, bin }
    }

    fn script(&self, name: &str, body: &str) -> String {
        let path = self.bin.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("writes the formatter script");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("makes the formatter executable");
        path.to_string_lossy().into_owned()
    }

    fn formatter(&self, layers: &[Option<Value>]) -> Formatter {
        self.formatter_at(&self.project, layers)
    }

    fn formatter_at(&self, directory: &Path, layers: &[Option<Value>]) -> Formatter {
        let bin = self.bin.clone();
        let formatter =
            Formatter::new(directory.to_path_buf(), self.project.clone(), self.bin.clone(), move |name| match name {
                "PATH" => Some(OsString::from(bin.as_os_str())),
                _ => None,
            });
        let entries: Vec<Entry> = layers.iter().flatten().map(entry).collect();
        formatter.configure(&entries);
        formatter
    }

    fn file(&self, name: &str, content: &str) -> PathBuf {
        let path = self.project.join(name);
        fs::write(&path, content).expect("writes the formatted file");
        path
    }
}

fn entry(formatter: &Value) -> Entry {
    let info = ConfigEntryDocumentInfo::decode(&json!({ "formatter": formatter }), &mut codec::Path::new())
        .expect("the formatter document decodes");
    Entry::Document(Document { path: None, info, origin: Origin::File })
}

fn target(path: &Path) -> Target {
    Target {
        absolute: path.to_string_lossy().into_owned(),
        resource: path.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        external_directory: None,
    }
}

async fn assert_format(formatter: &Formatter, file: &Path, expected: Option<&str>) {
    let formatted = formatter.file(&target(file)).await;
    match expected {
        Some(expected) => {
            assert!(formatted, "{} is formatted", file.display());
            assert_eq!(fs::read_to_string(file).expect("reads the formatted file"), expected);
        }
        None => assert!(!formatted, "{} is left alone", file.display()),
    }
}

#[tokio::test]
async fn keeps_lower_formatters_when_a_higher_config_adds_another() {
    let case = Case::new("formatter-layers-add");
    let markdown = case.script("markdown.sh", r#"printf 'md' >> "$1""#);
    let project = case.script("project.sh", r#"printf 'project' >> "$1""#);
    let formatter = case.formatter(&[
        Some(json!({ "markdown": { "command": [markdown, "$FILE"], "extensions": [".md"] } })),
        None,
        Some(json!({ "project": { "command": [project, "$FILE"], "extensions": [".project"] } })),
    ]);

    assert_format(&formatter, &case.file("test.md", ""), Some("md")).await;
    assert_format(&formatter, &case.file("test.project", ""), Some("project")).await;
}

#[tokio::test]
async fn inherits_omitted_fields_and_replaces_supplied_arrays() {
    let case = Case::new("formatter-layers-inherit");
    let a = case.script("a.sh", r#"printf 'A' >> "$1""#);
    let b = case.script("b.sh", r#"printf 'B' >> "$1""#);
    let formatter = case.formatter(&[
        Some(json!({ "markdown": { "command": [a, "$FILE"], "extensions": [".md", ".project"] } })),
        Some(json!({ "markdown": { "command": [b, "$FILE"], "extensions": [".project"] } })),
    ]);

    assert_format(&formatter, &case.file("test.md", ""), None).await;
    assert_format(&formatter, &case.file("test.project", ""), Some("B")).await;
}

#[tokio::test]
async fn merges_environment_variables_by_key() {
    let case = Case::new("formatter-layers-environment");
    let script = case.script("environment.sh", r#"printf '%s%s' "$FIRST" "$SECOND" >> "$1""#);
    let formatter = case.formatter(&[
        Some(json!({
            "markdown": {
                "command": [script, "$FILE"],
                "extensions": [".md"],
                "environment": { "FIRST": "global", "SECOND": "global" }
            }
        })),
        Some(json!({ "markdown": { "environment": { "SECOND": "project" } } })),
    ]);

    assert_format(&formatter, &case.file("test.md", ""), Some("globalproject")).await;
}

#[tokio::test]
async fn keeps_a_lower_disable_through_overrides_that_omit_it() {
    let case = Case::new("formatter-layers-disable");
    let markdown = case.script("markdown.sh", r#"printf 'md' >> "$1""#);
    let overridden = case.script("override.sh", r#"printf 'override' >> "$1""#);
    let project = case.script("project.sh", r#"printf 'project' >> "$1""#);
    let formatter = case.formatter(&[
        Some(json!({
            "markdown": { "command": [markdown, "$FILE"], "extensions": [".md"], "disabled": true }
        })),
        Some(json!({
            "markdown": { "command": [overridden, "$FILE"] },
            "project": { "command": [project, "$FILE"], "extensions": [".project"] }
        })),
    ]);

    assert_format(&formatter, &case.file("test.md", ""), None).await;
    assert_format(&formatter, &case.file("test.project", ""), Some("project")).await;
}

#[tokio::test]
async fn re_enables_a_lower_disabled_formatter() {
    let case = Case::new("formatter-layers-reenable");
    let markdown = case.script("markdown.sh", r#"printf 'md' >> "$1""#);
    let formatter = case.formatter(&[
        Some(json!({
            "markdown": { "command": [markdown, "$FILE"], "extensions": [".md"], "disabled": true }
        })),
        Some(json!({ "markdown": { "disabled": false } })),
    ]);

    assert_format(&formatter, &case.file("test.md", ""), Some("md")).await;
}

#[tokio::test]
async fn keeps_lower_formatters_through_an_empty_object() {
    let case = Case::new("formatter-layers-empty");
    let markdown = case.script("markdown.sh", r#"printf 'md' >> "$1""#);
    let formatter = case.formatter(&[
        Some(json!({ "markdown": { "command": [markdown, "$FILE"], "extensions": [".md"] } })),
        Some(json!({})),
    ]);

    assert_format(&formatter, &case.file("test.md", ""), Some("md")).await;
}

#[tokio::test]
async fn clears_lower_formatters_when_a_higher_config_is_false() {
    let case = Case::new("formatter-layers-false");
    let markdown = case.script("markdown.sh", r#"printf 'md' >> "$1""#);
    let formatter = case.formatter(&[
        Some(json!({ "markdown": { "command": [markdown, "$FILE"], "extensions": [".md"] } })),
        Some(json!(false)),
    ]);

    assert_format(&formatter, &case.file("test.md", ""), None).await;
}

#[tokio::test]
async fn clears_lower_formatters_before_a_later_object_after_true() {
    let case = Case::new("formatter-layers-true");
    let markdown = case.script("markdown.sh", r#"printf 'md' >> "$1""#);
    let project = case.script("project.sh", r#"printf 'project' >> "$1""#);
    let formatter = case.formatter(&[
        Some(json!({ "markdown": { "command": [markdown, "$FILE"], "extensions": [".md"] } })),
        Some(json!(true)),
        Some(json!({ "project": { "command": [project, "$FILE"], "extensions": [".project"] } })),
    ]);

    assert_format(&formatter, &case.file("test.md", ""), None).await;
    assert_format(&formatter, &case.file("test.project", ""), Some("project")).await;
}

#[tokio::test]
async fn enables_only_formatters_configured_after_false() {
    let case = Case::new("formatter-layers-false-then-object");
    let markdown = case.script("markdown.sh", r#"printf 'md' >> "$1""#);
    let project = case.script("project.sh", r#"printf 'project' >> "$1""#);
    let formatter = case.formatter(&[
        Some(json!({ "markdown": { "command": [markdown, "$FILE"], "extensions": [".md"] } })),
        Some(json!(false)),
        Some(json!({ "project": { "command": [project, "$FILE"], "extensions": [".project"] } })),
    ]);

    assert_format(&formatter, &case.file("test.md", ""), None).await;
    assert_format(&formatter, &case.file("test.project", ""), Some("project")).await;
}

#[tokio::test]
async fn matches_the_node_extension_rules() {
    let case = Case::new("formatter-extension");
    let exit = case.script("exit-zero.sh", "exit 0");
    let cases = [
        ("test.match", ".match", true),
        ("test.other", ".match", false),
        ("test.MATCH", ".match", false),
        ("test.MATCH", ".MATCH", true),
        (".match", ".match", false),
        (".match", "", true),
        ("README", ".match", false),
        ("README", "", true),
        ("test.part.match", ".match", true),
        ("test.part.match", ".part.match", false),
    ];

    for (name, extension, matches) in cases {
        let formatter = case.formatter(&[Some(json!({
            "matching": { "command": [exit, "$FILE"], "extensions": [extension] }
        }))]);
        let formatted = formatter.file(&target(&case.project.join(name))).await;
        assert_eq!(formatted, matches, "{name} against {extension:?}");
    }
}

#[tokio::test]
async fn does_not_run_formatters_marked_as_disabled_in_config() {
    let case = Case::new("formatter-disabled");
    let exit = case.script("exit-zero.sh", "exit 0");
    let formatter = case.formatter(&[Some(json!({
        "disabled": { "disabled": true, "command": [exit, "$FILE"], "extensions": [".disabled"] }
    }))]);

    assert!(!formatter.file(&target(&case.project.join("test.disabled"))).await);
}

#[tokio::test]
async fn file_returns_false_when_no_formatter_runs() {
    let case = Case::new("formatter-none");
    let formatter = case.formatter(&[Some(json!(false))]);
    let file = case.file("test.txt", "x");

    assert!(!formatter.file(&target(&file)).await);
    assert_eq!(fs::read_to_string(&file).expect("reads the file"), "x");
}

#[tokio::test]
async fn loads_formatter_state_per_directory() {
    let case = Case::new("formatter-directories");
    let off = case.project.join("off");
    let on = case.project.join("on");
    fs::create_dir_all(&off).expect("creates the disabled directory");
    fs::create_dir_all(&on).expect("creates the enabled directory");
    let exit = case.script("exit-zero.sh", "exit 0");
    let disabled = case.formatter_at(&off, &[Some(json!(false))]);
    let enabled = case
        .formatter_at(&on, &[Some(json!({ "isolated": { "command": [exit, "$FILE"], "extensions": [".isolated"] } }))]);

    assert!(!disabled.file(&target(&off.join("test.isolated"))).await);
    assert!(enabled.file(&target(&on.join("test.isolated"))).await);
}

#[tokio::test]
async fn stops_after_the_first_matching_formatter_succeeds() {
    let case = Case::new("formatter-first-success");
    let first = case.script("first.sh", r#"printf 'A' >> "$1""#);
    let second = case.script("second.sh", r#"printf 'B' >> "$1""#);
    let formatter = case.formatter(&[Some(json!({
        "first": { "command": [first, "$FILE"], "extensions": [".seq"] },
        "second": { "command": [second, "$FILE"], "extensions": [".seq"] }
    }))]);

    assert_format(&formatter, &case.file("test.seq", "x"), Some("xA")).await;
}

#[tokio::test]
async fn tries_the_next_matching_formatter_when_the_first_fails() {
    let case = Case::new("formatter-fallback");
    let first = case.script("first.sh", "exit 1");
    let second = case.script("second.sh", r#"printf 'B' >> "$1""#);
    let formatter = case.formatter(&[Some(json!({
        "first": { "command": [first, "$FILE"], "extensions": [".fallback"] },
        "second": { "command": [second, "$FILE"], "extensions": [".fallback"] }
    }))]);

    assert_format(&formatter, &case.file("test.fallback", "x"), Some("xB")).await;
}

#[tokio::test]
async fn rebuilding_formatter_state_clears_resolved_commands() {
    let case = Case::new("formatter-rebuild");
    let a = case.script("a.sh", r#"printf 'A' >> "$1""#);
    let b = case.script("b.sh", r#"printf 'B' >> "$1""#);
    let formatter = case.formatter(&[Some(json!({
        "reload": { "command": [a, "$FILE"], "extensions": [".reload"] }
    }))]);
    let file = case.file("test.reload", "x");
    assert_format(&formatter, &file, Some("xA")).await;

    formatter.configure(&[entry(&json!({
        "reload": { "command": [b, "$FILE"], "extensions": [".reload"] }
    }))]);
    assert_format(&formatter, &file, Some("xAB")).await;
}

#[tokio::test]
async fn enabling_all_formatters_uses_the_builtin_executable_checks() {
    let case = Case::new("formatter-builtin-path");
    case.script("gofmt", r#"for last do :; done; printf ' formatted' >> "$last""#);
    let formatter = case.formatter(&[Some(json!(true))]);

    assert_format(&formatter, &case.file("main.go", "x"), Some("x formatted")).await;
}

#[tokio::test]
async fn enables_prettier_only_with_a_package_dependency() {
    let case = Case::new("formatter-builtin-prettier");
    let bin = case.project.join("node_modules/.bin");
    fs::create_dir_all(&bin).expect("creates the node bin directory");
    fs::write(bin.join("prettier"), "#!/bin/sh\nfor last do :; done\nprintf ' prettier' >> \"$last\"\n")
        .expect("writes prettier");
    fs::set_permissions(bin.join("prettier"), fs::Permissions::from_mode(0o755)).expect("makes prettier executable");

    fs::write(case.project.join("package.json"), json!({ "dependencies": {} }).to_string()).expect("writes package");
    let disabled = case.formatter(&[Some(json!(true))]);
    let file = case.file("main.js", "x");
    assert_format(&disabled, &file, None).await;

    fs::write(case.project.join("package.json"), json!({ "dependencies": { "prettier": "3" } }).to_string())
        .expect("writes package");
    let enabled = case.formatter(&[Some(json!(true))]);
    assert_format(&enabled, &file, Some("x prettier")).await;
}

#[tokio::test]
async fn enables_biome_only_with_a_config_file() {
    let case = Case::new("formatter-builtin-biome");
    let bin = case.project.join("node_modules/.bin");
    fs::create_dir_all(&bin).expect("creates the node bin directory");
    fs::write(bin.join("biome"), "#!/bin/sh\nfor last do :; done\nprintf ' biome' >> \"$last\"\n")
        .expect("writes biome");
    fs::set_permissions(bin.join("biome"), fs::Permissions::from_mode(0o755)).expect("makes biome executable");

    let disabled = case.formatter(&[Some(json!(true))]);
    let file = case.file("main.js", "x");
    assert_format(&disabled, &file, None).await;

    fs::write(case.project.join("biome.json"), "{}").expect("writes the biome config");
    let enabled = case.formatter(&[Some(json!(true))]);
    assert_format(&enabled, &file, Some("x biome")).await;
}

struct Allow;

impl Authorizer for Allow {
    fn assert<'a>(&'a self, _input: AssertInput) -> BoxFuture<'a, Result<(), PermissionError>> {
        Box::pin(async { Ok(()) })
    }

    fn allows<'a>(&'a self, _input: AssertInput) -> BoxFuture<'a, Result<bool, PermissionError>> {
        Box::pin(async { Ok(true) })
    }
}

fn access(case: &Case) -> Access {
    let directory = case.project.to_string_lossy().into_owned();
    Access::new(directory.clone(), directory, "/home/agent")
}

async fn target_for(access: &Access, path: &str) -> Target {
    access.resolve(ResolveInput { path: path.to_owned(), kind: None, base: None }).await.expect("resolves the target")
}

#[tokio::test]
async fn commit_runs_no_command_without_formatter_config() {
    let case = Case::new("formatter-commit-off");
    case.script("rustfmt", r#"printf ' formatted' >> "$1""#);
    fs::write(case.project.join("main.rs"), "before").expect("writes the file");
    let access = access(&case);
    let target = target_for(&access, "main.rs").await;
    let writer = FileWriter::new(access, Mutation::new(), Arc::new(Allow), Arc::new(case.formatter(&[])));

    let written = writer.commit(&target, "after").await.expect("commits the write");
    assert_eq!(written.formatted, None);
    assert_eq!(fs::read_to_string(case.project.join("main.rs")).expect("reads the file"), "after");
}

#[tokio::test]
async fn commit_runs_the_configured_formatter() {
    let case = Case::new("formatter-commit-on");
    let rustfmt = case.script("rustfmt.sh", r#"printf ' formatted' >> "$1""#);
    let formatter = case.formatter(&[Some(json!({
        "rustfmt": { "command": [rustfmt, "$FILE"], "extensions": [".rs"] }
    }))]);
    fs::write(case.project.join("main.rs"), "before").expect("writes the file");
    let access = access(&case);
    let target = target_for(&access, "main.rs").await;
    let writer = FileWriter::new(access, Mutation::new(), Arc::new(Allow), Arc::new(formatter));

    let written = writer.commit(&target, "after").await.expect("commits the write");
    assert_eq!(written.formatted.as_deref(), Some("after formatted"));
    assert_eq!(fs::read_to_string(case.project.join("main.rs")).expect("reads the file"), "after formatted");
}

#[tokio::test]
async fn writing_a_file_runs_the_configured_formatter() {
    let case = Case::new("formatter-write");
    let script = case.script("write.sh", r#"printf ' formatted' >> "$1""#);
    let formatter = case.formatter(&[Some(json!({
        "custom": { "command": [script, "$FILE"], "extensions": [".txt"] }
    }))]);
    let writer = FileWriter::new(access(&case), Mutation::new(), Arc::new(Allow), Arc::new(formatter));

    let written = writer
        .write(WriteInput { path: "hello.txt".to_owned(), content: "hello".to_owned() }, &Invocation::default())
        .await
        .expect("writes the file");
    assert_eq!(written.formatted.as_deref(), Some("hello formatted"));
}
