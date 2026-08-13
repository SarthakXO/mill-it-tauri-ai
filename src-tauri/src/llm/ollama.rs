use super::{ChatMessage, ChatProvider, LlmError, OllamaConfig};
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

const SYSTEM_PROMPT: &str = "You are Mill-it Ops, an internal AI assistant for the Mill-it ecommerce team. For now, respond normally and concisely to general conversation. Operational database tools will be provided later.";
const OLLAMA_CONTEXT_TOKENS: u32 = 16384;
const OLLAMA_OUTPUT_TOKENS: u32 = 4_096;

pub(crate) struct OllamaClient {
    http: Client,
    config: OllamaConfig,
    thinking_enabled: bool,
}

impl OllamaClient {
    pub(crate) fn new() -> Result<Self, LlmError> {
        Self::from_config(OllamaConfig::global().clone())
    }

    pub(crate) fn from_config(config: OllamaConfig) -> Result<Self, LlmError> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(180))
            .build()
            .map_err(|error| LlmError::RequestFailed(error.to_string()))?;

        Ok(Self {
            http,
            config,
            thinking_enabled: false,
        })
    }

    pub(crate) fn with_thinking(mut self, thinking_enabled: bool) -> Self {
        self.thinking_enabled = thinking_enabled;
        self
    }

    pub(crate) const fn thinking_enabled(&self) -> bool {
        self.thinking_enabled
    }

    pub(crate) fn model_name(&self) -> &str {
        self.config.model()
    }

    pub(crate) async fn version(&self) -> Result<String, LlmError> {
        let response = self
            .http
            .get(format!("{}/api/version", self.config.base_url()))
            .send()
            .await
            .map_err(request_error)?;
        let response = successful_response(response).await?;
        let version = response
            .json::<VersionResponse>()
            .await
            .map_err(|error| LlmError::InvalidResponse(error.to_string()))?;

        if version.version.trim().is_empty() {
            return Err(LlmError::InvalidResponse("version was empty".to_owned()));
        }

        Ok(version.version)
    }

    pub(crate) async fn list_models(&self) -> Result<Vec<OllamaModel>, LlmError> {
        let response = self
            .http
            .get(format!("{}/api/tags", self.config.base_url()))
            .send()
            .await
            .map_err(request_error)?;
        let response = successful_response(response).await?;
        let models = response
            .json::<ModelsResponse>()
            .await
            .map_err(|error| LlmError::InvalidResponse(error.to_string()))?;

        Ok(models
            .models
            .into_iter()
            .map(|model| OllamaModel { name: model.name })
            .collect())
    }

    pub(crate) async fn chat_json(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
    ) -> Result<String, LlmError> {
        self.send_chat(
            system_prompt,
            messages,
            Some(Value::String("json".to_owned())),
        )
        .await
    }

    pub(crate) async fn chat_json_schema(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        schema: Value,
    ) -> Result<String, LlmError> {
        self.send_chat(system_prompt, messages, Some(schema)).await
    }

    async fn send_chat(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        format: Option<Value>,
    ) -> Result<String, LlmError> {
        let mut ollama_messages = Vec::with_capacity(messages.len() + 1);
        ollama_messages.push(OllamaMessage {
            role: "system".to_owned(),
            content: system_prompt.to_owned(),
        });
        ollama_messages.extend(messages.iter().map(|message| OllamaMessage {
            role: message.role.as_str().to_owned(),
            content: message.content.clone(),
        }));

        let request = OllamaChatRequest {
            model: self.config.model(),
            messages: ollama_messages,
            stream: false,
            think: self.thinking_enabled,
            format,
            options: OllamaOptions {
                temperature: 0.0,
                num_ctx: OLLAMA_CONTEXT_TOKENS,
                num_predict: OLLAMA_OUTPUT_TOKENS,
            },
        };
        let response = self
            .http
            .post(format!("{}/api/chat", self.config.base_url()))
            .json(&request)
            .send()
            .await
            .map_err(request_error)?;
        let response = chat_response(response, self.config.model()).await?;
        let response = response
            .json::<OllamaChatResponse>()
            .await
            .map_err(|error| LlmError::InvalidResponse(error.to_string()))?;
        let answer = response.message.content.trim().to_owned();

        if answer.is_empty() {
            return Err(LlmError::InvalidResponse(
                "assistant content was empty".to_owned(),
            ));
        }

        Ok(answer)
    }
}

impl ChatProvider for OllamaClient {
    async fn chat(&self, messages: &[ChatMessage]) -> Result<String, LlmError> {
        self.send_chat(SYSTEM_PROMPT, messages, None).await
    }
}

fn request_error(error: reqwest::Error) -> LlmError {
    classify_transport_error(error.is_connect(), error.is_timeout(), error.to_string())
}

fn classify_transport_error(connect: bool, timeout: bool, detail: String) -> LlmError {
    if connect {
        LlmError::ServiceUnavailable
    } else if timeout {
        LlmError::GenerationTimeout
    } else {
        LlmError::RequestFailed(detail)
    }
}

async fn successful_response(response: Response) -> Result<Response, LlmError> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    let detail = response_detail(response).await;
    Err(LlmError::HttpError(format!("HTTP {status}: {detail}")))
}

async fn chat_response(response: Response, model: &str) -> Result<Response, LlmError> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    let detail = response_detail(response).await;
    let normalized_detail = detail.to_lowercase();

    Err(classify_chat_http_error(
        status,
        &normalized_detail,
        detail,
        model,
    ))
}

fn classify_chat_http_error(
    status: StatusCode,
    normalized_detail: &str,
    detail: String,
    model: &str,
) -> LlmError {
    if normalized_detail.contains("loading") || normalized_detail.contains("load model") {
        LlmError::ModelLoading
    } else if status == StatusCode::NOT_FOUND
        || (normalized_detail.contains("model")
            && (normalized_detail.contains("not found")
                || normalized_detail.contains("not installed")))
    {
        LlmError::ModelMissing(model.to_owned())
    } else {
        LlmError::HttpError(format!("HTTP {status}: {detail}"))
    }
}

async fn response_detail(response: Response) -> String {
    response
        .json::<OllamaErrorResponse>()
        .await
        .map(|body| body.error)
        .unwrap_or_else(|_| "Ollama returned an error without details".to_owned())
}

#[derive(Debug, Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: Vec<OllamaMessage>,
    stream: bool,
    think: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<Value>,
    options: OllamaOptions,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    temperature: f32,
    num_ctx: u32,
    num_predict: u32,
}

#[derive(Debug, Deserialize, Serialize)]
struct OllamaMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: OllamaMessage,
}

#[derive(Debug, Deserialize)]
struct VersionResponse {
    version: String,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    models: Vec<ModelResponse>,
}

#[derive(Debug, Deserialize)]
struct ModelResponse {
    name: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OllamaStatus {
    pub(crate) available: bool,
    pub(crate) version: Option<String>,
    pub(crate) configured_model: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct OllamaModel {
    pub(crate) name: String,
}

#[derive(Debug, Deserialize)]
struct OllamaErrorResponse {
    error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_mode_is_configurable() {
        let client = OllamaClient::new()
            .expect("client should initialize")
            .with_thinking(true);
        assert!(client.thinking_enabled);
    }

    #[test]
    fn llm_errors_have_distinct_safe_categories() {
        assert_eq!(
            LlmError::ServiceUnavailable.to_string(),
            "Ollama service is unavailable"
        );
        assert_eq!(
            LlmError::GenerationTimeout.to_string(),
            "LLM generation timed out"
        );
        assert_eq!(
            LlmError::ModelMissing("test-model".to_owned()).to_string(),
            "the required test-model model is missing"
        );
        assert!(LlmError::InvalidResponse("bad JSON".to_owned())
            .to_string()
            .contains("invalid response"));
        assert!(matches!(
            classify_transport_error(true, false, "connection refused".to_owned()),
            LlmError::ServiceUnavailable
        ));
        assert!(matches!(
            classify_transport_error(false, true, "deadline exceeded".to_owned()),
            LlmError::GenerationTimeout
        ));
        assert!(matches!(
            classify_chat_http_error(
                StatusCode::NOT_FOUND,
                "model not found",
                "model not found".to_owned(),
                "test-model"
            ),
            LlmError::ModelMissing(_)
        ));
        assert!(matches!(
            classify_chat_http_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "model loading",
                "model loading".to_owned(),
                "test-model"
            ),
            LlmError::ModelLoading
        ));
    }

    #[test]
    fn serialized_request_contains_thinking_flag_but_no_reasoning_output_contract() {
        let config = OllamaConfig::for_test("http://127.0.0.1:11434", "test-model");
        let request = OllamaChatRequest {
            model: config.model(),
            messages: Vec::new(),
            stream: false,
            think: true,
            format: None,
            options: OllamaOptions {
                temperature: 0.0,
                num_ctx: OLLAMA_CONTEXT_TOKENS,
                num_predict: OLLAMA_OUTPUT_TOKENS,
            },
        };
        let json = serde_json::to_value(request).expect("request should serialize");
        assert_eq!(json.get("think"), Some(&serde_json::Value::Bool(true)));
        assert!(json.get("thinking").is_none());
        assert_eq!(json["options"]["num_ctx"], OLLAMA_CONTEXT_TOKENS);
        assert_eq!(json["options"]["num_predict"], OLLAMA_OUTPUT_TOKENS);
    }
}
