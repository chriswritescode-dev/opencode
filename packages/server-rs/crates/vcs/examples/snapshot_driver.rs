use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use opencode_git::Git;
use opencode_location::environment::local::LocalEnvironment;
use opencode_location::location::{LocationRef, boot_location};
use opencode_location::project::ProjectService;
use opencode_storage::{Database, DatabaseLocation};
use opencode_vcs::snapshot::{DiffInput, RestoreInput, Snapshot};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

const ERROR_RESPONSE: &str = r#"{"error":true}"#;

struct Driver {
    git: Arc<Git>,
    environment: LocalEnvironment,
    projects: HashMap<String, Arc<ProjectService>>,
    snapshots: HashMap<(String, String), Arc<Snapshot>>,
}

impl Driver {
    async fn handle(&mut self, line: &str) -> Option<Value> {
        let request = match serde_json::from_str::<Value>(line) {
            Ok(request) => request,
            Err(_) => return Some(error_response()),
        };
        let op = match request.get("op").and_then(Value::as_str) {
            Some(op) => op,
            None => return Some(error_response()),
        };
        match op {
            "close" => None,
            "capture" => Some(self.capture(&request).await),
            "files" => Some(self.files(&request).await),
            "diff" => Some(self.diff(&request).await),
            "restore" => Some(self.restore(&request).await),
            _ => Some(error_response()),
        }
    }

    async fn capture(&mut self, request: &Value) -> Value {
        let Some(snapshot) = self.snapshot(request).await else {
            return error_response();
        };
        match snapshot.capture().await {
            Some(tree) => serde_json::json!({ "ok": tree }),
            None => serde_json::json!({ "ok": null }),
        }
    }

    async fn files(&mut self, request: &Value) -> Value {
        let Some(snapshot) = self.snapshot(request).await else {
            return error_response();
        };
        let (Some(from), Some(to)) = (string(request, "from"), string(request, "to")) else {
            return error_response();
        };
        match snapshot.files(&from, &to).await {
            Ok(files) => serde_json::json!({ "ok": files }),
            Err(_) => error_response(),
        }
    }

    async fn diff(&mut self, request: &Value) -> Value {
        let Some(snapshot) = self.snapshot(request).await else {
            return error_response();
        };
        let (Some(from), Some(to)) = (string(request, "from"), string(request, "to")) else {
            return error_response();
        };
        let paths = match request.get("paths") {
            None | Some(Value::Null) => None,
            Some(Value::Array(items)) => {
                let paths = items.iter().map(|item| item.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>();
                let Some(paths) = paths else {
                    return error_response();
                };
                Some(paths)
            }
            Some(_) => return error_response(),
        };
        let diffs =
            match snapshot.diff(DiffInput { from: &from, to: &to, context: None, paths: paths.as_deref() }).await {
                Ok(diffs) => diffs,
                Err(_) => return error_response(),
            };
        let entries = diffs
            .into_iter()
            .map(|diff| {
                let mut entry = serde_json::json!({
                    "file": diff.file,
                    "status": diff.status,
                    "additions": diff.additions,
                    "deletions": diff.deletions,
                    "patch": diff.patch,
                });
                if diff.more == Some(true) {
                    entry["more"] = Value::Bool(true);
                }
                entry
            })
            .collect::<Vec<_>>();
        serde_json::json!({ "ok": entries })
    }

    async fn restore(&mut self, request: &Value) -> Value {
        let Some(snapshot) = self.snapshot(request).await else {
            return error_response();
        };
        let Some(entries) = request.get("files").and_then(Value::as_array) else {
            return error_response();
        };
        let mut files = Vec::with_capacity(entries.len());
        for entry in entries {
            let Some(pair) = entry.as_array().filter(|pair| pair.len() == 2) else {
                return error_response();
            };
            let (Some(file), Some(tree)) = (pair[0].as_str(), pair[1].as_str()) else {
                return error_response();
            };
            files.push((file.to_owned(), tree.to_owned()));
        }
        match snapshot.restore(RestoreInput { files: &files }).await {
            Ok(()) => serde_json::json!({ "ok": null }),
            Err(_) => error_response(),
        }
    }

    async fn snapshot(&mut self, request: &Value) -> Option<Arc<Snapshot>> {
        let data = string(request, "data")?;
        let directory = string(request, "directory")?;
        let key = (data.clone(), directory.clone());
        if let Some(snapshot) = self.snapshots.get(&key).cloned() {
            return Some(snapshot);
        }
        let cached = self.projects.get(&data).cloned();
        let project = match cached {
            Some(project) => project,
            None => {
                let project = Arc::new(create_project(&self.git, &data).await?);
                self.projects.insert(data.clone(), project.clone());
                project
            }
        };
        let info = boot_location(&LocationRef::new(directory.clone(), None), &project, &self.environment).await.ok()?;
        let snapshot = Arc::new(Snapshot::new(self.git.clone(), &info, data));
        self.snapshots.insert(key, snapshot.clone());
        Some(snapshot)
    }
}

async fn create_project(git: &Arc<Git>, data: &str) -> Option<ProjectService> {
    tokio::fs::create_dir_all(data).await.ok()?;
    let data = Path::new(data);
    let (database, _) = Database::open(DatabaseLocation::File(data.join("opencode.db")), data).ok()?;
    Some(ProjectService::new(Arc::new(database), git.clone()))
}

fn string(request: &Value, key: &str) -> Option<String> {
    request.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn error_response() -> Value {
    serde_json::json!({ "error": true })
}

async fn write_response(writer: &mut BufWriter<tokio::io::Stdout>, response: &Value) -> std::io::Result<()> {
    let text = serde_json::to_string(response).unwrap_or_else(|_| ERROR_RESPONSE.to_owned());
    writer.write_all(text.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

#[tokio::main]
async fn main() {
    let git = Arc::new(Git::new(Arc::new(|name: &str| std::env::var_os(name))));
    let mut driver =
        Driver { git, environment: LocalEnvironment::new(), projects: HashMap::new(), snapshots: HashMap::new() };
    let mut reader = BufReader::new(tokio::io::stdin()).lines();
    let mut writer = BufWriter::new(tokio::io::stdout());
    loop {
        let line = match reader.next_line().await {
            Ok(Some(line)) => line,
            _ => break,
        };
        let Some(response) = driver.handle(&line).await else {
            break;
        };
        if write_response(&mut writer, &response).await.is_err() {
            break;
        }
    }
    let _ = writer.flush().await;
}
