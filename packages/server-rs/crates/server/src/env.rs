use std::ffi::{OsStr, OsString};

use indexmap::IndexMap;

pub struct ServerEnv {
    vars: IndexMap<OsString, OsString>,
}

impl ServerEnv {
    pub fn from_process() -> Self {
        Self::from_vars(std::env::vars_os())
    }

    fn from_vars(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        Self { vars: vars.into_iter().collect() }
    }

    pub fn get(&self, name: &str) -> Option<&OsStr> {
        self.vars.get(OsStr::new(name)).map(OsString::as_os_str)
    }

    #[expect(dead_code)]
    pub fn merge(&mut self, env: &IndexMap<String, String>) {
        let Some(env) = opencode_local::service_config::process_env(Some(env)) else {
            return;
        };
        for (name, value) in env {
            self.vars.insert(name.into(), value.into());
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.vars.shift_remove(OsStr::new(name));
    }

    pub fn password(&self) -> Option<String> {
        let password = |name: &str| {
            self.get(name).filter(|value| !value.is_empty()).map(|value| value.to_string_lossy().into_owned())
        };
        password("OPENCODE_PASSWORD").or_else(|| password("OPENCODE_SERVER_PASSWORD"))
    }
}
