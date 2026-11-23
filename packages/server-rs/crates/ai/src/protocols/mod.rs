pub mod anthropic_messages;
pub mod bedrock_converse;
pub mod bedrock_event_stream;
pub mod gemini;
pub mod mistral_chat;
pub mod open_responses;
pub mod open_responses_channel;
pub mod open_responses_continuation;
pub mod openai_chat;
pub mod openai_compatible_responses;
pub mod openai_freeform;
pub mod openai_responses;
pub mod shared;
pub mod utils;
pub mod venice_chat;
pub mod zai_chat;
pub mod zai_messages;

pub fn registry() -> crate::route::model::ProtocolRegistry {
    let mut registry = crate::route::model::ProtocolRegistry::new();
    registry.register(std::sync::Arc::new(anthropic_messages::AnthropicMessages::default()));
    registry.register(std::sync::Arc::new(bedrock_converse::BedrockConverse));
    registry.register(std::sync::Arc::new(gemini::Gemini));
    registry.register(std::sync::Arc::new(mistral_chat::MistralChat));
    registry.register(std::sync::Arc::new(open_responses::OpenResponses));
    registry.register(std::sync::Arc::new(openai_chat::OpenAiChat));
    registry.register(std::sync::Arc::new(openai_responses::OpenAiResponses));
    registry.register(std::sync::Arc::new(venice_chat::VeniceChat));
    registry.register(std::sync::Arc::new(zai_chat::ZaiChat));
    registry.register(std::sync::Arc::new(zai_messages::ZaiMessages));
    registry
}
