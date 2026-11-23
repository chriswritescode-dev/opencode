use std::ffi::OsString;

use opencode_util::temp::tempdir;

use opencode_sidecar::bun;

use crate::support;

#[tokio::test]
async fn an_absolute_opencode_bun_wins_without_being_probed() {
    let dir = tempdir().expect("a directory");
    let bun = dir.path().join("not-a-runtime");
    let vars = support::lookup([("OPENCODE_BUN", bun.clone().into_os_string()), ("PATH", OsString::new())]);

    let found = bun::find(&vars, dir.path()).await.expect("the variable wins");

    assert_eq!(found.bun, bun);
    assert!(!found.opencode);
    assert_eq!(found.version, None);
}

#[tokio::test]
async fn a_relative_opencode_bun_falls_through() {
    let dir = tempdir().expect("a directory");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("a directory");
    let real = support::script(&bin, "bun", "echo 1.4.4");
    let vars = support::lookup([
        ("OPENCODE_BUN", OsString::from("relative/bun")),
        ("PATH", bin.into_os_string()),
        ("HOME", dir.path().join("home").into_os_string()),
    ]);

    let found = bun::find(&vars, &dir.path().join("project")).await.expect("bun on the path");

    assert_eq!(found.bun, real);
    assert!(!found.opencode);
    assert_eq!(found.version.as_deref(), Some("1.4.4"));
}

#[tokio::test]
async fn a_bun_inside_the_project_is_ignored() {
    let dir = tempdir().expect("a directory");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).expect("a directory");
    support::script(&project, "bun", "echo 1.4.4");
    let vars = support::lookup([
        ("PATH", project.clone().into_os_string()),
        ("HOME", dir.path().join("home").into_os_string()),
    ]);

    assert_eq!(bun::find(&vars, &project).await, None);
}

#[tokio::test]
async fn a_relative_path_entry_is_ignored() {
    let dir = tempdir().expect("a directory");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("a directory");
    support::script(&bin, "bun", "echo 1.4.4");
    let relative = relative_to(&bin);
    assert!(relative.is_relative());
    let vars =
        support::lookup([("PATH", relative.into_os_string()), ("HOME", dir.path().join("home").into_os_string())]);

    assert_eq!(bun::find(&vars, &dir.path().join("project")).await, None);
}

fn relative_to(target: &std::path::Path) -> std::path::PathBuf {
    let cwd = std::env::current_dir().expect("a directory");
    let common = cwd.components().zip(target.components()).take_while(|(a, b)| a == b).count();
    let ups = std::iter::repeat_n(OsString::from(".."), cwd.components().count() - common);
    ups.chain(target.components().skip(common).map(|part| part.as_os_str().to_owned())).collect()
}

#[tokio::test]
async fn an_opencode_shim_is_taken_as_its_embedded_bun() {
    let dir = tempdir().expect("a directory");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("a directory");
    let opencode = support::script(
        &bin,
        "opencode",
        "if [ \"$BUN_BE_BUN\" = \"1\" ]; then echo 1.4.4; else echo \"opencode v0.0.0\"; fi",
    );
    let vars = support::lookup([("PATH", bin.into_os_string()), ("HOME", dir.path().join("home").into_os_string())]);

    let found = bun::find(&vars, &dir.path().join("project")).await.expect("the shim's Bun");

    assert!(found.opencode);
    assert_eq!(found.version.as_deref(), Some("1.4.4"));
    assert_eq!(found.bun, std::fs::canonicalize(&opencode).expect("a real path"));
}

#[tokio::test]
async fn an_old_opencode_bun_is_not_taken() {
    let dir = tempdir().expect("a directory");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("a directory");
    support::script(&bin, "opencode", "echo 1.3.0");
    let vars = support::lookup([("PATH", bin.into_os_string()), ("HOME", dir.path().join("home").into_os_string())]);

    assert_eq!(bun::find(&vars, &dir.path().join("project")).await, None);
}

#[test]
fn a_bun_on_the_path_is_found_when_the_directory_is_a_filesystem_root() {
    let dir = tempdir().expect("a directory");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("a directory");
    let bun = support::script(&bin, "bun", "echo 1.4.4");
    let path = bin.into_os_string();

    let found = bun::find_in("bun", &path, std::path::Path::new("/"), None).expect("bun");

    assert_eq!(found, bun);
}

#[tokio::test]
async fn a_bun_on_the_path_is_found() {
    let dir = tempdir().expect("a directory");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("a directory");
    let bun = support::script(&bin, "bun", "echo 1.4.4");
    let vars = support::lookup([("PATH", bin.into_os_string()), ("HOME", dir.path().join("home").into_os_string())]);

    let found = bun::find(&vars, &dir.path().join("project")).await.expect("bun");

    assert_eq!(found.bun, bun);
    assert!(!found.opencode);
    assert_eq!(found.version.as_deref(), Some("1.4.4"));
}
