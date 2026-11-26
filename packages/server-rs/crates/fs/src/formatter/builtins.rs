use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use serde_json::Value;

use crate::ripgrep::binary;

const WEB_EXTENSIONS: &[&str] = &[
    ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".html", ".htm", ".css", ".scss", ".sass", ".less",
    ".vue", ".svelte", ".json", ".jsonc", ".yaml", ".yml", ".toml", ".xml", ".md", ".mdx", ".graphql", ".gql",
];

pub type Check = Arc<dyn Fn() -> BoxFuture<'static, Option<Vec<String>>> + Send + Sync>;
pub(crate) type Vars = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

pub struct Info {
    pub name: String,
    pub extensions: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub(crate) check: Check,
    cached: Mutex<Option<Option<Vec<String>>>>,
}

impl Info {
    pub fn new(
        name: impl Into<String>,
        extensions: Vec<String>,
        environment: Vec<(String, String)>,
        check: Check,
    ) -> Self {
        Self { name: name.into(), extensions, environment, check, cached: Mutex::new(None) }
    }

    pub async fn command(&self) -> Option<Vec<String>> {
        if let Some(command) = self.cached.lock().expect("the formatter cache is not poisoned").clone() {
            return command;
        }
        let command = (self.check)().await;
        *self.cached.lock().expect("the formatter cache is not poisoned") = Some(command.clone());
        command
    }
}

pub(crate) struct Context {
    pub(crate) directory: PathBuf,
    pub(crate) worktree: PathBuf,
    pub(crate) bin: PathBuf,
    pub(crate) vars: Vars,
}

impl Context {
    fn var(&self, name: &str) -> Option<String> {
        (self.vars)(name).filter(|value| !value.is_empty()).map(|value| value.to_string_lossy().into_owned())
    }

    fn path_extensions(&self) -> Option<String> {
        self.var("PATHEXT").or_else(|| self.var("PathExt"))
    }

    fn find_executable(&self, name: &str) -> Option<PathBuf> {
        let path = self.var("PATH").or_else(|| self.var("Path")).unwrap_or_default();
        let bin = self.bin.to_string_lossy().into_owned();
        let search = if path.is_empty() { bin } else { format!("{path}{}{bin}", binary::delimiter()) };
        binary::which(name, &search, self.path_extensions().as_deref())
    }
}

pub(crate) fn make(context: Arc<Context>) -> Vec<Info> {
    vec![
        executable(&context, "gofmt", &[".go"], &["-w", "$FILE"]),
        executable(
            &context,
            "mix",
            &[".ex", ".exs", ".eex", ".heex", ".leex", ".neex", ".sface"],
            &["format", "$FILE"],
        ),
        oxfmt(&context),
        prettier(&context),
        biome(&context),
        executable(&context, "zig", &[".zig", ".zon"], &["fmt", "$FILE"]),
        clang(&context),
        executable(&context, "ktlint", &[".kt", ".kts"], &["-F", "$FILE"]),
        ruff(&context),
        air(&context),
        uv(&context),
        executable(&context, "rubocop", &[".rb", ".rake", ".gemspec", ".ru"], &["--autocorrect", "$FILE"]),
        executable(&context, "standardrb", &[".rb", ".rake", ".gemspec", ".ru"], &["--fix", "$FILE"]),
        executable(&context, "htmlbeautifier", &[".erb", ".html.erb"], &["$FILE"]),
        executable(&context, "dart", &[".dart"], &["format", "$FILE"]),
        ocamlformat(&context),
        executable(&context, "terraform", &[".tf", ".tfvars"], &["fmt", "$FILE"]),
        executable(&context, "latexindent", &[".tex"], &["-w", "-s", "$FILE"]),
        executable(&context, "gleam", &[".gleam"], &["format", "$FILE"]),
        executable(&context, "shfmt", &[".sh", ".bash"], &["-w", "$FILE"]),
        executable(&context, "nixfmt", &[".nix"], &["$FILE"]),
        executable(&context, "rustfmt", &[".rs"], &["$FILE"]),
        pint(&context),
        executable(&context, "ormolu", &[".hs"], &["-i", "$FILE"]),
        executable(&context, "cljfmt", &[".clj", ".cljs", ".cljc", ".edn"], &["fix", "--quiet", "$FILE"]),
        executable(&context, "dfmt", &[".d"], &["-i", "$FILE"]),
    ]
}

fn info(name: &str, extensions: &[&str], environment: &[(&str, &str)], check: Check) -> Info {
    Info::new(
        name,
        extensions.iter().map(|extension| (*extension).to_owned()).collect(),
        environment.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect(),
        check,
    )
}

fn command(program: &Path, args: &[&str]) -> Vec<String> {
    let mut command = vec![program.to_string_lossy().into_owned()];
    command.extend(args.iter().map(|argument| (*argument).to_owned()));
    command
}

fn executable(context: &Arc<Context>, name: &'static str, extensions: &[&str], args: &[&'static str]) -> Info {
    let context = context.clone();
    let args: Vec<String> = args.iter().map(|argument| (*argument).to_owned()).collect();
    info(
        name,
        extensions,
        &[],
        Arc::new(move || {
            let context = context.clone();
            let args = args.clone();
            Box::pin(async move {
                let program = context.find_executable(name)?;
                let mut command = vec![program.to_string_lossy().into_owned()];
                command.extend(args);
                Some(command)
            })
        }),
    )
}

fn oxfmt(context: &Arc<Context>) -> Info {
    let context = context.clone();
    info(
        "oxfmt",
        &[".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"],
        &[("BUN_BE_BUN", "1")],
        Arc::new(move || {
            let context = context.clone();
            Box::pin(async move {
                for file in find_up(&context, "package.json") {
                    let Some(json) = read_json(&file) else {
                        continue;
                    };
                    if !has_dependency(&json, "oxfmt") {
                        continue;
                    }
                    if let Some(program) = node_bin(&context, "oxfmt") {
                        return Some(command(&program, &["$FILE"]));
                    }
                }
                None
            })
        }),
    )
}

fn prettier(context: &Arc<Context>) -> Info {
    let context = context.clone();
    info(
        "prettier",
        WEB_EXTENSIONS,
        &[("BUN_BE_BUN", "1")],
        Arc::new(move || {
            let context = context.clone();
            Box::pin(async move {
                for file in find_up(&context, "package.json") {
                    let Some(json) = read_json(&file) else {
                        continue;
                    };
                    if !has_dependency(&json, "prettier") {
                        continue;
                    }
                    if let Some(program) = node_bin(&context, "prettier") {
                        return Some(command(&program, &["--write", "$FILE"]));
                    }
                }
                None
            })
        }),
    )
}

fn biome(context: &Arc<Context>) -> Info {
    let context = context.clone();
    info(
        "biome",
        WEB_EXTENSIONS,
        &[("BUN_BE_BUN", "1")],
        Arc::new(move || {
            let context = context.clone();
            Box::pin(async move {
                let configured =
                    ["biome.json", "biome.jsonc"].iter().any(|target| !find_up(&context, target).is_empty());
                if !configured {
                    return None;
                }
                let program = node_bin(&context, "biome")?;
                Some(command(&program, &["format", "--write", "$FILE"]))
            })
        }),
    )
}

fn clang(context: &Arc<Context>) -> Info {
    let context = context.clone();
    info(
        "clang-format",
        &[".c", ".cc", ".cpp", ".cxx", ".c++", ".h", ".hh", ".hpp", ".hxx", ".h++", ".ino", ".C", ".H"],
        &[],
        Arc::new(move || {
            let context = context.clone();
            Box::pin(async move {
                if find_up(&context, ".clang-format").is_empty() {
                    return None;
                }
                let program = context.find_executable("clang-format")?;
                Some(command(&program, &["-i", "$FILE"]))
            })
        }),
    )
}

fn ruff(context: &Arc<Context>) -> Info {
    let context = context.clone();
    info(
        "ruff",
        &[".py", ".pyi"],
        &[],
        Arc::new(move || {
            let context = context.clone();
            Box::pin(async move {
                let program = context.find_executable("ruff")?;
                for config in ["pyproject.toml", "ruff.toml", ".ruff.toml"] {
                    let found = find_up(&context, config);
                    let Some(file) = found.first() else { continue };
                    let text = read_text(file);
                    if config != "pyproject.toml" || text.contains("[tool.ruff]") {
                        return Some(command(&program, &["format", "$FILE"]));
                    }
                }
                for dependency in ["requirements.txt", "pyproject.toml", "Pipfile"] {
                    let found = find_up(&context, dependency);
                    let Some(file) = found.first() else { continue };
                    if read_text(file).contains("ruff") {
                        return Some(command(&program, &["format", "$FILE"]));
                    }
                }
                None
            })
        }),
    )
}

fn air(context: &Arc<Context>) -> Info {
    let context = context.clone();
    info(
        "air",
        &[".R"],
        &[],
        Arc::new(move || {
            let context = context.clone();
            Box::pin(async move {
                let program = context.find_executable("air")?;
                let output = run(&context, &program, &["--help"]).await?;
                if !output.status.success() {
                    return None;
                }
                let stdout = String::from_utf8_lossy(&output.stdout);
                let first = stdout.lines().next().unwrap_or_default();
                if !first.contains("R language") || !first.contains("formatter") {
                    return None;
                }
                Some(command(&program, &["format", "$FILE"]))
            })
        }),
    )
}

fn uv(context: &Arc<Context>) -> Info {
    let context = context.clone();
    info(
        "uv",
        &[".py", ".pyi"],
        &[],
        Arc::new(move || {
            let context = context.clone();
            Box::pin(async move {
                let program = context.find_executable("uv")?;
                let output = run(&context, &program, &["format", "--help"]).await?;
                if !output.status.success() {
                    return None;
                }
                Some(command(&program, &["format", "--", "$FILE"]))
            })
        }),
    )
}

fn ocamlformat(context: &Arc<Context>) -> Info {
    let context = context.clone();
    info(
        "ocamlformat",
        &[".ml", ".mli"],
        &[],
        Arc::new(move || {
            let context = context.clone();
            Box::pin(async move {
                if find_up(&context, ".ocamlformat").is_empty() {
                    return None;
                }
                let program = context.find_executable("ocamlformat")?;
                Some(command(&program, &["-i", "$FILE"]))
            })
        }),
    )
}

fn pint(context: &Arc<Context>) -> Info {
    let context = context.clone();
    info(
        "pint",
        &[".php"],
        &[],
        Arc::new(move || {
            let context = context.clone();
            Box::pin(async move {
                for file in find_up(&context, "composer.json") {
                    let Some(json) = read_json(&file) else {
                        continue;
                    };
                    if has_record_key(&json, "require", "laravel/pint")
                        || has_record_key(&json, "require-dev", "laravel/pint")
                    {
                        return Some(vec!["./vendor/bin/pint".to_owned(), "$FILE".to_owned()]);
                    }
                }
                None
            })
        }),
    )
}

fn find_up(context: &Context, target: &str) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let mut current = context.directory.clone();
    loop {
        let candidate = current.join(target);
        if std::fs::metadata(&candidate).is_ok_and(|metadata| metadata.is_file()) {
            results.push(candidate);
        }
        if current == context.worktree {
            break;
        }
        let Some(parent) = current.parent() else { break };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }
    results
}

fn node_bin(context: &Context, name: &str) -> Option<PathBuf> {
    let mut current = context.directory.clone();
    loop {
        let bin = current.join("node_modules").join(".bin");
        if let Some(found) = binary::which(name, &bin.to_string_lossy(), context.path_extensions().as_deref()) {
            return Some(found);
        }
        if current == context.worktree {
            break;
        }
        let Some(parent) = current.parent() else { break };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }
    None
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn has_dependency(input: &Value, dependency: &str) -> bool {
    has_record_key(input, "dependencies", dependency) || has_record_key(input, "devDependencies", dependency)
}

fn has_record_key(input: &Value, field: &str, key: &str) -> bool {
    input.get(field).and_then(Value::as_object).is_some_and(|object| object.contains_key(key))
}

async fn run(context: &Context, program: &Path, args: &[&str]) -> Option<std::process::Output> {
    tokio::process::Command::new(program)
        .args(args)
        .current_dir(&context.directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()
}
