use opencode_location::environment::local::LocalEnvironment;
use opencode_location::environment::memory::MemoryEnvironment;
use opencode_location::environment::unavailable;
use opencode_location::environment::{
    CommandSpec, DirEntry, Environment, EnvironmentError, FileType, Spawner, type_following,
};

#[tokio::test]
async fn type_following_reports_kinds_and_dangling_links() {
    let environment = MemoryEnvironment::new("type-following");
    let files = environment.files();
    files.mkdir("/directory").await.expect("creates the directory");
    files.write("/file", b"data").await.expect("writes the file");
    environment.symlink("/directory", "/directory-link").expect("creates the directory link");
    environment.symlink("/file", "/file-link").expect("creates the file link");
    environment.symlink("/missing", "/dangling-link").expect("creates the dangling link");
    assert_eq!(type_following(files, "/directory").await.expect("stats the directory"), FileType::Directory);
    assert_eq!(type_following(files, "/file-link").await.expect("follows the file link"), FileType::File);
    let error = type_following(files, "/dangling-link").await.expect_err("fails for the dangling link");
    assert!(matches!(error, EnvironmentError::NotFound { .. }));
}

#[tokio::test]
async fn memory_round_trip() {
    let environment = MemoryEnvironment::new("memory");
    let files = environment.files();
    files.write("/root/hello.txt", b"hello").await.expect("writes the file");
    let read = files.read("/root/hello.txt", None).await.expect("reads the file");
    assert_eq!(read.bytes, b"hello");
    assert_eq!(read.info.kind, FileType::File);
    assert_eq!(read.info.size, 5);
    assert_eq!(files.stat("/root/hello.txt").await.expect("stats the file"), read.info);
    let entries = files.list("/root").await.expect("lists the directory");
    assert_eq!(entries, vec![DirEntry { name: "hello.txt".to_string(), kind: FileType::File }]);
    environment.symlink("/root/hello.txt", "/hello-link").expect("creates the link");
    let link = files.stat("/hello-link").await.expect("stats the link");
    assert_eq!(link.kind, FileType::Symlink);
    assert_eq!(link.size, "/root/hello.txt".len() as u64);
    assert_eq!(files.read("/hello-link", None).await.expect("follows the link").bytes, b"hello");
    files.mkdir("/links").await.expect("creates the link directory");
    environment.symlink("/root/hello.txt", "/links/absolute").expect("creates an absolute link");
    assert_eq!(files.read("/links/absolute", None).await.expect("follows the absolute link").bytes, b"hello");
    let missing_read = files.read("/missing", None).await.expect_err("fails to read the missing path");
    assert!(matches!(missing_read, EnvironmentError::NotFound { .. }));
    let missing_stat = files.stat("/missing").await.expect_err("fails to stat the missing path");
    assert!(matches!(missing_stat, EnvironmentError::NotFound { .. }));
}

#[tokio::test]
async fn local_round_trip() {
    let directory = crate::TempDir::new();
    let root = directory.path();
    let environment = LocalEnvironment::new();
    let files = environment.files();
    files.mkdir(&root).await.expect("creates the temp directory");
    let target = std::path::Path::new(&root).join("hello.txt").to_string_lossy().into_owned();
    let missing = std::path::Path::new(&root).join("missing").to_string_lossy().into_owned();
    files.write(&target, b"hello").await.expect("writes the file");
    let read = files.read(&target, None).await.expect("reads the file");
    assert_eq!(read.bytes, b"hello");
    assert_eq!(read.info.kind, FileType::File);
    assert_eq!(read.info.size, 5);
    assert_eq!(files.stat(&target).await.expect("stats the file"), read.info);
    let entries = files.list(&root).await.expect("lists the directory");
    assert!(entries.contains(&DirEntry { name: "hello.txt".to_string(), kind: FileType::File }));
    let error = files.read(&missing, None).await.expect_err("fails to read the missing path");
    assert!(matches!(error, EnvironmentError::NotFound { .. }));
    let error = files.stat(&missing).await.expect_err("fails to stat the missing path");
    assert!(matches!(error, EnvironmentError::NotFound { .. }));
}

#[tokio::test]
async fn unavailable_spawner_reports_no_execution_plane() {
    let command = CommandSpec { program: "echo".to_string(), args: vec!["hello".to_string()], ..Default::default() };
    let error = unavailable::spawner().spawn(&command).await.expect_err("fails to spawn");
    match error {
        EnvironmentError::Failed { cause, .. } => assert!(cause.contains("location has no execution plane")),
        _ => panic!("expected a failed environment error"),
    }
}
