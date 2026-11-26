use std::convert::Infallible;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use opencode_config::paths::GlobalPaths;
use opencode_fs::filesystem::Entry;
use opencode_fs::ripgrep::binary::{Binary, Extension, Release, releases, verify};
use opencode_fs::ripgrep::{Error, FindInput, GlobInput, GrepInput, Ripgrep, Submatch};

use super::fixture::TempDir;

fn globripgrep() -> Ripgrep {
    Ripgrep::new(Binary::new(GlobalPaths::from_env().expect("global paths"), |name| std::env::var_os(name), releases()))
}

fn write(path: &Path, contents: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("creates the parent directory");
    }
    fs::write(path, contents).expect("writes the file");
}

fn paths_of(entries: &[Entry]) -> Vec<String> {
    entries.iter().map(|entry| entry.path.clone()).collect()
}

fn sorted(entries: &[Entry]) -> Vec<String> {
    let mut paths = paths_of(entries);
    paths.sort();
    paths
}

#[tokio::test]
async fn glob_keeps_partial_results_for_a_malformed_ignore_glob_and_a_broken_symlink() {
    if cfg!(windows) {
        return;
    }
    let tmp = TempDir::new("rg-glob-partial");
    write(&tmp.path().join("visible.ts"), "needle\n");
    write(&tmp.path().join(".ignore"), "[z-a]\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink("missing", tmp.path().join("broken")).expect("creates the broken symlink");

    let files = globripgrep()
        .glob(GlobInput {
            cwd: tmp.text(),
            pattern: "*.ts".to_string(),
            limit: 10,
            follow: true,
            ..GlobInput::default()
        })
        .await
        .expect("globs");
    assert_eq!(paths_of(&files), ["visible.ts"]);
}

#[tokio::test]
async fn glob_honors_hidden_before_limit() {
    for (hidden, limits) in [(false, vec![1, 10]), (true, vec![10])] {
        for limit in limits {
            let tmp = TempDir::new("rg-glob-hidden");
            for file in ["src/visible.ts", ".hidden.ts", "src/.hidden.ts", ".hidden/nested.ts", ".git/config.ts"] {
                write(&tmp.path().join(file), "needle\n");
            }
            let files = globripgrep()
                .glob(GlobInput {
                    cwd: tmp.text(),
                    pattern: "**/*.ts".to_string(),
                    limit,
                    hidden,
                    ..GlobInput::default()
                })
                .await
                .expect("globs");
            let expected = if hidden {
                vec![".hidden.ts", ".hidden/nested.ts", "src/.hidden.ts", "src/visible.ts"]
            } else {
                vec!["src/visible.ts"]
            };
            assert_eq!(sorted(&files), expected, "hidden={hidden} limit={limit}");
        }
    }
}

#[tokio::test]
async fn glob_returns_files_as_an_array() {
    let tmp = TempDir::new("rg-glob-array");
    write(&tmp.path().join("src/match.ts"), "needle\n");
    let files = globripgrep()
        .glob(GlobInput { cwd: tmp.text(), pattern: "**/*.ts".to_string(), limit: 10, ..GlobInput::default() })
        .await
        .expect("globs");
    assert_eq!(paths_of(&files), ["src/match.ts"]);
}

#[tokio::test]
async fn grep_filters_by_include() {
    let tmp = TempDir::new("rg-grep-include");
    write(&tmp.path().join("src/match.ts"), "needle\n");
    write(&tmp.path().join("src/skip.txt"), "needle\n");
    let matches = globripgrep()
        .grep(GrepInput {
            cwd: tmp.text(),
            pattern: "needle".to_string(),
            include: Some("*.ts".to_string()),
            limit: 10,
            ..GrepInput::default()
        })
        .await
        .expect("greps");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].entry.path, "src/match.ts");
    assert_eq!(matches[0].submatches[0].text, "needle");
}

#[tokio::test]
async fn grep_reads_lines_that_are_not_valid_utf8() {
    let tmp = TempDir::new("rg-grep-utf8");
    write(&tmp.path().join("utf8.txt"), "needle\n");
    write(&tmp.path().join("latin1.txt"), b"needle caf\xe9\n");
    let mut matches = globripgrep()
        .grep(GrepInput { cwd: tmp.text(), pattern: "needle".to_string(), limit: 10, ..GrepInput::default() })
        .await
        .expect("greps");
    matches.sort_by(|left, right| left.entry.path.cmp(&right.entry.path));
    let rows = matches
        .iter()
        .map(|matched| (matched.entry.path.clone(), matched.text.clone(), matched.submatches[0].text.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            ("latin1.txt".to_string(), "needle caf\u{FFFD}\n".to_string(), "needle".to_string()),
            ("utf8.txt".to_string(), "needle\n".to_string(), "needle".to_string()),
        ]
    );
}

#[tokio::test]
async fn find_keeps_ignored_files_out_of_catch_all_results() {
    let tmp = TempDir::new("rg-find-ignored");
    write(&tmp.path().join(".gitignore"), "node_modules/\n");
    write(&tmp.path().join("node_modules/pkg/index.js"), "ignored\n");
    write(&tmp.path().join("src/index.js"), "included\n");
    init_repository(tmp.path());

    let files = globripgrep()
        .find(FindInput { cwd: tmp.text(), pattern: "*".to_string(), limit: 10, ..FindInput::default() })
        .await
        .expect("finds");
    let paths = paths_of(&files);
    assert!(paths.contains(&"src/index.js".to_string()));
    assert!(!paths.contains(&"node_modules/pkg/index.js".to_string()));
}

#[tokio::test]
async fn find_never_includes_git_metadata() {
    let tmp = TempDir::new("rg-find-git");
    write(&tmp.path().join(".opencode/config"), "needle\n");
    write(&tmp.path().join(".git/config"), "needle\n");
    let observed = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = observed.clone();
    let limited = globripgrep()
        .find(FindInput {
            cwd: tmp.text(),
            pattern: "**/*".to_string(),
            limit: 10,
            on_entry: Some(Arc::new(move |entry: &Entry| {
                seen.lock().expect("the observed paths lock is not poisoned").push(entry.path.clone());
            })),
            ..FindInput::default()
        })
        .await
        .expect("finds");
    let paths = paths_of(&limited);
    assert!(paths.contains(&".opencode/config".to_string()));
    assert!(!paths.contains(&".git/config".to_string()));
    assert_eq!(observed.lock().expect("the observed paths lock is not poisoned").as_slice(), paths.as_slice());

    let matches = globripgrep()
        .grep(GrepInput {
            cwd: tmp.text(),
            pattern: "needle".to_string(),
            include: Some("config".to_string()),
            limit: 10,
            ..GrepInput::default()
        })
        .await
        .expect("greps");
    let matched = matches.iter().map(|matched| matched.entry.path.clone()).collect::<Vec<_>>();
    assert!(matched.contains(&".opencode/config".to_string()));
    assert!(!matched.contains(&".git/config".to_string()));
}

#[tokio::test]
async fn find_excludes_protected_directory_trees() {
    let tmp = TempDir::new("rg-find-exclude");
    write(&tmp.path().join("Pictures/private.jpg"), "private\n");
    write(&tmp.path().join("visible.txt"), "visible\n");
    let files = globripgrep()
        .find(FindInput {
            cwd: tmp.text(),
            pattern: "*".to_string(),
            limit: 10,
            exclude: vec!["Pictures/**".to_string()],
            ..FindInput::default()
        })
        .await
        .expect("finds");
    let paths = paths_of(&files);
    assert!(paths.contains(&"visible.txt".to_string()));
    assert!(!paths.contains(&"Pictures/private.jpg".to_string()));
}

#[tokio::test]
async fn glob_reports_an_invalid_glob() {
    let tmp = TempDir::new("rg-glob-invalid");
    write(&tmp.path().join("visible.ts"), "needle\n");
    let error = globripgrep()
        .glob(GlobInput { cwd: tmp.text(), pattern: "[z-a]".to_string(), limit: 10, ..GlobInput::default() })
        .await
        .expect_err("reports the malformed glob");
    match error {
        Error::InvalidGlob { message } => assert!(message.contains("invalid range"), "{message}"),
        other => panic!("expected an invalid glob, got {other:?}"),
    }
}

#[tokio::test]
async fn grep_reports_an_invalid_pattern() {
    let tmp = TempDir::new("rg-grep-invalid");
    write(&tmp.path().join("visible.ts"), "needle\n");
    let error = globripgrep()
        .grep(GrepInput { cwd: tmp.text(), pattern: "[".to_string(), limit: 10, ..GrepInput::default() })
        .await
        .expect_err("reports the malformed pattern");
    match error {
        Error::InvalidPattern { pattern, message } => {
            assert_eq!(pattern, "[");
            assert!(message.contains("regex parse error"), "{message}");
        }
        other => panic!("expected an invalid pattern, got {other:?}"),
    }
}

#[tokio::test]
async fn grep_returns_a_bounded_preview_for_oversized_lines() {
    let tmp = TempDir::new("rg-grep-oversized");
    write(&tmp.path().join("generated.ts"), format!("Cloudflare{}\n", "x".repeat(70 * 1024)));
    let matches = globripgrep()
        .grep(GrepInput { cwd: tmp.text(), pattern: "Cloudflare".to_string(), limit: 10, ..GrepInput::default() })
        .await
        .expect("greps");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].entry.path, "generated.ts");
    assert_eq!(matches[0].text.encode_utf16().count(), 2_003);
    assert!(matches[0].text.ends_with("..."));
    assert_eq!(matches[0].submatches, [Submatch { text: "Cloudflare".to_string(), start: 0, end: 10 }]);
}

#[tokio::test]
async fn grep_ends_oversized_line_previews_on_a_whole_character() {
    let tmp = TempDir::new("rg-grep-emoji");
    let contents = [1_998, 1_999, 2_000].map(|count| format!("{}😀tail\n", "x".repeat(count))).concat();
    write(&tmp.path().join("emoji.txt"), contents);
    let matches = globripgrep()
        .grep(GrepInput { cwd: tmp.text(), pattern: "tail".to_string(), limit: 10, ..GrepInput::default() })
        .await
        .expect("greps");
    let texts = matches.iter().map(|matched| matched.text.clone()).collect::<Vec<_>>();
    assert_eq!(
        texts,
        [
            format!("{}😀...", "x".repeat(1_998)),
            format!("{}...", "x".repeat(1_999)),
            format!("{}...", "x".repeat(2_000)),
        ]
    );
}

#[test]
fn verify_accepts_a_download_matching_the_pinned_hash() {
    let bytes = b"ripgrep archive";
    let hash = opencode_util::hash::sha256_hex(bytes);
    verify(bytes, "ripgrep.tar.gz", &hash).expect("accepts a matching hash");
}

#[test]
fn verify_refuses_a_download_whose_hash_does_not_match() {
    let error = verify(b"ripgrep archive", "ripgrep.tar.gz", &"0".repeat(64)).expect_err("refuses a mismatching hash");
    assert!(error.to_string().starts_with("ripgrep download ripgrep.tar.gz failed SHA-256 verification"));
}

#[tokio::test]
async fn binary_downloads_and_installs_ripgrep() {
    let tmp = TempDir::new("rg-download");
    let triple = opencode_fs::ripgrep::binary::host_platform().expect("a supported host platform");
    let archive = tar_gz(&format!("ripgrep-15.1.0-{triple}/rg"), b"fake ripgrep binary");
    let stub = Stub::start(archive.clone()).await;
    let paths = test_paths(tmp.path());
    let binary = Binary::new(
        paths.clone(),
        |_| None,
        vec![Release {
            platform: triple.to_string(),
            extension: Extension::TarGz,
            sha256: opencode_util::hash::sha256_hex(&archive),
            url: stub.url("/ripgrep.tar.gz"),
        }],
    );

    let filepath = binary.filepath().await.expect("downloads ripgrep");
    assert_eq!(filepath, paths.bin.join("rg"));
    assert_eq!(fs::read(&filepath).expect("reads the installed binary"), b"fake ripgrep binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&filepath).expect("reads the installed mode").permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    assert!(!paths.bin.join(format!("ripgrep-15.1.0-{triple}.tar.gz")).exists());
    assert!(
        !fs::read_dir(&paths.bin)
            .expect("reads the bin directory")
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with("ripgrep-"))
    );
}

#[tokio::test]
async fn binary_rejects_a_download_with_the_wrong_hash_without_installing_it() {
    let tmp = TempDir::new("rg-download-mismatch");
    let triple = opencode_fs::ripgrep::binary::host_platform().expect("a supported host platform");
    let archive = tar_gz(&format!("ripgrep-15.1.0-{triple}/rg"), b"fake ripgrep binary");
    let stub = Stub::start(archive).await;
    let paths = test_paths(tmp.path());
    let binary = Binary::new(
        paths.clone(),
        |_| None,
        vec![Release {
            platform: triple.to_string(),
            extension: Extension::TarGz,
            sha256: "0".repeat(64),
            url: stub.url("/ripgrep.tar.gz"),
        }],
    );

    let error = binary.filepath().await.expect_err("rejects the download");
    assert!(error.to_string().contains("failed SHA-256 verification"));
    assert!(!paths.bin.join("rg").exists());
}

fn init_repository(directory: &Path) {
    let output = std::process::Command::new("git").args(["init", "-q"]).arg(directory).output().expect("runs git init");
    assert!(output.status.success(), "git init failed: {}", String::from_utf8_lossy(&output.stderr));
}

fn test_paths(root: &Path) -> GlobalPaths {
    let root = root.to_path_buf();
    GlobalPaths::from_vars(move |name| match name {
        "HOME" => Some(root.join("home").into_os_string()),
        "XDG_CACHE_HOME" => Some(root.join("cache").into_os_string()),
        _ => None,
    })
    .expect("resolves the test paths")
}

struct Stub {
    address: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Stub {
    async fn start(body: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds the stub listener");
        let address = listener.local_addr().expect("the stub listener has an address");
        let body = Bytes::from(body);
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |_request| {
                        let body = body.clone();
                        async move { Ok::<_, Infallible>(Response::new(Full::new(body))) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(socket), service)
                        .await;
                });
            }
        });
        Self { address, task }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn tar_gz(name: &str, contents: &[u8]) -> Vec<u8> {
    let mut archive = Vec::new();
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..108].copy_from_slice(b"0000755\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    header[124..136].copy_from_slice(format!("{:011o}\0", contents.len()).as_bytes());
    header[136..148].copy_from_slice(b"00000000000\0");
    header[148..156].copy_from_slice(b"        ");
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let checksum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
    header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
    archive.extend_from_slice(&header);
    archive.extend_from_slice(contents);
    let padding = (512 - contents.len() % 512) % 512;
    archive.extend(std::iter::repeat_n(0u8, padding));
    archive.extend(std::iter::repeat_n(0u8, 1024));
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, &archive).expect("writes the tar archive");
    encoder.finish().expect("finishes the gzip stream")
}
