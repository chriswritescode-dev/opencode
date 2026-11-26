use std::sync::Mutex;

use futures_util::future::BoxFuture;
use opencode_contract::codec::JsonMap;
use opencode_fs::access::{Access, AccessError, Authorizer, Invocation, Kind, ResolveInput, Target};
use opencode_permission::service::{AssertInput, PermissionError};
use serde_json::{Value, json};

use crate::fixture::{self, TempDir};

struct Recording {
    requests: Mutex<Vec<Value>>,
    deny: Option<String>,
}

impl Recording {
    fn new(deny: Option<&str>) -> Self {
        Self { requests: Mutex::new(Vec::new()), deny: deny.map(str::to_owned) }
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

impl Authorizer for Recording {
    fn assert<'a>(&'a self, input: AssertInput) -> BoxFuture<'a, Result<(), PermissionError>> {
        Box::pin(async move {
            let metadata = input.metadata.as_ref().map(|metadata| {
                Value::Object(metadata.0.iter().map(|(key, value)| (key.clone(), value.clone())).collect())
            });
            let mut request = serde_json::Map::new();
            request.insert("action".to_owned(), Value::String(input.action.clone()));
            request.insert("resources".to_owned(), json!(input.resources));
            request.insert("save".to_owned(), json!(input.save));
            request.insert("sessionID".to_owned(), Value::String(input.session_id.0.clone()));
            request.insert("agent".to_owned(), json!(input.agent.as_ref().map(|agent| agent.0.clone())));
            request.insert(
                "source".to_owned(),
                match &input.source {
                    Some(source) => json!({
                        "type": "tool",
                        "messageID": source.message_id,
                        "id": source.id,
                    }),
                    None => Value::Null,
                },
            );
            if let Some(metadata) = metadata {
                request.insert("metadata".to_owned(), metadata);
            }
            self.requests.lock().unwrap().push(Value::Object(request));
            if self.deny.as_deref() == Some(input.action.as_str()) {
                return Err(PermissionError::Blocked {
                    rules: Vec::new(),
                    permission: input.action,
                    resources: input.resources,
                    reason: None,
                });
            }
            Ok(())
        })
    }

    fn allows<'a>(&'a self, _input: AssertInput) -> BoxFuture<'a, Result<bool, PermissionError>> {
        Box::pin(async { Ok(true) })
    }
}

#[tokio::test]
async fn access_corpus_matches_expected_output() {
    for case in fixture::cases("access") {
        let name = fixture::field(&case, "name");
        let temp = TempDir::new(name);
        let directory = temp.path().join(name);
        std::fs::create_dir_all(&directory).expect("creates the case directory");
        fixture::materialize(&directory, &case["fixture"]);
        let data = fixture::template_json(&case, &temp.text());
        let location = directory.join(case["location"].as_str().unwrap_or("."));
        let project = match case["project"].as_str() {
            Some("/") => "/".to_owned(),
            Some(project) => directory.join(project).to_string_lossy().into_owned(),
            None => location.to_string_lossy().into_owned(),
        };
        let access = Access::new(location.to_string_lossy().into_owned(), project, "/home/agent");
        let recording = Recording::new(case["deny"].as_str());
        let context = Invocation {
            session_id: "ses_fs_corpus".to_owned(),
            agent: Some("build".to_owned()),
            message_id: "msg_fs_corpus".to_owned(),
            id: "call-fs-corpus".to_owned(),
        };
        let mut targets: Vec<Target> = Vec::new();
        let mut results: Vec<Value> = Vec::new();
        let mut error: Option<&'static str> = None;
        for op in case["ops"].as_array().expect("ops is an array") {
            let operation = fixture::field(op, "op");
            let path = match op["absoluteInput"].as_str() {
                Some(input) => directory.join(input).to_string_lossy().into_owned(),
                None => op["path"].as_str().unwrap_or_default().to_owned(),
            };
            let outcome: Result<Option<Value>, AccessError> = match operation {
                "resolve" => access
                    .resolve(ResolveInput {
                        path,
                        kind: op["kind"].as_str().map(|kind| match kind {
                            "directory" => Kind::Directory,
                            _ => Kind::File,
                        }),
                        base: None,
                    })
                    .await
                    .map(|target| {
                        let value = target_value(&target);
                        targets.push(target);
                        Some(value)
                    }),
                "authorizeExternal" => {
                    let selected: Vec<&Target> = op["targets"]
                        .as_array()
                        .expect("targets is an array")
                        .iter()
                        .map(|index| &targets[index.as_u64().expect("target index") as usize])
                        .collect();
                    let metadata =
                        if op["metadata"].as_bool() == Some(true) { metadata_map(&data["metadata"]) } else { None };
                    access.authorize_external(&selected, &context, metadata, &recording).await.map(|()| None)
                }
                "authorizeRead" => {
                    let sibling = op["siblingOf"].as_u64().map(|index| targets[index as usize].clone());
                    access.authorize_read(&path, &context, sibling.as_ref(), &recording).await.map(|target| {
                        let value = target_value(&target);
                        targets.push(target);
                        Some(value)
                    })
                }
                other => panic!("case {name}: unknown op {other}"),
            };
            match outcome {
                Ok(Some(value)) => results.push(value),
                Ok(None) => {}
                Err(cause) => {
                    error = Some(access_error_tag(&cause));
                    break;
                }
            }
        }
        let expected = &data["expected"];
        assert_eq!(Value::Array(results), expected["targets"], "case {name}: targets");
        assert_eq!(Value::Array(recording.requests()), expected["requests"], "case {name}: requests");
        if let Some(expected_error) = expected["error"].as_str() {
            assert_eq!(error, Some(expected_error), "case {name}: error");
        } else {
            assert_eq!(error, None, "case {name}: error");
        }
    }
}

fn target_value(target: &Target) -> Value {
    let external = match &target.external_directory {
        Some(external) => json!({
            "action": external.action,
            "directory": external.directory,
            "resource": external.resource,
            "save": external.save,
        }),
        None => Value::Null,
    };
    json!({
        "absolute": target.absolute,
        "resource": target.resource,
        "externalDirectory": external,
    })
}

fn metadata_map(value: &Value) -> Option<JsonMap<Value>> {
    match value {
        Value::Object(map) => Some(JsonMap(map.iter().map(|(key, value)| (key.clone(), value.clone())).collect())),
        _ => None,
    }
}

fn access_error_tag(error: &AccessError) -> &'static str {
    match error {
        AccessError::Permission(PermissionError::Blocked { .. }) => "Permission.BlockedError",
        AccessError::Permission(PermissionError::Declined) => "Permission.Declined",
        AccessError::Permission(_) => "Permission.Error",
        AccessError::Failed { .. } => "Environment.Failed",
    }
}
