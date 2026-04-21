//! `genai`-backed implementation of [`TextRefiner`].
//!
//! Covers OpenAI, Anthropic, Ollama, Groq, Gemini, xAI, DeepSeek, and other
//! providers supported by the `genai` crate. Provider selection is derived
//! from the configured model name (e.g. `gpt-4o-mini` → OpenAI,
//! `claude-haiku-4-5` → Anthropic, `llama3.2` → Ollama when a custom base
//! URL is supplied).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use genai::adapter::AdapterKind;
use genai::chat::{ChatMessage, ChatOptions, ChatRequest};
use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
use genai::{Client, ModelIden, ServiceTarget};

use super::{RefineError, TextRefiner, DEFAULT_SYSTEM_PROMPT};
use crate::config::Config;

pub struct GenAiRefiner {
    client: Client,
    model: Arc<str>,
    system_prompt: Arc<str>,
    timeout: Duration,
    options: ChatOptions,
}

impl GenAiRefiner {
    /// Build a refiner from the current config.
    ///
    /// Applies a `ServiceTargetResolver` only when the user has overridden
    /// the base URL or API key; otherwise relies on `genai`'s defaults
    /// (adapter inferred from the model name; auth read from provider env
    /// vars such as `OPENAI_API_KEY` / `ANTHROPIC_API_KEY`).
    ///
    /// # Errors
    ///
    /// Returns `RefineError::Configuration` when the model field is empty.
    pub fn from_config(config: &Config) -> Result<Self, RefineError> {
        let model = config.llm_refine_model.trim();
        if model.is_empty() {
            return Err(RefineError::Configuration(
                "LLM_REFINE_MODEL is empty".into(),
            ));
        }

        let base_url = config
            .llm_refine_base_url
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let api_key = config
            .llm_refine_api_key
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let client = if base_url.is_some() || api_key.is_some() {
            let resolver = ServiceTargetResolver::from_resolver_fn(
                move |service_target: ServiceTarget|
                    -> Result<ServiceTarget, genai::resolver::Error> {
                    let ServiceTarget {
                        mut endpoint,
                        mut auth,
                        mut model,
                    } = service_target;
                    if let Some(ref url) = base_url {
                        endpoint = Endpoint::from_owned(url.clone());
                        // A custom base URL almost always means an
                        // OpenAI-compatible endpoint (Ollama, LM Studio,
                        // LocalAI, vLLM, Groq via compat, …).
                        model = ModelIden::new(AdapterKind::OpenAI, model.model_name);
                    }
                    if let Some(ref key) = api_key {
                        auth = AuthData::from_single(key.clone());
                    }
                    Ok(ServiceTarget {
                        endpoint,
                        auth,
                        model,
                    })
                },
            );
            Client::builder()
                .with_service_target_resolver(resolver)
                .build()
        } else {
            Client::default()
        };

        let system_prompt: Arc<str> = config
            .llm_refine_system_prompt
            .as_deref()
            .unwrap_or(DEFAULT_SYSTEM_PROMPT)
            .into();

        let mut options = ChatOptions::default();
        if let Some(max) = config.llm_refine_max_tokens {
            options = options.with_max_tokens(max);
        }

        Ok(Self {
            client,
            model: Arc::<str>::from(model),
            system_prompt,
            timeout: Duration::from_millis(config.llm_refine_timeout_ms.max(1)),
            options,
        })
    }
}

#[async_trait]
impl TextRefiner for GenAiRefiner {
    async fn refine(&self, text: &str) -> Result<String, RefineError> {
        let wrapped = format!(
            "Clean up this transcript and return only the cleaned text:\n<transcript>\n{text}\n</transcript>"
        );
        let request = ChatRequest::new(vec![
            ChatMessage::system(self.system_prompt.as_ref()),
            ChatMessage::user(wrapped),
        ]);
        let fut = self
            .client
            .exec_chat(self.model.as_ref(), request, Some(&self.options));
        let response = tokio::time::timeout(self.timeout, fut)
            .await
            .map_err(|_| RefineError::Timeout)?
            .map_err(map_genai_error)?;
        match response.first_text() {
            Some(s) if !s.trim().is_empty() => Ok(s.to_string()),
            _ => Err(RefineError::EmptyResponse),
        }
    }
}

fn map_genai_error(err: genai::Error) -> RefineError {
    let msg = err.to_string();
    let lower = msg.to_lowercase();
    if lower.contains("401")
        || lower.contains("unauthorized")
        || lower.contains("authentication")
        || lower.contains("api key")
    {
        RefineError::AuthenticationFailed(msg)
    } else if lower.contains("connect")
        || lower.contains("dns")
        || lower.contains("tls")
        || lower.contains("timeout")
        || lower.contains("timed out")
    {
        RefineError::Network(msg)
    } else {
        RefineError::Api(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unwrap_refiner(result: Result<GenAiRefiner, RefineError>) -> GenAiRefiner {
        match result {
            Ok(r) => r,
            Err(e) => panic!("expected Ok, got Err({e})"),
        }
    }

    #[test]
    fn from_config_rejects_empty_model() {
        let cfg = Config {
            llm_refine_enabled: true,
            llm_refine_model: String::new(),
            ..Config::default()
        };
        match GenAiRefiner::from_config(&cfg) {
            Err(RefineError::Configuration(_)) => {}
            Ok(_) => panic!("expected Err, got Ok"),
            Err(e) => panic!("expected Configuration error, got {e}"),
        }
    }

    #[test]
    fn from_config_builds_with_custom_base_url_and_key() {
        let cfg = Config {
            llm_refine_enabled: true,
            llm_refine_base_url: Some("http://127.0.0.1:9/v1".into()),
            llm_refine_api_key: Some("sk-test".into()),
            llm_refine_model: "gpt-4o-mini".into(),
            ..Config::default()
        };
        let refiner = unwrap_refiner(GenAiRefiner::from_config(&cfg));
        assert_eq!(refiner.model.as_ref(), "gpt-4o-mini");
        assert_eq!(refiner.timeout, Duration::from_millis(5000));
    }

    /// Hitting an unreachable local port must surface as a non-panicking
    /// `RefineError` (Network / Api / Timeout) — never a success, never a
    /// panic. Uses port 9 (discard) which is typically closed.
    #[tokio::test]
    async fn refine_returns_error_on_unreachable_endpoint() {
        let cfg = Config {
            llm_refine_enabled: true,
            llm_refine_base_url: Some("http://127.0.0.1:9/v1".into()),
            llm_refine_api_key: Some("sk-test".into()),
            llm_refine_model: "gpt-4o-mini".into(),
            llm_refine_timeout_ms: 500,
            ..Config::default()
        };
        let refiner = unwrap_refiner(GenAiRefiner::from_config(&cfg));
        let res = refiner.refine("hello world").await;
        match res {
            Err(_) => {}
            Ok(s) => panic!("expected error, got Ok({s:?})"),
        }
    }
}
