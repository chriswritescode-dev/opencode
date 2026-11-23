pub mod openai_chat;
pub mod openai_freeform;
pub mod shared;
pub mod utils;

pub fn registry() -> crate::route::model::ProtocolRegistry {
    let mut registry = crate::route::model::ProtocolRegistry::new();
    registry.register(std::sync::Arc::new(openai_chat::OpenAiChat));
    registry
}
