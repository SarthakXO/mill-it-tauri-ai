use std::{env, sync::OnceLock};

const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434";
const DEFAULT_OLLAMA_MODEL: &str = "qwen3.5:9b";
static OLLAMA_CONFIG: OnceLock<OllamaConfig> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OllamaConfig {
    base_url: String,
    model: String,
}

impl OllamaConfig {
    pub(crate) fn global() -> &'static Self {
        OLLAMA_CONFIG.get_or_init(Self::from_environment)
    }

    fn from_environment() -> Self {
        let _ = dotenvy::dotenv();
        Self {
            base_url: environment_value("OLLAMA_BASE_URL", DEFAULT_OLLAMA_BASE_URL)
                .trim_end_matches('/')
                .to_owned(),
            model: environment_value("OLLAMA_MODEL", DEFAULT_OLLAMA_MODEL),
        }
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    #[cfg(test)]
    pub(crate) fn for_test(base_url: &str, model: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            model: model.to_owned(),
        }
    }
}

fn environment_value(key: &str, fallback: &str) -> String {
    env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_configuration_trims_a_trailing_base_url_slash() {
        let config = OllamaConfig::for_test("http://127.0.0.1:11434/", "local-test-model");
        assert_eq!(config.base_url(), "http://127.0.0.1:11434");
        assert_eq!(config.model(), "local-test-model");
    }
}
