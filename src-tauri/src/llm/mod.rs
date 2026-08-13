mod config;
pub(crate) mod ollama;
mod provider;

pub(crate) use config::OllamaConfig;
pub(crate) use provider::{ChatMessage, ChatProvider, ChatRole, LlmError};
