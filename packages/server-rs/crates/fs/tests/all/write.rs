use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use opencode_fs::access::{Access, Authorizer, Invocation, ResolveInput, Target};
use opencode_fs::mutation::Mutation;
use opencode_fs::write::{FileWriter, FormatHook, NoFormat, WriteInput};
use opencode_permission::service::{AssertInput, PermissionError};

use crate::fixture::TempDir;

struct Allow {
    requests: Mutex<Vec<String>>,
}

impl Allow {
    fn new() -> Self {
        Self { requests: Mutex::new(Vec::new()) }
    }
}

impl Authorizer for Allow {
    fn assert<'a>(&'a self, input: AssertInput) -> BoxFuture<'a, Result<(), PermissionError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(input.action);
            Ok(())
        })
    }

    fn allows<'a>(&'a self, _input: AssertInput) -> BoxFuture<'a, Result<bool, PermissionError>> {
        Box::pin(async { Ok(true) })
    }
}

struct Rewrite;

impl FormatHook for Rewrite {
    fn file<'a>(&'a self, target: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            std::fs::write(&target.absolute, "\u{feff}after formatted").expect("formats the file");
            true
        })
    }
}

fn context() -> Invocation {
    Invocation {
        session_id: "ses_fs_corpus".to_owned(),
        agent: Some("build".to_owned()),
        message_id: "msg_fs_corpus".to_owned(),
        id: "call-fs-corpus".to_owned(),
    }
}

fn access(directory: &std::path::Path) -> Access {
    Access::new(directory.to_string_lossy().into_owned(), directory.to_string_lossy().into_owned(), "/home/agent")
}

#[tokio::test]
async fn writes_a_whole_file_and_asserts_edit_approval() {
    let temp = TempDir::new("write-file");
    let directory = temp.path().join("case");
    std::fs::create_dir_all(&directory).unwrap();
    let authorizer = Arc::new(Allow::new());
    let writer = FileWriter::new(
        access(&directory),
        Mutation::new(),
        Arc::clone(&authorizer) as Arc<dyn Authorizer>,
        Arc::new(NoFormat),
    );

    let written = writer
        .write(WriteInput { path: "src/hello.txt".to_owned(), content: "hello".to_owned() }, &context())
        .await
        .expect("writes the file");
    assert_eq!(written.resource, "src/hello.txt");
    assert_eq!(written.formatted, None);
    assert_eq!(std::fs::read_to_string(directory.join("src/hello.txt")).unwrap(), "hello");
    assert_eq!(authorizer.requests.lock().unwrap().as_slice(), ["edit"]);
}

#[tokio::test]
async fn commit_reruns_the_bom_sync_after_a_formatter_rewrites_the_file() {
    let temp = TempDir::new("write-format");
    let directory = temp.path().join("case");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("hello.txt"), "before").unwrap();
    let resolved = access(&directory);
    let target = resolved
        .resolve(ResolveInput { path: "hello.txt".to_owned(), kind: None, base: None })
        .await
        .expect("resolves the target");
    let writer = FileWriter::new(access(&directory), Mutation::new(), Arc::new(Allow::new()), Arc::new(Rewrite));

    let written = writer.commit(&target, "after").await.expect("commits the write");
    assert_eq!(written.formatted.as_deref(), Some("after formatted"));
    assert_eq!(std::fs::read_to_string(directory.join("hello.txt")).unwrap(), "after formatted");
}
