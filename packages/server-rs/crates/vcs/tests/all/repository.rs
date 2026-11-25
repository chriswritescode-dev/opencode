use std::ffi::OsString;
use std::sync::Arc;

use opencode_util::path::Flavor;
use opencode_vcs::repository::{
    Error, Reference, RemoteReference, Vars, cache_identity, cache_path, is_file, is_remote, parse, parse_remote, same,
    validate_branch,
};
use url::Url;

const INVALID_REFERENCE_MESSAGE: &str =
    "Repository must be a git URL, host/path reference, or GitHub owner/repo shorthand";
const UNSUPPORTED_LOCAL_MESSAGE: &str = "Local file repositories are not supported";
const INVALID_BRANCH_MESSAGE: &str =
    "Branch must contain only alphanumeric characters, /, _, ., and -, and cannot start with - or contain ..";

fn vars() -> Vars {
    Arc::new(|_: &str| None)
}

fn with_github_base(base: &'static str) -> Vars {
    Arc::new(move |key: &str| (key == "OPENCODE_REPO_CLONE_GITHUB_BASE_URL").then(|| OsString::from(base)))
}

fn remote(input: &str) -> RemoteReference {
    parse_remote(input, &vars()).expect("remote reference")
}

fn remote_reference(input: &str) -> Reference {
    Reference::Remote(remote(input))
}

#[test]
fn parses_github_shorthand_and_builds_an_explicit_root_cache_path() {
    let reference = remote("owner/repo");
    assert_eq!(
        reference,
        RemoteReference {
            host: "github.com".to_owned(),
            path: "owner/repo".to_owned(),
            segments: vec!["owner".to_owned(), "repo".to_owned()],
            owner: Some("owner".to_owned()),
            repo: "repo".to_owned(),
            remote: "https://github.com/owner/repo.git".to_owned(),
            label: "owner/repo".to_owned(),
            protocol: None,
        }
    );

    let reference = Reference::Remote(reference);
    assert_eq!(cache_path("/cache", &reference, None), Flavor::NATIVE.join(&["/cache", "github.com", "owner", "repo"]));
    assert_eq!(
        cache_path("/cache", &reference, Some("main")),
        Flavor::NATIVE.join(&["/cache", "github.com", "owner", "repo@main"])
    );
    assert_eq!(
        cache_path("/cache", &reference, Some("feature/x")),
        Flavor::NATIVE.join(&["/cache", "github.com", "owner", "repo@feature%2Fx"])
    );
    assert_eq!(cache_identity(&reference), "github.com/owner/repo");
}

#[test]
fn parses_host_path_and_scp_remote_references() {
    assert_eq!(
        remote("gitlab.com/group/repo"),
        RemoteReference {
            host: "gitlab.com".to_owned(),
            path: "group/repo".to_owned(),
            segments: vec!["group".to_owned(), "repo".to_owned()],
            owner: Some("group".to_owned()),
            repo: "repo".to_owned(),
            remote: "https://gitlab.com/group/repo.git".to_owned(),
            label: "gitlab.com/group/repo".to_owned(),
            protocol: None,
        }
    );
    assert_eq!(
        remote("git@github.com:owner/repo.git"),
        RemoteReference {
            host: "github.com".to_owned(),
            path: "owner/repo".to_owned(),
            segments: vec!["owner".to_owned(), "repo".to_owned()],
            owner: Some("owner".to_owned()),
            repo: "repo".to_owned(),
            remote: "git@github.com:owner/repo.git".to_owned(),
            label: "owner/repo".to_owned(),
            protocol: None,
        }
    );
}

#[test]
fn caches_a_host_with_a_port_under_a_directory_per_host_part() {
    assert_eq!(
        cache_path("/cache", &remote_reference("ssh://git@example.com:2222/owner/repo"), None),
        Flavor::NATIVE.join(&["/cache", "example.com", "2222", "owner", "repo"])
    );
}

#[test]
fn keeps_local_file_repositories_distinct_from_remote_repositories() {
    let cwd = std::env::current_dir().expect("current directory").to_string_lossy().into_owned();
    let local_path = Flavor::NATIVE.join(&[&cwd, "repo.git"]);
    let url = Url::from_file_path(&local_path).expect("file url");

    let reference = parse(url.as_str(), &vars()).expect("file reference");
    let Reference::File(file) = &reference else {
        panic!("expected file reference");
    };
    assert_eq!(file.host, "file");
    assert_eq!(file.path, local_path);
    assert_eq!(file.protocol.as_deref(), Some("file:"));
    assert_eq!(file.label, local_path);
    assert_eq!(file.owner, None);
    assert_eq!(file.repo, "repo");
    assert_eq!(file.remote, url.as_str());
    assert!(is_file(&reference));
    assert!(!is_remote(&reference));

    assert_eq!(
        parse_remote(url.as_str(), &vars()),
        Err(Error::UnsupportedLocalRepository {
            repository: url.as_str().to_owned(),
            message: UNSUPPORTED_LOCAL_MESSAGE.to_owned(),
        })
    );
}

#[test]
fn rejects_unsafe_remote_references_and_branches_with_typed_errors() {
    let invalid = |input: &str| Error::InvalidReference {
        repository: input.to_owned(),
        message: INVALID_REFERENCE_MESSAGE.to_owned(),
    };
    assert_eq!(parse_remote("not-a-repo", &vars()), Err(invalid("not-a-repo")));
    assert_eq!(
        parse_remote("git@github.com:../../../etc/passwd", &vars()),
        Err(invalid("git@github.com:../../../etc/passwd"))
    );

    assert_eq!(validate_branch("feature/docs.v1"), Ok(()));

    let invalid_branch =
        |branch: &str| Error::InvalidBranch { branch: branch.to_owned(), message: INVALID_BRANCH_MESSAGE.to_owned() };
    assert_eq!(validate_branch("-bad"), Err(invalid_branch("-bad")));
    assert_eq!(validate_branch("bad..branch"), Err(invalid_branch("bad..branch")));
    assert_eq!(validate_branch("bad branch"), Err(invalid_branch("bad branch")));
}

#[test]
fn rejects_hosts_with_relative_path_segments() {
    for input in [
        "..:repo",
        "git@..:repo",
        "../owner/repo",
        "ssh://../repo",
        "ssh://..:22/repo",
        "https://%2e%2e/owner/repo",
        ".:repo",
    ] {
        assert_eq!(
            parse_remote(input, &vars()),
            Err(Error::InvalidReference {
                repository: input.to_owned(),
                message: INVALID_REFERENCE_MESSAGE.to_owned(),
            }),
            "{input}"
        );
    }
}

#[test]
fn compares_cache_identity_independent_of_input_spelling() {
    let shorthand = remote_reference("owner/repo");
    assert!(same(&shorthand, &remote_reference("https://github.com/owner/repo.git")));
    assert!(same(&shorthand, &remote_reference("github.com/owner/repo")));
}

#[test]
fn resolves_github_remotes_against_a_configured_base_url() {
    assert_eq!(
        parse_remote("owner/repo", &with_github_base("https://git.example.com/mirror")).expect("remote").remote,
        "https://git.example.com/mirror/owner/repo.git"
    );
    assert_eq!(
        parse_remote("owner/repo", &with_github_base("https://git.example.com/mirror/")).expect("remote").remote,
        "https://git.example.com/mirror/owner/repo.git"
    );
    assert_eq!(
        parse_remote("owner/repo", &with_github_base("not a url")).expect("remote").remote,
        "https://github.com/owner/repo.git"
    );
    assert_eq!(
        parse_remote("owner/repo", &with_github_base("")).expect("remote").remote,
        "https://github.com/owner/repo.git"
    );
}
