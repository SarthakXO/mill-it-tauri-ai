use crate::{
    agent::{AgentError, AgentOrchestrator, AgentResponse},
    analytics::ForecastStore,
    database::{Database, DatabaseError, DatabaseHealth},
    llm::{
        ollama::{OllamaClient, OllamaModel, OllamaStatus},
        ChatMessage, LlmError,
    },
};

fn command_error(error: LlmError) -> String {
    eprintln!("Ollama request failed: {error}");
    error.user_message()
}

fn client() -> Result<OllamaClient, String> {
    OllamaClient::new().map_err(command_error)
}

fn agent_error(error: AgentError) -> String {
    eprintln!("Agent request failed: {error}");
    error.user_message()
}

#[tauri::command]
pub(crate) async fn check_ollama() -> Result<OllamaStatus, String> {
    let client = client()?;

    match client.version().await {
        Ok(version) => {
            let models = client.list_models().await.map_err(command_error)?;
            if !models.iter().any(|model| model.name == client.model_name()) {
                return Err(command_error(LlmError::ModelMissing(
                    client.model_name().to_owned(),
                )));
            }
            Ok(OllamaStatus {
                available: true,
                version: Some(version),
                configured_model: client.model_name().to_owned(),
            })
        }
        Err(LlmError::ServiceUnavailable) => Ok(OllamaStatus {
            available: false,
            version: None,
            configured_model: client.model_name().to_owned(),
        }),
        Err(error) => Err(command_error(error)),
    }
}

#[tauri::command]
pub(crate) async fn list_ollama_models() -> Result<Vec<OllamaModel>, String> {
    client()?.list_models().await.map_err(command_error)
}

#[tauri::command]
pub(crate) async fn check_database(
    database: tauri::State<'_, Database>,
) -> Result<DatabaseHealth, String> {
    match database.health().await {
        Ok(health) => Ok(health),
        Err(DatabaseError::Configuration | DatabaseError::Unavailable) => Ok(DatabaseHealth {
            available: false,
            database: None,
            timezone: None,
            data_as_of: None,
        }),
        Err(error) => {
            eprintln!("Database health check failed: {error}");
            Err(error.user_message().to_owned())
        }
    }
}

#[tauri::command]
pub(crate) async fn chat_with_ollama(
    messages: Vec<ChatMessage>,
    thinking_enabled: bool,
    database: tauri::State<'_, Database>,
    forecast_store: tauri::State<'_, ForecastStore>,
) -> Result<AgentResponse, String> {
    if messages.is_empty() {
        return Err("Start the conversation with a message.".to_owned());
    }

    AgentOrchestrator::new_with_thinking_and_store(
        &database,
        thinking_enabled,
        forecast_store.inner().clone(),
    )
    .map_err(agent_error)?
    .respond(&messages)
    .await
    .map_err(agent_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatProvider, ChatRole};

    fn message(role: ChatRole, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: content.to_owned(),
        }
    }

    #[test]
    #[ignore = "requires a running local Ollama service with the configured model"]
    fn live_ollama_command_flow() {
        tauri::async_runtime::block_on(async {
            let status = check_ollama().await.expect("Ollama health check failed");
            assert!(status.available, "Ollama should be available");
            println!("Ollama version: {:?}", status.version);

            let models = list_ollama_models()
                .await
                .expect("Ollama model listing failed");
            let ollama = client().expect("Ollama client should initialize");
            assert!(
                models.iter().any(|model| model.name == ollama.model_name()),
                "configured model should be installed"
            );
            let greeting = ollama
                .chat(&[message(ChatRole::User, "Hi")])
                .await
                .expect("basic chat failed");
            assert!(!greeting.trim().is_empty(), "greeting should not be empty");
            println!("Greeting: {greeting}");

            let exact = ollama
                .chat(&[message(
                    ChatRole::User,
                    "Reply with exactly: Mill-it Ops ready",
                )])
                .await
                .expect("exact reply chat failed");
            assert_eq!(exact.trim(), "Mill-it Ops ready");

            let first_user = message(ChatRole::User, "My test number is 4821.");
            let acknowledgement = ollama
                .chat(std::slice::from_ref(&first_user))
                .await
                .expect("context setup chat failed");
            let recalled = ollama
                .chat(&[
                    first_user,
                    message(ChatRole::Assistant, &acknowledgement),
                    message(ChatRole::User, "What test number did I give you?"),
                ])
                .await
                .expect("context recall chat failed");
            assert!(
                recalled.contains("4821"),
                "context response should recall 4821: {recalled}"
            );
            println!("Context recall: {recalled}");

            let thinking_reply = OllamaClient::new()
                .expect("thinking client should initialize")
                .with_thinking(true)
                .chat(&[message(
                    ChatRole::User,
                    "Reply with exactly: Thinking mode ready",
                )])
                .await
                .expect("configured model should support thinking mode");
            assert_eq!(thinking_reply.trim(), "Thinking mode ready");
        });
    }

    #[test]
    #[ignore = "requires the local Ollama service to be stopped"]
    fn live_ollama_unavailable_error() {
        tauri::async_runtime::block_on(async {
            let error = client()
                .expect("Ollama client should initialize")
                .chat(&[message(ChatRole::User, "Hi")])
                .await
                .expect_err("chat should fail while Ollama is stopped");
            assert!(matches!(error, LlmError::ServiceUnavailable));
        });
    }
}
