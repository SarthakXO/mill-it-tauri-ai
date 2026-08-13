use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ChatRole {
    User,
    Assistant,
}

impl ChatRole {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ChatMessage {
    pub(crate) role: ChatRole,
    pub(crate) content: String,
}

impl ChatMessage {
    pub(crate) fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }
}

pub(crate) trait ChatProvider {
    async fn chat(&self, messages: &[ChatMessage]) -> Result<String, LlmError>;
}

#[derive(Debug)]
pub(crate) enum LlmError {
    ServiceUnavailable,
    GenerationTimeout,
    ModelMissing(String),
    ModelLoading,
    HttpError(String),
    RequestFailed(String),
    InvalidResponse(String),
}

impl LlmError {
    pub(crate) fn user_message(&self) -> String {
        match self {
            Self::ServiceUnavailable => {
                "Ollama is not available. Make sure Ollama is running and try again.".to_owned()
            }
            Self::GenerationTimeout => {
                "The local model took too long to respond. Try again or turn Thinking mode off."
                    .to_owned()
            }
            Self::ModelMissing(model) => {
                format!("The required local model {model} is not installed.")
            }
            Self::ModelLoading => {
                "The local model is still loading. Please try again shortly.".to_owned()
            }
            Self::HttpError(_) | Self::RequestFailed(_) => {
                "The local AI request failed. Check Ollama and try again.".to_owned()
            }
            Self::InvalidResponse(_) => {
                "The local AI model returned an invalid response. Please try again.".to_owned()
            }
        }
    }
}

impl fmt::Display for LlmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServiceUnavailable => formatter.write_str("Ollama service is unavailable"),
            Self::GenerationTimeout => formatter.write_str("LLM generation timed out"),
            Self::ModelMissing(model) => {
                write!(formatter, "the required {model} model is missing")
            }
            Self::ModelLoading => formatter.write_str("the configured model is loading"),
            Self::HttpError(detail) => write!(formatter, "Ollama HTTP error: {detail}"),
            Self::RequestFailed(detail) => write!(formatter, "request failed: {detail}"),
            Self::InvalidResponse(detail) => write!(formatter, "invalid response: {detail}"),
        }
    }
}

impl Error for LlmError {}
