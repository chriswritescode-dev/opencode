use std::sync::Arc;

use futures_util::future::BoxFuture;
use opencode_contract::codec::JsonMap;
use serde_json::Value;

use crate::FsError;
use crate::access::{Access, AccessError, Authorizer, Invocation, Kind, ResolveInput, Target};
use crate::bom;
use crate::diff::{self, FileDiffStatus};
use crate::mutation::{Mutation, TextWriteResult};

#[derive(Clone, Debug)]
pub struct WriteInput {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    pub operation: &'static str,
    pub target: String,
    pub resource: String,
    pub existed: bool,
    pub bom: bool,
    pub formatted: Option<String>,
}

pub trait FormatHook: Send + Sync {
    fn file<'a>(&'a self, target: &'a Target) -> BoxFuture<'a, bool>;
}

pub struct NoFormat;

impl FormatHook for NoFormat {
    fn file<'a>(&'a self, _target: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error(transparent)]
    Access(#[from] AccessError),
    #[error(transparent)]
    Fs(#[from] FsError),
}

#[derive(Clone)]
pub struct FileWriter {
    access: Access,
    mutation: Mutation,
    authorizer: Arc<dyn Authorizer>,
    format: Arc<dyn FormatHook>,
}

impl FileWriter {
    pub fn new(
        access: Access,
        mutation: Mutation,
        authorizer: Arc<dyn Authorizer>,
        format: Arc<dyn FormatHook>,
    ) -> Self {
        Self { access, mutation, authorizer, format }
    }

    pub async fn target(&self, file: &str, context: &Invocation) -> Result<Target, AccessError> {
        let target =
            self.access.resolve(ResolveInput { path: file.to_owned(), kind: Some(Kind::File), base: None }).await?;
        self.access.authorize_external(&[&target], context, None, self.authorizer.as_ref()).await?;
        Ok(target)
    }

    pub async fn authorize(
        &self,
        target: &Target,
        context: &Invocation,
        preview: Option<Value>,
    ) -> Result<(), AccessError> {
        let metadata = preview.map(|preview| JsonMap(vec![("files".to_owned(), Value::Array(vec![preview]))]));
        self.access.authorize_edit(target, context, metadata, self.authorizer.as_ref()).await
    }

    pub async fn commit(&self, target: &Target, content: &str) -> Result<Written, WriteError> {
        let written = self.mutation.write_text_preserving_bom(target, content).await?;
        if !self.format.file(target).await {
            return Ok(written.into());
        }
        let formatted = self.mutation.sync_text_bom(target, written.bom).await?;
        Ok(Written { formatted: Some(formatted), ..written.into() })
    }

    pub async fn write(&self, input: WriteInput, context: &Invocation) -> Result<Written, WriteError> {
        let target = self.target(&input.path, context).await?;
        let current = self.mutation.read_optional_text(&target).await?;
        let (_, text) = bom::split(&input.content);
        let preview = diff::file_diff(
            &target.resource,
            current.as_ref().map_or("", |current| current.text.as_str()),
            text,
            if current.is_some() { FileDiffStatus::Modified } else { FileDiffStatus::Added },
        );
        self.authorize(&target, context, Some(serde_json::to_value(preview).expect("the file diff serializes")))
            .await?;
        self.commit(&target, &input.content).await
    }
}

impl From<TextWriteResult> for Written {
    fn from(written: TextWriteResult) -> Self {
        Self {
            operation: written.operation,
            target: written.target,
            resource: written.resource,
            existed: written.existed,
            bom: written.bom,
            formatted: None,
        }
    }
}
