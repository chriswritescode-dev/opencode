use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use serde_json::{Map, Value};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq)]
pub struct CredentialValue {
    pub key: Option<String>,
    pub metadata: Map<String, Value>,
}

pub trait CredentialLookup: Send + Sync {
    fn latest(&self, integration_id: &str) -> Option<CredentialValue>;
    fn get(&self, credential_id: &str) -> Option<CredentialValue>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct WellKnownEntry {
    pub origin: String,
    pub integration_id: String,
    pub manifest: Value,
}

impl WellKnownEntry {
    pub fn auth_env(&self) -> Option<&str> {
        self.manifest.get("auth")?.get("env")?.as_str()
    }
}

pub type WellKnownError = Box<dyn std::error::Error + Send + Sync>;

pub trait WellKnownSource: Send + Sync {
    fn entries<'a>(&'a self) -> BoxFuture<'a, Vec<WellKnownEntry>>;
    fn resolve<'a>(
        &'a self,
        entry: &'a WellKnownEntry,
        variables: &'a BTreeMap<String, String>,
    ) -> BoxFuture<'a, Result<Vec<Value>, WellKnownError>>;
    fn cached<'a>(&'a self, origin: &'a str) -> BoxFuture<'a, Vec<Value>>;
    fn clear_cached<'a>(&'a self, origin: &'a str) -> BoxFuture<'a, ()>;
}

pub trait KvStore: Send + Sync {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Value>>;
    fn set<'a>(&'a self, key: &'a str, value: Value) -> BoxFuture<'a, ()>;
    fn remove<'a>(&'a self, key: &'a str) -> BoxFuture<'a, ()>;
}

impl CredentialLookup for () {
    fn latest(&self, _integration_id: &str) -> Option<CredentialValue> {
        None
    }

    fn get(&self, _credential_id: &str) -> Option<CredentialValue> {
        None
    }
}

pub struct NoWellKnown;

impl WellKnownSource for NoWellKnown {
    fn entries<'a>(&'a self) -> BoxFuture<'a, Vec<WellKnownEntry>> {
        Box::pin(async { Vec::new() })
    }

    fn resolve<'a>(
        &'a self,
        _entry: &'a WellKnownEntry,
        _variables: &'a BTreeMap<String, String>,
    ) -> BoxFuture<'a, Result<Vec<Value>, WellKnownError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn cached<'a>(&'a self, _origin: &'a str) -> BoxFuture<'a, Vec<Value>> {
        Box::pin(async { Vec::new() })
    }

    fn clear_cached<'a>(&'a self, _origin: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

pub struct NoKv;

impl KvStore for NoKv {
    fn get<'a>(&'a self, _key: &'a str) -> BoxFuture<'a, Option<Value>> {
        Box::pin(async { None })
    }

    fn set<'a>(&'a self, _key: &'a str, _value: Value) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn remove<'a>(&'a self, _key: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}
