use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use opencode_local::paths::Paths;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalPaths {
    pub home: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub tmp: PathBuf,
    pub bin: PathBuf,
    pub log: PathBuf,
    pub repos: PathBuf,
    tmp_explicit: bool,
}

impl GlobalPaths {
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> io::Result<Self> {
        Ok(Self::from_paths(Paths::from_vars(var)?))
    }

    pub fn from_env() -> io::Result<Self> {
        Ok(Self::from_paths(Paths::from_env()?))
    }

    pub fn with_config(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config = dir.into();
        self
    }

    pub fn with_tmp(mut self, dir: impl Into<PathBuf>) -> Self {
        self.tmp = dir.into();
        self.tmp_explicit = true;
        self
    }

    pub fn acquire(self) -> io::Result<Self> {
        for dir in [&self.data, &self.config, &self.state, &self.log, &self.bin, &self.repos, &self.tmp] {
            std::fs::create_dir_all(dir)?;
        }
        if self.tmp_explicit {
            return Ok(self);
        }
        let real = std::fs::canonicalize(&self.tmp)?;
        let tmp = match real.to_str() {
            Some(text) => PathBuf::from(opencode_util::path::strip_verbatim(text).as_ref()),
            None => real,
        };
        Ok(Self { tmp, ..self })
    }

    pub fn locks(&self) -> PathBuf {
        self.local().locks()
    }

    fn from_paths(paths: Paths) -> Self {
        Self {
            home: paths.home,
            data: paths.data,
            cache: paths.cache,
            config: paths.config,
            state: paths.state,
            tmp: paths.tmp,
            bin: paths.bin,
            log: paths.log,
            repos: paths.repos,
            tmp_explicit: false,
        }
    }

    fn local(&self) -> Paths {
        Paths {
            home: self.home.clone(),
            data: self.data.clone(),
            cache: self.cache.clone(),
            config: self.config.clone(),
            state: self.state.clone(),
            tmp: self.tmp.clone(),
            bin: self.bin.clone(),
            log: self.log.clone(),
            repos: self.repos.clone(),
        }
    }
}
