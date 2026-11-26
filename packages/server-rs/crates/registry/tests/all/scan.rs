use std::fs;
use std::path::{Path, PathBuf};

use opencode_registry::scan::{ScanOptions, glob_up, scan, up};
use serde_json::{Map, Value};

#[test]
fn corpus_matches_expected_output() {
    let path = format!("{}/tests/corpus/scan.json", env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(&path).expect("corpus file is readable");
    let cases: Vec<Value> = serde_json::from_str(&text).expect("corpus is a JSON array");
    assert!(!cases.is_empty(), "corpus case count");

    for case in &cases {
        if case["platform"].as_str() == Some("unix") && !cfg!(unix) {
            continue;
        }
        let name = case["name"].as_str().expect("case name is a string");
        let files = case["files"].as_object().expect("case files is an object");
        let directory = crate::TempDir::new();
        create_fixture(directory.path(), files);
        let actual = run_operation(directory.path(), &case["operation"]);
        let expected: Vec<String> = case["output"]
            .as_array()
            .expect("case output is an array")
            .iter()
            .map(|value| value.as_str().expect("case output entries are strings").to_owned())
            .collect();
        assert_eq!(actual, expected, "case {name}");
    }
}

fn create_fixture(root: &Path, files: &Map<String, Value>) {
    for (name, value) in files {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create the fixture parent directory");
        }
        match value {
            Value::String(content) => {
                fs::write(&path, content).expect("write the fixture file");
            }
            Value::Object(object) if object.get("type").and_then(Value::as_str) == Some("symlink") => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::symlink;
                    let link = object["target"].as_str().expect("symlink target is a string");
                    symlink(link, &path).expect("create the fixture symlink");
                }
                #[cfg(not(unix))]
                {
                    let _ = (object, &path);
                }
            }
            _ => panic!("unsupported fixture entry {name}"),
        }
    }
}

fn run_operation(root: &Path, operation: &Value) -> Vec<String> {
    let kind = operation["kind"].as_str().expect("operation kind is a string");
    let paths = match kind {
        "scan" => {
            let options = &operation["options"];
            let ignore: Vec<&str> = options["ignore"]
                .as_array()
                .expect("scan options ignore is an array")
                .iter()
                .map(|value| value.as_str().expect("ignore entries are strings"))
                .collect();
            scan(
                &root.join(operation["root"].as_str().expect("scan root is a string")),
                operation["pattern"].as_str().expect("scan pattern is a string"),
                ScanOptions {
                    dot: options["dot"].as_bool().expect("scan dot is a boolean"),
                    symlink: options["symlink"].as_bool().expect("scan symlink is a boolean"),
                    include_files_only: options["include_files_only"]
                        .as_bool()
                        .expect("scan include_files_only is a boolean"),
                    ignore: &ignore,
                },
            )
        }
        "up" => {
            let names: Vec<&str> = operation["names"]
                .as_array()
                .expect("up names is an array")
                .iter()
                .map(|value| value.as_str().expect("up names are strings"))
                .collect();
            let start = root.join(operation["start"].as_str().expect("up start is a string"));
            let stop = operation["stop"].as_str().map(|stop| root.join(stop));
            up(&names, &start, stop.as_deref())
        }
        "globUp" => {
            let start = root.join(operation["start"].as_str().expect("globUp start is a string"));
            let stop = operation["stop"].as_str().map(|stop| root.join(stop));
            glob_up(operation["pattern"].as_str().expect("globUp pattern is a string"), &start, stop.as_deref())
        }
        other => panic!("unsupported operation {other}"),
    };
    paths
        .iter()
        .map(|path| path.strip_prefix(root).expect("result is under the fixture").to_string_lossy().replace('\\', "/"))
        .collect()
}

fn write(root: &Path, relative: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("a parent directory")).expect("create the parent directory");
    fs::write(path, "content").expect("write the file");
}

fn relative(paths: &[PathBuf], root: &Path) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.strip_prefix(root).expect("result is under root").to_string_lossy().replace('\\', "/"))
        .collect()
}

#[test]
fn expands_brace_alternation_with_globstar_and_direct_children() {
    let directory = crate::TempDir::new();
    let root = directory.path();
    write(root, "command/a.md");
    write(root, "commands/nested/b.md");
    write(root, "mode/one.md");
    write(root, "mode/nested/two.md");
    write(root, "modes/three.md");

    let found = scan(root, "{command,commands}/**/*.md", ScanOptions { dot: true, ..Default::default() });
    assert_eq!(relative(&found, root), ["command/a.md", "commands/nested/b.md"]);

    let direct = scan(root, "{mode,modes}/*.md", ScanOptions { dot: true, ..Default::default() });
    assert_eq!(relative(&direct, root), ["mode/one.md", "modes/three.md"]);
}

#[test]
fn matches_single_characters_character_classes_and_root_files_through_globstar() {
    let directory = crate::TempDir::new();
    let root = directory.path();
    write(root, "a.md");
    write(root, "ab.md");
    write(root, "b.md");
    write(root, "c.md");
    write(root, "root.md");

    let single = scan(root, "?.md", ScanOptions::default());
    assert_eq!(relative(&single, root), ["a.md", "b.md", "c.md"]);

    let class = scan(root, "[ab]*.md", ScanOptions::default());
    assert_eq!(relative(&class, root), ["a.md", "ab.md", "b.md"]);

    let globstar = scan(root, "**/*.md", ScanOptions::default());
    assert_eq!(relative(&globstar, root), ["a.md", "ab.md", "b.md", "c.md", "root.md"]);
}

#[test]
fn prunes_ignored_directories_and_excludes_dot_entries() {
    let directory = crate::TempDir::new();
    let root = directory.path();
    write(root, "src/a.md");
    write(root, "src/.hidden.md");
    write(root, ".secret/b.md");
    write(root, "node_modules/pkg/p.md");
    write(root, "nested/node_modules/q.md");

    let found = scan(
        root,
        "**/*.md",
        ScanOptions { dot: true, ignore: &["node_modules", "**/node_modules/**"], ..Default::default() },
    );
    assert_eq!(relative(&found, root), [".secret/b.md", "src/.hidden.md", "src/a.md"]);

    let visible = scan(root, "**/*.md", ScanOptions::default());
    assert_eq!(relative(&visible, root), ["nested/node_modules/q.md", "node_modules/pkg/p.md", "src/a.md"]);
}

#[test]
fn up_returns_the_nearest_match_first() {
    let directory = crate::TempDir::new();
    let root = directory.path();
    write(root, "AGENTS.md");
    write(root, "CLAUDE.md");
    write(root, "nested/AGENTS.md");
    write(root, "nested/CLAUDE.md");
    write(root, "nested/deep/keep.txt");

    let found = up(&["AGENTS.md", "CLAUDE.md"], &root.join("nested/deep"), Some(root));
    assert_eq!(relative(&found, root), ["nested/AGENTS.md", "nested/CLAUDE.md", "AGENTS.md", "CLAUDE.md"]);

    let stopped = up(&["AGENTS.md", "CLAUDE.md"], &root.join("nested/deep"), Some(&root.join("nested")));
    assert_eq!(relative(&stopped, root), ["nested/AGENTS.md", "nested/CLAUDE.md"]);
}

#[test]
fn glob_up_returns_the_nearest_level_first() {
    let directory = crate::TempDir::new();
    let root = directory.path();
    write(root, "root.md");
    write(root, "nested/one.md");
    write(root, "nested/deep/two.md");
    write(root, "nested/deep/three.md");

    let found = glob_up("*.md", &root.join("nested/deep"), Some(root));
    assert_eq!(relative(&found, root), ["nested/deep/three.md", "nested/deep/two.md", "nested/one.md", "root.md"]);
}
