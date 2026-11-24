use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;

use opencode_config::paths::GlobalPaths;
use opencode_util::temp::TempDir;

fn paths(vars: &[(&str, OsString)]) -> GlobalPaths {
    let vars: HashMap<String, OsString> = vars.iter().map(|(key, value)| (key.to_string(), value.clone())).collect();
    GlobalPaths::from_vars(|name| vars.get(name).cloned()).unwrap()
}

fn app(root: &std::path::Path) -> PathBuf {
    root.join("opencode")
}

#[test]
fn xdg_overrides_replace_the_roots() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    let (data, cache, config, state) =
        (root.path().join("data"), root.path().join("cache"), root.path().join("config"), root.path().join("state"));
    let p = paths(&[
        ("HOME", home.clone().into()),
        ("XDG_DATA_HOME", data.clone().into()),
        ("XDG_CACHE_HOME", cache.clone().into()),
        ("XDG_CONFIG_HOME", config.clone().into()),
        ("XDG_STATE_HOME", state.clone().into()),
    ]);
    assert_eq!(p.data, app(&data));
    assert_eq!(p.cache, app(&cache));
    assert_eq!(p.config, app(&config));
    assert_eq!(p.state, app(&state));
    assert_eq!(p.bin, app(&cache).join("bin"));
    assert_eq!(p.log, app(&data).join("log"));
    assert_eq!(p.repos, app(&data).join("repos"));
    assert_eq!(p.home, home);
}

#[test]
fn empty_xdg_values_use_home_defaults() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    let p = paths(&[
        ("HOME", home.clone().into()),
        ("XDG_DATA_HOME", "".into()),
        ("XDG_CACHE_HOME", "".into()),
        ("XDG_CONFIG_HOME", "".into()),
        ("XDG_STATE_HOME", "".into()),
    ]);
    assert_eq!(p.data, app(&home.join(".local").join("share")));
    assert_eq!(p.cache, app(&home.join(".cache")));
    assert_eq!(p.config, app(&home.join(".config")));
    assert_eq!(p.state, app(&home.join(".local").join("state")));
}

#[test]
fn opencode_config_dir_replaces_only_config() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    let custom = root.path().join("custom");
    let p = paths(&[
        ("HOME", home.clone().into()),
        ("XDG_CONFIG_HOME", root.path().join("config").into()),
        ("OPENCODE_CONFIG_DIR", custom.clone().into()),
    ]);
    assert_eq!(p.config, custom);
    assert_eq!(p.data, app(&home.join(".local").join("share")));
    assert_eq!(p.cache, app(&home.join(".cache")));
    assert_eq!(p.state, app(&home.join(".local").join("state")));
    assert_eq!(p.home, home);

    let default = paths(&[("HOME", home.into())]);
    let mut expected = default.clone();
    expected.config = custom.clone();
    assert_eq!(default.with_config(custom), expected);
}

#[test]
fn opencode_test_home_replaces_only_home() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    let test_home = root.path().join("test-home");
    let p = paths(&[("HOME", home.clone().into()), ("OPENCODE_TEST_HOME", test_home.clone().into())]);
    assert_eq!(p.home, test_home);
    assert_eq!(p.data, app(&home.join(".local").join("share")));
    assert_eq!(p.cache, app(&home.join(".cache")));
    assert_eq!(p.config, app(&home.join(".config")));
    assert_eq!(p.state, app(&home.join(".local").join("state")));
}

#[test]
fn acquire_creates_directories_and_canonicalizes_default_tmp() {
    let root = TempDir::new().unwrap();
    let real = root.path().join("tmp");
    #[cfg(unix)]
    let (temp_var, tmp) = {
        use std::os::unix::fs::symlink;
        std::fs::create_dir(&real).unwrap();
        let link = root.path().join("link");
        symlink(&real, &link).unwrap();
        ("TMPDIR", link)
    };
    #[cfg(not(unix))]
    let (temp_var, tmp) = ("TEMP", root.path().join("nested").join("..").join("tmp"));
    let p = paths(&[
        ("HOME", root.path().join("home").into()),
        ("XDG_DATA_HOME", root.path().join("data").into()),
        ("XDG_CACHE_HOME", root.path().join("cache").into()),
        ("XDG_CONFIG_HOME", root.path().join("config").into()),
        ("XDG_STATE_HOME", root.path().join("state").into()),
        (temp_var, tmp.into()),
    ]);
    let acquired = p.clone().acquire().unwrap();
    for dir in [
        &acquired.data,
        &acquired.config,
        &acquired.state,
        &acquired.log,
        &acquired.bin,
        &acquired.repos,
        &acquired.tmp,
    ] {
        assert!(dir.is_dir(), "{}", dir.display());
    }
    assert_eq!(acquired.tmp, opencode_util::path::realpath(&real.join("opencode")).unwrap());
    #[cfg(unix)]
    assert_ne!(acquired.tmp, p.tmp);
}

#[test]
fn an_explicit_tmp_is_kept_as_given() {
    let root = TempDir::new().unwrap();
    let given = root.path().join("custom").join("..").join("tmp");
    let acquired = paths(&[("HOME", root.path().join("home").into())]).with_tmp(given.clone()).acquire().unwrap();
    assert_eq!(acquired.tmp, given);
    assert!(acquired.tmp.is_dir());
}
