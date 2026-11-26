use std::sync::Arc;

use opencode_config::seams::KvStore;
use serde_json::Value;

pub struct Storage {
    kv: Arc<dyn KvStore>,
    namespace: String,
}

impl Storage {
    pub fn new(kv: Arc<dyn KvStore>, plugin_id: &str) -> Self {
        let encoded: String = plugin_id.encode_utf16().map(|unit| format!("{unit:04x}")).collect();
        Self { kv, namespace: format!("plugin:{encoded}:") }
    }

    pub async fn get(&self, key: &str) -> Option<Value> {
        let key = self.scoped(key);
        self.kv.get(&key).await
    }

    pub async fn set(&self, key: &str, value: Value) {
        let key = self.scoped(key);
        self.kv.set(&key, value).await
    }

    pub async fn remove(&self, key: &str) {
        let key = self.scoped(key);
        self.kv.remove(&key).await
    }

    pub async fn scan(&self, prefix: &str, limit: usize) -> Vec<(String, Value)> {
        let prefix = self.scoped(prefix);
        self.kv
            .scan(&prefix, limit)
            .await
            .into_iter()
            .map(|(key, value)| (key.strip_prefix(&self.namespace).unwrap_or(&key).to_owned(), value))
            .collect()
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    fn scoped(&self, key: &str) -> String {
        format!("{}{key}", self.namespace)
    }
}
