use opencode_service::discover::binary_version;
use opencode_service::ts_command::TsCommand;

fn expected() -> String {
    option_env!("OPENCODE_VERSION").unwrap_or("local").to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn the_binary_reports_its_version() {
    let command = TsCommand { program: env!("CARGO_BIN_EXE_opencode-server").into(), args: vec![] };
    assert_eq!(binary_version(&command).await, Some(expected()));
}

#[test]
fn version_prints_the_binary_and_version() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_opencode-server")).arg("--version").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), format!("opencode-server v{}\n", expected()));
}

#[test]
fn no_arguments_prints_usage_and_exits_one() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_opencode-server")).output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "usage: opencode-server <serve|service|--version>\n",
        "{output:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_non_utf8_argument_prints_usage_and_exits_one() {
    use std::os::unix::ffi::OsStrExt;
    let arg = std::ffi::OsStr::from_bytes(b"\xff");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_opencode-server")).arg(arg).output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "usage: opencode-server <serve|service|--version>\n",
        "{output:?}"
    );
}
