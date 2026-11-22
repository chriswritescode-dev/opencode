#[cfg(unix)]
#[test]
fn paths_come_from_the_client_crate() {
    use std::path::PathBuf;

    let paths = opencode_local::paths::Paths::from_vars(|name| (name == "HOME").then(|| "/h".into())).unwrap();
    assert_eq!(paths.state, PathBuf::from("/h/.local/state/opencode"));
    assert_eq!(paths.locks(), PathBuf::from("/h/.local/state/opencode/locks"));
}

#[test]
fn dates_accept_only_leap_days_that_exist() {
    assert_eq!(opencode_util::time::Date::new(2026, 2, 29), None);
    assert!(opencode_util::time::Date::new(2028, 2, 29).is_some());
}
