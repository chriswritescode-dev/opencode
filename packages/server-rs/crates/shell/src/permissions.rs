use std::future::Future;

use opencode_contract::generated::{PermissionEffect, PermissionRule};
use opencode_fs::access::{
    Access, AccessError, ExternalDirectoryRequest, Kind, Place, ResolveInput, Target, external_directory_request,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::parse;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub action: String,
    pub resources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "Portable shell scanner cannot analyze command: {reason}. A shell deny rule is configured and the command cannot be checked against it; rewrite it without eval or other dynamic execution."
    )]
    Unverifiable { reason: String },
    #[error(transparent)]
    Access(#[from] AccessError),
}

pub struct Input<'a> {
    pub command: &'a str,
    pub program: &'a str,
    pub target: &'a Target,
    pub once: bool,
}

pub trait DirectoryResolver {
    fn resolve(&self, directory: &str, base: &Target) -> impl Future<Output = Result<Target, AccessError>> + Send;
}

impl DirectoryResolver for Access {
    async fn resolve(&self, directory: &str, base: &Target) -> Result<Target, AccessError> {
        Access::resolve(
            self,
            ResolveInput {
                path: directory.to_owned(),
                kind: Some(Kind::Directory),
                base: Some(Place { absolute: base.absolute.clone(), id: base.resource.clone() }),
            },
        )
        .await
    }
}

pub async fn requests<R: DirectoryResolver>(
    resolver: &R,
    input: &Input<'_>,
    rules: &[PermissionRule],
) -> Result<Vec<Request>, Error> {
    let parsed = match parse::scan(input.command, input.program, &input.target.absolute) {
        Ok(parsed) => parsed,
        Err(error) => {
            if rules.iter().any(|rule| {
                rule.effect == PermissionEffect::Deny
                    && opencode_config::wildcard::matches("shell", &rule.action, false)
            }) {
                return Err(Error::Unverifiable { reason: error.reason });
            }
            return Ok(vec![
                Request {
                    action: "external_directory".to_owned(),
                    resources: vec!["*".to_owned()],
                    save: None,
                    metadata: None,
                },
                Request {
                    action: "shell".to_owned(),
                    resources: vec![input.command.to_owned()],
                    save: None,
                    metadata: Some(Map::from_iter([("reason".to_owned(), Value::String(error.reason))])),
                },
            ]);
        }
    };

    let mut directories = Vec::with_capacity(parsed.directories.len());
    for directory in &parsed.directories {
        directories.push(resolver.resolve(directory, input.target).await?);
    }
    let external = external_request(input.target, &directories);
    let command = if !parsed.commands.is_empty() || input.once {
        Some(Request {
            action: "shell".to_owned(),
            resources: if parsed.commands.is_empty() {
                vec![input.command.to_owned()]
            } else {
                parsed.commands.iter().map(|command| command.resource.clone()).collect()
            },
            save: Some(parsed.commands.iter().map(|command| command.save.clone()).collect()),
            metadata: None,
        })
    } else {
        None
    };
    Ok([external, command]
        .into_iter()
        .flatten()
        .map(|request| Request { save: if input.once { None } else { request.save }, ..request })
        .collect())
}

fn external_request(target: &Target, directories: &[Target]) -> Option<Request> {
    let request: ExternalDirectoryRequest =
        external_directory_request(std::iter::once(target).chain(directories.iter()))?;
    Some(Request {
        action: "external_directory".to_owned(),
        resources: request.resources,
        save: Some(request.save),
        metadata: None,
    })
}
