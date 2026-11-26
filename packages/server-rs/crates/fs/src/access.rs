use std::io::ErrorKind;

use futures_util::future::BoxFuture;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{AgentID, PermissionSource2, PermissionSourceType, SessionID};
use opencode_permission::service::{AssertInput, PermissionError, PermissionService};
use opencode_util::path::Flavor;
use serde_json::Value;

use crate::path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub absolute: String,
    pub id: String,
}

#[derive(Clone, Debug, Default)]
pub struct ResolveInput {
    pub path: String,
    pub kind: Option<Kind>,
    pub base: Option<Place>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExternalDirectory {
    pub action: &'static str,
    pub directory: String,
    pub resource: String,
    pub save: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub absolute: String,
    pub resource: String,
    pub external_directory: Option<ExternalDirectory>,
}

#[derive(Clone, Debug, Default)]
pub struct Invocation {
    pub session_id: String,
    pub agent: Option<String>,
    pub message_id: String,
    pub id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AccessError {
    #[error(transparent)]
    Permission(#[from] PermissionError),
    #[error("Failed at {path}: {cause}")]
    Failed { path: String, cause: String },
}

pub trait Authorizer: Send + Sync {
    fn assert<'a>(&'a self, input: AssertInput) -> BoxFuture<'a, Result<(), PermissionError>>;
    fn allows<'a>(&'a self, input: AssertInput) -> BoxFuture<'a, Result<bool, PermissionError>>;
}

impl Authorizer for PermissionService {
    fn assert<'a>(&'a self, input: AssertInput) -> BoxFuture<'a, Result<(), PermissionError>> {
        Box::pin(async move { PermissionService::assert(self, input).await })
    }

    fn allows<'a>(&'a self, input: AssertInput) -> BoxFuture<'a, Result<bool, PermissionError>> {
        Box::pin(async move { PermissionService::allows(self, input).await })
    }
}

#[derive(Clone)]
pub struct Access {
    working_directory: String,
    project_directory: String,
    home: String,
}

impl Access {
    pub fn new(
        working_directory: impl Into<String>,
        project_directory: impl Into<String>,
        home: impl Into<String>,
    ) -> Self {
        Self {
            working_directory: working_directory.into(),
            project_directory: project_directory.into(),
            home: home.into(),
        }
    }

    pub fn working_directory(&self) -> &str {
        &self.working_directory
    }

    pub async fn resolve(&self, input: ResolveInput) -> Result<Target, AccessError> {
        let flavor = Flavor::NATIVE;
        let from = input.base.as_ref().map_or(self.working_directory.as_str(), |base| base.absolute.as_str());
        let absolute = path::resolve_path(from, &input.path, &self.home);
        let worktree = Flavor::NATIVE.resolve(&[&self.project_directory]);
        let inside_worktree = worktree != flavor.root(&worktree) && path::contains(&worktree, &absolute);
        if path::contains(&self.working_directory, &absolute) || inside_worktree {
            let relative = flavor.relative(&self.working_directory, &absolute);
            let resource = slash(if relative.is_empty() { "." } else { &relative });
            return Ok(Target { absolute, resource, external_directory: None });
        }
        let kind = match input.kind {
            Some(kind) => Some(kind),
            None => kind_of(&absolute).await?,
        };
        let directory = match kind {
            Some(Kind::Directory) => absolute.clone(),
            _ => path::dirname(&absolute),
        };
        let save = project_root(&directory).await.unwrap_or_else(|| directory.clone());
        Ok(Target {
            absolute: absolute.clone(),
            resource: slash(&absolute),
            external_directory: Some(ExternalDirectory {
                action: "external_directory",
                directory: directory.clone(),
                resource: slash(&flavor.join(&[&directory, "*"])),
                save: slash(&flavor.join(&[&save, "*"])),
            }),
        })
    }

    pub async fn authorize_external(
        &self,
        targets: &[&Target],
        context: &Invocation,
        metadata: Option<JsonMap<Value>>,
        authorizer: &dyn Authorizer,
    ) -> Result<(), AccessError> {
        let mut seen = Vec::new();
        let mut resources = Vec::new();
        let mut save = Vec::new();
        for target in targets {
            let Some(external) = &target.external_directory else { continue };
            if seen.contains(&external.resource) {
                continue;
            }
            seen.push(external.resource.clone());
            resources.push(external.resource.clone());
            save.push(external.save.clone());
        }
        if resources.is_empty() {
            return Ok(());
        }
        authorizer.assert(assert_input(context, "external_directory", resources, save, metadata)).await?;
        Ok(())
    }

    pub async fn authorize_read(
        &self,
        file: &str,
        context: &Invocation,
        sibling_of: Option<&Target>,
        authorizer: &dyn Authorizer,
    ) -> Result<Target, AccessError> {
        let target = self
            .resolve(ResolveInput { path: file.to_owned(), kind: sibling_of.map(|_| Kind::File), base: None })
            .await?;
        let sibling =
            sibling_of.is_some_and(|sibling| path::dirname(&target.absolute) == path::dirname(&sibling.absolute));
        if !sibling {
            self.authorize_external(&[&target], context, None, authorizer).await?;
        }
        authorizer
            .assert(assert_input(context, "read", vec![target.resource.clone()], vec![always(&target)], None))
            .await?;
        Ok(target)
    }

    pub async fn authorize_edit(
        &self,
        target: &Target,
        context: &Invocation,
        metadata: Option<JsonMap<Value>>,
        authorizer: &dyn Authorizer,
    ) -> Result<(), AccessError> {
        authorizer
            .assert(assert_input(context, "edit", vec![target.resource.clone()], vec![always(target)], metadata))
            .await?;
        Ok(())
    }
}

pub fn always(_target: &Target) -> String {
    "*".to_owned()
}

pub fn key(target: &Target) -> String {
    key_for(target, "local")
}

pub fn key_for(target: &Target, environment: &str) -> String {
    format!("{}:{environment}:{}", environment.len(), target.absolute)
}

fn assert_input(
    context: &Invocation,
    action: &str,
    resources: Vec<String>,
    save: Vec<String>,
    metadata: Option<JsonMap<Value>>,
) -> AssertInput {
    AssertInput {
        id: None,
        session_id: SessionID(context.session_id.clone()),
        agent: context.agent.clone().map(AgentID),
        action: action.to_owned(),
        resources,
        save: Some(save),
        metadata,
        source: Some(PermissionSource2 {
            r#type: PermissionSourceType::Tool,
            message_id: context.message_id.clone(),
            id: context.id.clone(),
        }),
        unattended: None,
    }
}

async fn kind_of(path: &str) -> Result<Option<Kind>, AccessError> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) if metadata.is_dir() => Ok(Some(Kind::Directory)),
        Ok(_) => Ok(Some(Kind::File)),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => Ok(None),
        Err(error) => Err(AccessError::Failed { path: path.to_owned(), cause: error.to_string() }),
    }
}

async fn project_root(directory: &str) -> Option<String> {
    let flavor = Flavor::NATIVE;
    let mut current = flavor.normalize(directory);
    loop {
        for marker in [".git", ".hg"] {
            let candidate = flavor.join(&[&current, marker]);
            if tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
                return Some(current);
            }
        }
        let parent = path::dirname(&current);
        if parent == current {
            return None;
        }
        current = parent;
    }
}

fn slash(value: &str) -> String {
    value.replace('\\', "/")
}
