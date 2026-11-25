pub trait RegistryEvents: Send + Sync {
    fn updated(&self, r#type: &'static str);
}
