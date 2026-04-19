//! Optional LLM post-processing of transcribed text.
//!
//! When enabled via `LLM_REFINE_ENABLED=true`, finalized transcripts are piped
//! through an LLM before being emitted to the configured output sink. The
//! feature is opt-in per flow (`LLM_REFINE_APPLY_BATCH`,
//! `LLM_REFINE_APPLY_CONTINUOUS`) and fails soft: any refiner error logs a
//! warning and the original text is emitted unchanged.

use async_trait::async_trait;
use std::fmt;
use std::sync::Arc;

use crate::config::Config;

#[cfg(feature = "llm-refine")]
pub mod genai_refiner;

/// The scope a piece of text is being refined in. Used only for logging.
#[derive(Clone, Copy, Debug)]
pub enum RefineScope {
    Batch,
    Continuous,
}

impl fmt::Display for RefineScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RefineScope::Batch => f.write_str("batch"),
            RefineScope::Continuous => f.write_str("continuous"),
        }
    }
}

/// Errors that a [`TextRefiner`] can produce.
#[derive(Debug)]
pub enum RefineError {
    /// The LLM took longer than `LLM_REFINE_TIMEOUT_MS`.
    Timeout,
    /// The LLM returned an empty / missing text block.
    EmptyResponse,
    /// Authentication (invalid API key, etc.).
    AuthenticationFailed(String),
    /// Network-level failure (connection, DNS, TLS).
    Network(String),
    /// HTTP / provider-level failure (non-2xx, malformed JSON).
    Api(String),
    /// Invalid or missing configuration.
    Configuration(String),
}

impl fmt::Display for RefineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RefineError::Timeout => write!(f, "LLM refinement timed out"),
            RefineError::EmptyResponse => write!(f, "LLM returned empty response"),
            RefineError::AuthenticationFailed(msg) => write!(f, "LLM authentication failed: {msg}"),
            RefineError::Network(msg) => write!(f, "LLM network error: {msg}"),
            RefineError::Api(msg) => write!(f, "LLM API error: {msg}"),
            RefineError::Configuration(msg) => write!(f, "LLM configuration error: {msg}"),
        }
    }
}

impl std::error::Error for RefineError {}

/// A refiner turns a raw transcript into a cleaned-up version.
///
/// Implementations must be cheap to clone via `Arc` and safe to call
/// concurrently from multiple tasks.
#[async_trait]
pub trait TextRefiner: Send + Sync {
    async fn refine(&self, text: &str) -> Result<String, RefineError>;
}

/// Default system prompt for the "aggressive cleanup" style. Users can override
/// via `LLM_REFINE_SYSTEM_PROMPT`.
pub const DEFAULT_SYSTEM_PROMPT: &str = concat!(
    "You clean up speech-to-text transcripts. ",
    "Rewrite the user's transcript into clean written form: ",
    "fix punctuation and capitalization; correct obvious misrecognitions; ",
    "remove filler words (um, uh, like, you know); collapse false starts; ",
    "fix grammar. Preserve the speaker's meaning and intent. ",
    "Output only the cleaned text with no prefixes, quotes, or commentary."
);

/// Build a refiner from the current config. Returns `Ok(None)` when the
/// feature is disabled so the caller can treat "no refiner" as the default.
///
/// # Errors
///
/// Returns `RefineError` when the feature is enabled but misconfigured (e.g.
/// the `llm-refine` Cargo feature was disabled at build time, or the model
/// field is empty).
pub fn try_build(config: &Config) -> Result<Option<Arc<dyn TextRefiner>>, RefineError> {
    if !config.llm_refine_enabled {
        return Ok(None);
    }
    if config.llm_refine_model.trim().is_empty() {
        return Err(RefineError::Configuration(
            "LLM_REFINE_MODEL is empty".into(),
        ));
    }
    #[cfg(feature = "llm-refine")]
    {
        let refiner = genai_refiner::GenAiRefiner::from_config(config)?;
        Ok(Some(Arc::new(refiner)))
    }
    #[cfg(not(feature = "llm-refine"))]
    {
        Err(RefineError::Configuration(
            "LLM_REFINE_ENABLED=true but binary was built without the `llm-refine` feature"
                .into(),
        ))
    }
}

/// Refine `text`, falling back to the original on any error. Safe to call
/// from contexts where there's no `&App` (e.g. a spawned output task).
///
/// Short-circuits without calling the refiner when it's `None`, when `text`
/// is empty / whitespace, or when it's shorter than `min_chars`.
pub async fn refine_or_fallback(
    refiner: Option<&Arc<dyn TextRefiner>>,
    text: String,
    min_chars: usize,
    scope: RefineScope,
) -> String {
    let Some(r) = refiner else {
        return text;
    };
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() < min_chars {
        return text;
    }
    match r.refine(&text).await {
        Ok(cleaned) => {
            let cleaned = cleaned.trim().to_string();
            if cleaned.is_empty() {
                eprintln!("[Refine/{scope}] empty response, keeping original");
                text
            } else {
                cleaned
            }
        }
        Err(e) => {
            eprintln!("[Refine/{scope}] {e}; emitting original transcript");
            text
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct StubRefiner {
        reply: String,
    }

    #[async_trait]
    impl TextRefiner for StubRefiner {
        async fn refine(&self, _text: &str) -> Result<String, RefineError> {
            Ok(self.reply.clone())
        }
    }

    struct FailingRefiner;

    #[async_trait]
    impl TextRefiner for FailingRefiner {
        async fn refine(&self, _text: &str) -> Result<String, RefineError> {
            Err(RefineError::Api("boom".into()))
        }
    }

    #[tokio::test]
    async fn returns_original_when_no_refiner() {
        let out =
            refine_or_fallback(None, "hello".into(), 0, RefineScope::Batch).await;
        assert_eq!(out, "hello");
    }

    #[tokio::test]
    async fn returns_original_when_empty() {
        let stub: Arc<dyn TextRefiner> = Arc::new(StubRefiner {
            reply: "never".into(),
        });
        let out = refine_or_fallback(Some(&stub), "   ".into(), 0, RefineScope::Batch).await;
        assert_eq!(out, "   ");
    }

    #[tokio::test]
    async fn returns_original_when_below_min_chars() {
        let stub: Arc<dyn TextRefiner> = Arc::new(StubRefiner {
            reply: "never".into(),
        });
        let out = refine_or_fallback(Some(&stub), "hi".into(), 10, RefineScope::Batch).await;
        assert_eq!(out, "hi");
    }

    #[tokio::test]
    async fn returns_refined_text_on_success() {
        let stub: Arc<dyn TextRefiner> = Arc::new(StubRefiner {
            reply: "Cleaned up sentence.".into(),
        });
        let out = refine_or_fallback(
            Some(&stub),
            "um so yeah cleaned up sentence".into(),
            0,
            RefineScope::Continuous,
        )
        .await;
        assert_eq!(out, "Cleaned up sentence.");
    }

    #[tokio::test]
    async fn falls_back_to_original_on_error() {
        let failing: Arc<dyn TextRefiner> = Arc::new(FailingRefiner);
        let original = "keep this".to_string();
        let out = refine_or_fallback(
            Some(&failing),
            original.clone(),
            0,
            RefineScope::Batch,
        )
        .await;
        assert_eq!(out, original);
    }

    #[tokio::test]
    async fn falls_back_when_llm_returns_empty() {
        let stub: Arc<dyn TextRefiner> = Arc::new(StubRefiner {
            reply: "   ".into(),
        });
        let out = refine_or_fallback(
            Some(&stub),
            "keep original".into(),
            0,
            RefineScope::Continuous,
        )
        .await;
        assert_eq!(out, "keep original");
    }

    #[test]
    fn try_build_returns_none_when_disabled() {
        let cfg = Config::default();
        let refiner = try_build(&cfg).expect("disabled should be Ok(None)");
        assert!(refiner.is_none());
    }

    #[test]
    fn try_build_errors_on_empty_model() {
        let cfg = Config {
            llm_refine_enabled: true,
            llm_refine_model: "   ".to_string(),
            ..Config::default()
        };
        match try_build(&cfg) {
            Err(RefineError::Configuration(_)) => {}
            Ok(_) => panic!("expected Err, got Ok"),
            Err(e) => panic!("expected Configuration error, got {e}"),
        }
    }
}
