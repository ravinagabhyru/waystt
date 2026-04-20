#![allow(clippy::float_cmp)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Configuration for waystt loaded from environment variables
#[derive(Debug, Clone)]
pub struct Config {
    pub openai_api_key: Option<String>,
    pub openai_base_url: Option<String>,
    pub transcription_provider: String,
    pub audio_buffer_duration_seconds: usize,
    pub audio_sample_rate: u32,
    pub audio_channels: u16,
    pub whisper_model: String,
    pub whisper_language: String,
    pub whisper_timeout_seconds: u64,
    pub whisper_max_retries: u32,
    pub rust_log: String,
    pub enable_audio_feedback: bool,
    pub beep_volume: f32,
    // Google Speech-to-Text configuration
    pub google_application_credentials: Option<String>,
    pub google_speech_language_code: String,
    pub google_speech_model: String,
    pub google_speech_alternative_languages: Vec<String>,
    // Parakeet configuration
    pub parakeet_model_type: String,
    pub parakeet_model_path: Option<String>,
    /// ONNX Runtime intra-op thread count for Parakeet. `None` keeps the
    /// parakeet-rs default (4). Set to your physical core count for
    /// CPU-bound throughput wins on 6+ core boxes.
    pub parakeet_intra_threads: Option<usize>,
    /// ONNX Runtime inter-op thread count for Parakeet. `None` keeps the
    /// parakeet-rs default (1). Rarely worth raising above 1 for this model.
    pub parakeet_inter_threads: Option<usize>,
    // Continuous mode tunables (see `[continuous]` in config.toml.example)
    pub continuous_min_speech_ms: u64,
    pub continuous_silence_threshold_ms: u64,
    pub continuous_max_chunk_ms: u64,
    pub continuous_worker_count: usize,
    pub continuous_max_queue_size: usize,
    // LLM post-processing ("refinement") configuration
    pub llm_refine_enabled: bool,
    pub llm_refine_apply_batch: bool,
    pub llm_refine_apply_continuous: bool,
    pub llm_refine_base_url: Option<String>,
    pub llm_refine_api_key: Option<String>,
    pub llm_refine_model: String,
    pub llm_refine_timeout_ms: u64,
    pub llm_refine_system_prompt: Option<String>,
    pub llm_refine_max_tokens: Option<u32>,
    pub llm_refine_min_chars: usize,
}

/// TOML-facing representation of the config file. Sections map to the
/// grouped `[audio]`, `[openai]`, … blocks. Every field is optional so
/// partial files are valid; absent values fall back to [`Config::default`].
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConfigFile {
    transcription_provider: Option<String>,
    rust_log: Option<String>,
    audio: AudioSection,
    beep: BeepSection,
    openai: OpenAiSection,
    whisper: WhisperSection,
    google: GoogleSection,
    parakeet: ParakeetSection,
    continuous: ContinuousSection,
    llm_refine: LlmRefineSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AudioSection {
    sample_rate: Option<u32>,
    channels: Option<u16>,
    buffer_duration_seconds: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BeepSection {
    enabled: Option<bool>,
    volume: Option<f32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct OpenAiSection {
    api_key: Option<String>,
    base_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct WhisperSection {
    model: Option<String>,
    language: Option<String>,
    timeout_seconds: Option<u64>,
    max_retries: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct GoogleSection {
    application_credentials: Option<String>,
    language_code: Option<String>,
    model: Option<String>,
    alternative_languages: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ParakeetSection {
    model_type: Option<String>,
    model_path: Option<String>,
    intra_threads: Option<usize>,
    inter_threads: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ContinuousSection {
    min_speech_ms: Option<u64>,
    silence_threshold_ms: Option<u64>,
    max_chunk_ms: Option<u64>,
    worker_count: Option<usize>,
    max_queue_size: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LlmRefineSection {
    enabled: Option<bool>,
    apply_batch: Option<bool>,
    apply_continuous: Option<bool>,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    timeout_ms: Option<u64>,
    system_prompt: Option<String>,
    max_tokens: Option<u32>,
    min_chars: Option<usize>,
}

impl ConfigFile {
    /// Apply non-empty values from a parsed TOML file onto `config`.
    fn merge_into(self, config: &mut Config) {
        if let Some(v) = self.transcription_provider {
            config.transcription_provider = v;
        }
        if let Some(v) = self.rust_log {
            config.rust_log = v;
        }
        if let Some(v) = self.audio.sample_rate {
            config.audio_sample_rate = v;
        }
        if let Some(v) = self.audio.channels {
            config.audio_channels = v;
        }
        if let Some(v) = self.audio.buffer_duration_seconds {
            config.audio_buffer_duration_seconds = v;
        }
        if let Some(v) = self.beep.enabled {
            config.enable_audio_feedback = v;
        }
        if let Some(v) = self.beep.volume {
            config.beep_volume = v.clamp(0.0, 1.0);
        }
        if self.openai.api_key.is_some() {
            config.openai_api_key = self.openai.api_key;
        }
        if self.openai.base_url.is_some() {
            config.openai_base_url = self.openai.base_url;
        }
        if let Some(v) = self.whisper.model {
            config.whisper_model = v;
        }
        if let Some(v) = self.whisper.language {
            config.whisper_language = v;
        }
        if let Some(v) = self.whisper.timeout_seconds {
            config.whisper_timeout_seconds = v;
        }
        if let Some(v) = self.whisper.max_retries {
            config.whisper_max_retries = v;
        }
        if self.google.application_credentials.is_some() {
            config.google_application_credentials = self.google.application_credentials;
        }
        if let Some(v) = self.google.language_code {
            config.google_speech_language_code = v;
        }
        if let Some(v) = self.google.model {
            config.google_speech_model = v;
        }
        if let Some(v) = self.google.alternative_languages {
            config.google_speech_alternative_languages = v;
        }
        if let Some(v) = self.parakeet.model_type {
            config.parakeet_model_type = v.to_lowercase();
        }
        if self.parakeet.model_path.is_some() {
            config.parakeet_model_path = self.parakeet.model_path;
        }
        if self.parakeet.intra_threads.is_some() {
            config.parakeet_intra_threads = self.parakeet.intra_threads;
        }
        if self.parakeet.inter_threads.is_some() {
            config.parakeet_inter_threads = self.parakeet.inter_threads;
        }
        if let Some(v) = self.continuous.min_speech_ms {
            config.continuous_min_speech_ms = v;
        }
        if let Some(v) = self.continuous.silence_threshold_ms {
            config.continuous_silence_threshold_ms = v;
        }
        if let Some(v) = self.continuous.max_chunk_ms {
            config.continuous_max_chunk_ms = v;
        }
        if let Some(v) = self.continuous.worker_count {
            config.continuous_worker_count = v.clamp(1, 4);
        }
        if let Some(v) = self.continuous.max_queue_size {
            config.continuous_max_queue_size = v;
        }
        if let Some(v) = self.llm_refine.enabled {
            config.llm_refine_enabled = v;
        }
        if let Some(v) = self.llm_refine.apply_batch {
            config.llm_refine_apply_batch = v;
        }
        if let Some(v) = self.llm_refine.apply_continuous {
            config.llm_refine_apply_continuous = v;
        }
        if self.llm_refine.base_url.is_some() {
            config.llm_refine_base_url = self.llm_refine.base_url;
        }
        if self.llm_refine.api_key.is_some() {
            config.llm_refine_api_key = self.llm_refine.api_key;
        }
        if let Some(v) = self.llm_refine.model {
            if !v.trim().is_empty() {
                config.llm_refine_model = v;
            }
        }
        if let Some(v) = self.llm_refine.timeout_ms {
            config.llm_refine_timeout_ms = v;
        }
        if self.llm_refine.system_prompt.is_some() {
            config.llm_refine_system_prompt =
                self.llm_refine.system_prompt.filter(|s| !s.is_empty());
        }
        if let Some(v) = self.llm_refine.max_tokens {
            config.llm_refine_max_tokens = Some(v);
        }
        if let Some(v) = self.llm_refine.min_chars {
            config.llm_refine_min_chars = v;
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            openai_api_key: None,
            openai_base_url: None,
            transcription_provider: "openai".to_string(),
            audio_buffer_duration_seconds: 300, // 5 minutes
            audio_sample_rate: 16000,           // Optimized for Whisper
            audio_channels: 1,                  // Mono
            whisper_model: "whisper-1".to_string(),
            whisper_language: "auto".to_string(),
            whisper_timeout_seconds: 60,
            whisper_max_retries: 3,
            rust_log: "info".to_string(),
            enable_audio_feedback: true,
            beep_volume: 0.1,
            // Google Speech-to-Text defaults
            google_application_credentials: None,
            google_speech_language_code: "en-US".to_string(),
            google_speech_model: "latest_long".to_string(),
            google_speech_alternative_languages: vec![],
            // Parakeet defaults
            parakeet_model_type: "ctc".to_string(),
            parakeet_model_path: None,
            parakeet_intra_threads: None,
            parakeet_inter_threads: None,
            // Continuous mode defaults
            continuous_min_speech_ms: 300,
            continuous_silence_threshold_ms: 700,
            continuous_max_chunk_ms: 30_000,
            continuous_worker_count: 2,
            continuous_max_queue_size: 10,
            // LLM refinement defaults (disabled)
            llm_refine_enabled: false,
            llm_refine_apply_batch: true,
            llm_refine_apply_continuous: true,
            llm_refine_base_url: None,
            llm_refine_api_key: None,
            llm_refine_model: "gpt-4o-mini".to_string(),
            llm_refine_timeout_ms: 5000,
            llm_refine_system_prompt: None,
            llm_refine_max_tokens: None,
            llm_refine_min_chars: 0,
        }
    }
}

impl Config {
    /// Directory where local whisper models are stored
    #[must_use]
    pub fn model_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local/share/applications/waystt/models")
    }

    /// Full path to a model file in the model directory
    #[must_use]
    pub fn model_path(model: &str) -> PathBuf {
        Self::model_dir().join(model)
    }

    /// Directory where parakeet models are stored
    #[must_use]
    pub fn parakeet_model_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local/share/applications/waystt/parakeet")
    }

    /// Full path to a parakeet model directory based on model type
    #[must_use]
    pub fn parakeet_model_path(model_type: &str) -> PathBuf {
        Self::parakeet_model_dir().join(model_type)
    }

    /// Build a Config from environment variables alone (no file).
    /// Equivalent to `Config::default()` then [`Config::apply_env_overrides`].
    #[must_use]
    pub fn from_env() -> Self {
        let mut config = Config::default();
        config.apply_env_overrides();
        config
    }

    /// Overlay environment variables onto `self`. Every variable is optional
    /// and invalid numeric values are silently ignored. Env vars take
    /// precedence over file-loaded values.
    #[allow(clippy::too_many_lines)]
    pub fn apply_env_overrides(&mut self) {
        let config = self;

        // Load OpenAI API key (only overrides when the env var is set; an
        // unset env var must not wipe out a value loaded from config.toml).
        if let Ok(v) = std::env::var("OPENAI_API_KEY") {
            config.openai_api_key = Some(v);
        }
        if let Ok(v) = std::env::var("OPENAI_BASE_URL") {
            config.openai_base_url = Some(v);
        }

        // Load transcription provider
        if let Ok(provider) = std::env::var("TRANSCRIPTION_PROVIDER") {
            config.transcription_provider = provider;
        }

        // Load audio configuration
        if let Ok(duration) = std::env::var("AUDIO_BUFFER_DURATION_SECONDS") {
            if let Ok(parsed) = duration.parse::<usize>() {
                config.audio_buffer_duration_seconds = parsed;
            }
        }

        if let Ok(sample_rate) = std::env::var("AUDIO_SAMPLE_RATE") {
            if let Ok(parsed) = sample_rate.parse::<u32>() {
                config.audio_sample_rate = parsed;
            }
        }

        if let Ok(channels) = std::env::var("AUDIO_CHANNELS") {
            if let Ok(parsed) = channels.parse::<u16>() {
                config.audio_channels = parsed;
            }
        }

        // Load transcription configuration
        if let Ok(model) = std::env::var("WHISPER_MODEL") {
            config.whisper_model = model;
        }

        if let Ok(language) = std::env::var("WHISPER_LANGUAGE") {
            config.whisper_language = language;
        }

        if let Ok(timeout) = std::env::var("WHISPER_TIMEOUT_SECONDS") {
            if let Ok(parsed) = timeout.parse::<u64>() {
                config.whisper_timeout_seconds = parsed;
            }
        }

        if let Ok(retries) = std::env::var("WHISPER_MAX_RETRIES") {
            if let Ok(parsed) = retries.parse::<u32>() {
                config.whisper_max_retries = parsed;
            }
        }

        // Load logging configuration
        if let Ok(log_level) = std::env::var("RUST_LOG") {
            config.rust_log = log_level;
        }

        // Load audio feedback configuration
        if let Ok(enabled) = std::env::var("ENABLE_AUDIO_FEEDBACK") {
            config.enable_audio_feedback = enabled.to_lowercase() == "true";
        }

        if let Ok(volume) = std::env::var("BEEP_VOLUME") {
            if let Ok(parsed) = volume.parse::<f32>() {
                config.beep_volume = parsed.clamp(0.0, 1.0);
            }
        }

        // Load Google Speech-to-Text configuration
        if let Ok(v) = std::env::var("GOOGLE_APPLICATION_CREDENTIALS") {
            config.google_application_credentials = Some(v);
        }

        if let Ok(language) = std::env::var("GOOGLE_SPEECH_LANGUAGE_CODE") {
            config.google_speech_language_code = language;
        }

        if let Ok(model) = std::env::var("GOOGLE_SPEECH_MODEL") {
            config.google_speech_model = model;
        }

        if let Ok(alt_languages) = std::env::var("GOOGLE_SPEECH_ALTERNATIVE_LANGUAGES") {
            config.google_speech_alternative_languages = alt_languages
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }

        // Load Parakeet configuration
        if let Ok(model_type) = std::env::var("PARAKEET_MODEL_TYPE") {
            config.parakeet_model_type = model_type.to_lowercase();
        }
        if let Ok(v) = std::env::var("PARAKEET_MODEL_PATH") {
            config.parakeet_model_path = Some(v);
        }
        if let Ok(v) = std::env::var("PARAKEET_INTRA_THREADS") {
            if let Ok(parsed) = v.parse::<usize>() {
                config.parakeet_intra_threads = Some(parsed);
            }
        }
        if let Ok(v) = std::env::var("PARAKEET_INTER_THREADS") {
            if let Ok(parsed) = v.parse::<usize>() {
                config.parakeet_inter_threads = Some(parsed);
            }
        }

        // Load LLM refinement configuration
        if let Ok(v) = std::env::var("LLM_REFINE_ENABLED") {
            config.llm_refine_enabled = v.to_lowercase() == "true";
        }
        if let Ok(v) = std::env::var("LLM_REFINE_APPLY_BATCH") {
            config.llm_refine_apply_batch = v.to_lowercase() == "true";
        }
        if let Ok(v) = std::env::var("LLM_REFINE_APPLY_CONTINUOUS") {
            config.llm_refine_apply_continuous = v.to_lowercase() == "true";
        }
        if let Ok(v) = std::env::var("LLM_REFINE_BASE_URL") {
            config.llm_refine_base_url = Some(v);
        }
        if let Ok(v) = std::env::var("LLM_REFINE_API_KEY") {
            config.llm_refine_api_key = Some(v);
        }
        if let Ok(v) = std::env::var("LLM_REFINE_MODEL") {
            if !v.trim().is_empty() {
                config.llm_refine_model = v;
            }
        }
        if let Ok(v) = std::env::var("LLM_REFINE_TIMEOUT_MS") {
            if let Ok(parsed) = v.parse::<u64>() {
                config.llm_refine_timeout_ms = parsed;
            }
        }
        if let Ok(v) = std::env::var("LLM_REFINE_SYSTEM_PROMPT") {
            config.llm_refine_system_prompt = if v.is_empty() { None } else { Some(v) };
        }
        if let Ok(v) = std::env::var("LLM_REFINE_MAX_TOKENS") {
            if let Ok(parsed) = v.parse::<u32>() {
                config.llm_refine_max_tokens = Some(parsed);
            }
        }
        if let Ok(v) = std::env::var("LLM_REFINE_MIN_CHARS") {
            if let Ok(parsed) = v.parse::<usize>() {
                config.llm_refine_min_chars = parsed;
            }
        }

        // Continuous mode tunables
        if let Ok(v) = std::env::var("CONTINUOUS_MIN_SPEECH_MS") {
            if let Ok(parsed) = v.parse::<u64>() {
                config.continuous_min_speech_ms = parsed;
            }
        }
        if let Ok(v) = std::env::var("CONTINUOUS_SILENCE_MS") {
            if let Ok(parsed) = v.parse::<u64>() {
                config.continuous_silence_threshold_ms = parsed;
            }
        }
        if let Ok(v) = std::env::var("CONTINUOUS_MAX_CHUNK_MS") {
            if let Ok(parsed) = v.parse::<u64>() {
                config.continuous_max_chunk_ms = parsed;
            }
        }
        if let Ok(v) = std::env::var("CONTINUOUS_WORKERS") {
            if let Ok(parsed) = v.parse::<usize>() {
                config.continuous_worker_count = parsed.clamp(1, 4);
            }
        }
        if let Ok(v) = std::env::var("CONTINUOUS_MAX_QUEUE_SIZE") {
            if let Ok(parsed) = v.parse::<usize>() {
                config.continuous_max_queue_size = parsed;
            }
        }
    }

    /// Load a TOML config file into a fresh Config (defaults → file values).
    /// Env-var overlay is NOT applied here; callers that want env overrides
    /// must also call [`Config::apply_env_overrides`].
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or is not valid TOML
    /// matching the expected schema (unknown keys are rejected).
    pub fn load_toml_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let parsed: ConfigFile = toml::from_str(&contents)
            .with_context(|| format!("parsing config file {}", path.display()))?;
        let mut config = Config::default();
        parsed.merge_into(&mut config);
        Ok(config)
    }

    /// Validate configuration
    ///
    /// # Errors
    ///
    /// Returns an error if required configuration values are missing or invalid
    pub fn validate(&self) -> Result<()> {
        // Provider-specific validation
        match self.transcription_provider.as_str() {
            "openai" => {
                if self.openai_api_key.is_none() {
                    return Err(anyhow::anyhow!(
                        "OPENAI_API_KEY (or [openai].api_key in config.toml) is required when using the OpenAI provider."
                    ));
                }
            }
            "local" => {
                let model_path = Config::model_path(&self.whisper_model);
                if !model_path.exists() {
                    return Err(anyhow::anyhow!(
                        "Local model not found at {}. Use --download-model to fetch it.",
                        model_path.display()
                    ));
                }
            }
            "google" => {
                if self.google_application_credentials.is_none() {
                    return Err(anyhow::anyhow!(
                        "GOOGLE_APPLICATION_CREDENTIALS is required when using Google provider. Please set it to the path of your service account JSON file."
                    ));
                }
            }
            "parakeet" => {
                #[cfg(not(feature = "parakeet"))]
                {
                    return Err(anyhow::anyhow!(
                        "Parakeet provider is not available. Rebuild with --features parakeet"
                    ));
                }
                #[cfg(feature = "parakeet")]
                {
                    let model_path = if let Some(ref custom_path) = self.parakeet_model_path {
                        std::path::PathBuf::from(custom_path)
                    } else {
                        Config::parakeet_model_path(&self.parakeet_model_type)
                    };
                    if !model_path.exists() {
                        return Err(anyhow::anyhow!(
                            "Parakeet model not found at {}. Download models from HuggingFace to this directory.",
                            model_path.display()
                        ));
                    }
                }
            }
            _ => {
                let provider = &self.transcription_provider;
                return Err(anyhow::anyhow!(
                    "Unsupported transcription provider: {provider}. Supported providers: openai, google, local, parakeet"
                ));
            }
        }

        if self.audio_buffer_duration_seconds == 0 {
            return Err(anyhow::anyhow!(
                "AUDIO_BUFFER_DURATION_SECONDS must be greater than 0"
            ));
        }

        if self.audio_sample_rate == 0 {
            return Err(anyhow::anyhow!("AUDIO_SAMPLE_RATE must be greater than 0"));
        }

        if self.audio_channels == 0 {
            return Err(anyhow::anyhow!("AUDIO_CHANNELS must be greater than 0"));
        }

        if self.beep_volume < 0.0 || self.beep_volume > 1.0 {
            let vol = self.beep_volume;
            return Err(anyhow::anyhow!(
                "BEEP_VOLUME must be between 0.0 and 1.0, got: {vol}"
            ));
        }

        Ok(())
    }
}

/// Load configuration from environment variables
#[must_use]
pub fn load_config() -> Config {
    Config::from_env()
}

/// Return the default config path, i.e. `~/.config/waystt/config.toml`.
#[must_use]
pub fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::env::var("HOME").map_or_else(|_| PathBuf::from("."), PathBuf::from))
        .join("waystt")
        .join("config.toml")
}

/// Bootstrap configuration.
///
/// Loads `~/.config/waystt/config.toml` by default, or the file at
/// `config_path` when provided, then overlays environment variables. Env
/// vars always win over file values.
///
/// Behavior when the file is missing:
/// - explicit `config_path` → error,
/// - default path → silently use env vars only.
///
/// # Errors
///
/// Returns an error if the config file cannot be read or parsed, or if
/// validation fails for local/parakeet providers (missing model files).
pub fn bootstrap(config_path: Option<&Path>) -> anyhow::Result<Config> {
    let mut cfg = match config_path {
        Some(path) => {
            if !path.exists() {
                anyhow::bail!("config file not found: {}", path.display());
            }
            Config::load_toml_file(path)?
        }
        None => {
            let def = default_config_path();
            if def.exists() {
                Config::load_toml_file(&def)?
            } else {
                maybe_warn_legacy_env_file();
                Config::default()
            }
        }
    };
    cfg.apply_env_overrides();

    // Validate configuration but allow non-fatal warnings for providers other than local/parakeet
    if let Err(e) = cfg.validate() {
        eprintln!("Configuration warning: {e}");
        if cfg.transcription_provider == "local" || cfg.transcription_provider == "parakeet" {
            // For local providers, validation is strict because model presence is required
            return Err(e);
        }
    }

    // Parakeet EOU is English-only. Warn (don't fail) when users pick a
    // non-English WHISPER_LANGUAGE with it — the model may still run but the
    // language tag is meaningless for EOU's English-only vocabulary.
    if cfg.transcription_provider == "parakeet"
        && cfg.parakeet_model_type.eq_ignore_ascii_case("eou")
    {
        let lang = cfg.whisper_language.trim();
        let english_ok = lang.is_empty()
            || lang.eq_ignore_ascii_case("auto")
            || lang.eq_ignore_ascii_case("en")
            || lang.to_ascii_lowercase().starts_with("en-")
            || lang.to_ascii_lowercase().starts_with("en_");
        if !english_ok {
            eprintln!(
                "Warning: PARAKEET_MODEL_TYPE=eou is English-only; \
                 WHISPER_LANGUAGE={lang} will be ignored."
            );
        }
    }

    Ok(cfg)
}

/// Warn once if the legacy `~/.config/waystt/.env` file exists but the user
/// hasn't created `config.toml` — they almost certainly meant for it to be
/// loaded and are confused about why it's being ignored.
fn maybe_warn_legacy_env_file() {
    let legacy = dirs::config_dir()
        .unwrap_or_else(|| std::env::var("HOME").map_or_else(|_| PathBuf::from("."), PathBuf::from))
        .join("waystt")
        .join(".env");
    if legacy.exists() {
        eprintln!(
            "Note: waystt no longer reads {} ; configuration has moved to {}. \
             See config.toml.example for the new format.",
            legacy.display(),
            default_config_path().display()
        );
    }
}

impl Config {
    /// Map the configured provider to the strongly-typed kind.
    #[must_use]
    pub fn provider_kind(&self) -> crate::transcription::ProviderKind {
        match self.transcription_provider.to_lowercase().as_str() {
            "google" => crate::transcription::ProviderKind::Google,
            "local" => crate::transcription::ProviderKind::Local,
            #[cfg(feature = "parakeet")]
            "parakeet" => crate::transcription::ProviderKind::Parakeet,
            _ => crate::transcription::ProviderKind::OpenAI,
        }
    }

    /// True when LLM refinement should run for daemon/batch transcribe flows.
    #[must_use]
    pub fn refine_enabled_for_batch(&self) -> bool {
        self.llm_refine_enabled && self.llm_refine_apply_batch
    }

    /// True when LLM refinement should run for continuous / streaming emission.
    #[must_use]
    pub fn refine_enabled_for_continuous(&self) -> bool {
        self.llm_refine_enabled && self.llm_refine_apply_continuous
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::ENV_MUTEX;
    use std::env;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // Helper function to clear all waystt environment variables
    fn clear_env_vars() {
        env::remove_var("OPENAI_API_KEY");
        env::remove_var("OPENAI_BASE_URL");
        env::remove_var("TRANSCRIPTION_PROVIDER");
        env::remove_var("AUDIO_BUFFER_DURATION_SECONDS");
        env::remove_var("AUDIO_SAMPLE_RATE");
        env::remove_var("AUDIO_CHANNELS");
        env::remove_var("WHISPER_MODEL");
        env::remove_var("WHISPER_LANGUAGE");
        env::remove_var("WHISPER_TIMEOUT_SECONDS");
        env::remove_var("WHISPER_MAX_RETRIES");
        env::remove_var("RUST_LOG");
        env::remove_var("ENABLE_AUDIO_FEEDBACK");
        env::remove_var("BEEP_VOLUME");
        env::remove_var("GOOGLE_APPLICATION_CREDENTIALS");
        env::remove_var("GOOGLE_SPEECH_LANGUAGE_CODE");
        env::remove_var("GOOGLE_SPEECH_MODEL");
        env::remove_var("GOOGLE_SPEECH_ALTERNATIVE_LANGUAGES");
        env::remove_var("PARAKEET_MODEL_TYPE");
        env::remove_var("PARAKEET_MODEL_PATH");
        env::remove_var("PARAKEET_INTRA_THREADS");
        env::remove_var("PARAKEET_INTER_THREADS");
        env::remove_var("LLM_REFINE_ENABLED");
        env::remove_var("LLM_REFINE_APPLY_BATCH");
        env::remove_var("LLM_REFINE_APPLY_CONTINUOUS");
        env::remove_var("LLM_REFINE_BASE_URL");
        env::remove_var("LLM_REFINE_API_KEY");
        env::remove_var("LLM_REFINE_MODEL");
        env::remove_var("LLM_REFINE_TIMEOUT_MS");
        env::remove_var("LLM_REFINE_SYSTEM_PROMPT");
        env::remove_var("LLM_REFINE_MAX_TOKENS");
        env::remove_var("LLM_REFINE_MIN_CHARS");
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.openai_api_key, None);
        assert_eq!(config.openai_base_url, None);
        assert_eq!(config.transcription_provider, "openai");
        assert_eq!(config.audio_buffer_duration_seconds, 300);
        assert_eq!(config.audio_sample_rate, 16000);
        assert_eq!(config.audio_channels, 1);
        assert_eq!(config.whisper_model, "whisper-1");
        assert_eq!(config.whisper_language, "auto");
        assert_eq!(config.rust_log, "info");
        assert!(config.enable_audio_feedback);
        assert!((config.beep_volume - 0.1).abs() < f32::EPSILON);
        // Google defaults
        assert_eq!(config.google_application_credentials, None);
        assert_eq!(config.google_speech_language_code, "en-US");
        assert_eq!(config.google_speech_model, "latest_long");
        assert!(config.google_speech_alternative_languages.is_empty());
        // Parakeet defaults
        assert_eq!(config.parakeet_model_type, "ctc");
        assert_eq!(config.parakeet_model_path, None);
        assert_eq!(config.parakeet_intra_threads, None);
        assert_eq!(config.parakeet_inter_threads, None);
    }

    #[tokio::test]
    async fn test_config_from_env_defaults() {
        #[allow(clippy::await_holding_lock)]
        {
            let _lock = ENV_MUTEX.lock().await;

            // Clear all environment variables first
            clear_env_vars();

            let config = Config::from_env();
            assert_eq!(config.openai_api_key, None);
            assert_eq!(config.openai_base_url, None);
            assert_eq!(config.transcription_provider, "openai");
            assert_eq!(config.audio_buffer_duration_seconds, 300);
            assert_eq!(config.audio_sample_rate, 16000);
            assert_eq!(config.audio_channels, 1);
            assert_eq!(config.whisper_model, "whisper-1");
            assert_eq!(config.whisper_language, "auto");
            assert_eq!(config.whisper_timeout_seconds, 60);
            assert_eq!(config.whisper_max_retries, 3);
            assert_eq!(config.rust_log, "info");

            // Clean up after test
            clear_env_vars();
        }
    }

    #[tokio::test]
    async fn test_config_from_env_variables() {
        #[allow(clippy::await_holding_lock)]
        {
            let _lock = ENV_MUTEX.lock().await;

            // Clear environment variables first to ensure clean state
            clear_env_vars();

            // Set environment variables
            env::set_var("OPENAI_API_KEY", "test-api-key");
            env::set_var("AUDIO_BUFFER_DURATION_SECONDS", "600");
            env::set_var("AUDIO_SAMPLE_RATE", "44100");
            env::set_var("AUDIO_CHANNELS", "2");
            env::set_var("WHISPER_MODEL", "whisper-large");
            env::set_var("WHISPER_LANGUAGE", "en");
            env::set_var("WHISPER_TIMEOUT_SECONDS", "120");
            env::set_var("WHISPER_MAX_RETRIES", "5");
            env::set_var("RUST_LOG", "debug");
            env::set_var("TRANSCRIPTION_PROVIDER", "google");
            env::set_var("OPENAI_BASE_URL", "http://localhost:8080");

            let config = Config::from_env();
            assert_eq!(config.openai_api_key, Some("test-api-key".to_string()));
            assert_eq!(
                config.openai_base_url,
                Some("http://localhost:8080".to_string())
            );
            assert_eq!(config.transcription_provider, "google");
            assert_eq!(config.audio_buffer_duration_seconds, 600);
            assert_eq!(config.audio_sample_rate, 44100);
            assert_eq!(config.audio_channels, 2);
            assert_eq!(config.whisper_model, "whisper-large");
            assert_eq!(config.whisper_language, "en");
            assert_eq!(config.whisper_timeout_seconds, 120);
            assert_eq!(config.whisper_max_retries, 5);
            assert_eq!(config.rust_log, "debug");

            // Clean up after test
            clear_env_vars();
        }
    }

    #[tokio::test]
    async fn test_config_from_env_invalid_numbers() {
        #[allow(clippy::await_holding_lock)]
        {
            let _lock = ENV_MUTEX.lock().await;

            // Clear at the start
            clear_env_vars();

            // Set invalid numeric values
            env::set_var("AUDIO_BUFFER_DURATION_SECONDS", "invalid");
            env::set_var("AUDIO_SAMPLE_RATE", "not-a-number");
            env::set_var("AUDIO_CHANNELS", "bad");
            env::set_var("WHISPER_TIMEOUT_SECONDS", "invalid");
            env::set_var("WHISPER_MAX_RETRIES", "bad");

            let config = Config::from_env();

            // Should fallback to defaults for invalid values
            assert_eq!(config.audio_buffer_duration_seconds, 300);
            assert_eq!(config.audio_sample_rate, 16000);
            assert_eq!(config.audio_channels, 1);
            assert_eq!(config.whisper_timeout_seconds, 60);
            assert_eq!(config.whisper_max_retries, 3);

            clear_env_vars();
        }
    }

    #[test]
    fn test_load_toml_file() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "transcription_provider = \"openai\"").unwrap();
        writeln!(temp_file, "rust_log = \"warn\"").unwrap();
        writeln!(temp_file).unwrap();
        writeln!(temp_file, "[audio]").unwrap();
        writeln!(temp_file, "buffer_duration_seconds = 120").unwrap();
        writeln!(temp_file).unwrap();
        writeln!(temp_file, "[openai]").unwrap();
        writeln!(temp_file, "api_key = \"file-api-key\"").unwrap();
        writeln!(temp_file, "base_url = \"http://localhost:8080\"").unwrap();
        writeln!(temp_file).unwrap();
        writeln!(temp_file, "[whisper]").unwrap();
        writeln!(temp_file, "model = \"whisper-base\"").unwrap();

        let config = Config::load_toml_file(temp_file.path()).unwrap();

        assert_eq!(config.openai_api_key, Some("file-api-key".to_string()));
        assert_eq!(
            config.openai_base_url,
            Some("http://localhost:8080".to_string())
        );
        assert_eq!(config.transcription_provider, "openai");
        assert_eq!(config.audio_buffer_duration_seconds, 120);
        assert_eq!(config.whisper_model, "whisper-base");
        assert_eq!(config.rust_log, "warn");

        // Unset fields should retain defaults
        assert_eq!(config.audio_sample_rate, 16000);
        assert_eq!(config.audio_channels, 1);
        assert_eq!(config.whisper_language, "auto");
    }

    #[test]
    fn test_load_toml_with_llm_refine_section() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "[llm_refine]").unwrap();
        writeln!(f, "enabled = true").unwrap();
        writeln!(f, "model = \"llama3.2\"").unwrap();
        writeln!(f, "base_url = \"http://localhost:11434/v1\"").unwrap();
        writeln!(f, "timeout_ms = 2500").unwrap();
        writeln!(f, "min_chars = 4").unwrap();

        let c = Config::load_toml_file(f.path()).unwrap();
        assert!(c.llm_refine_enabled);
        assert_eq!(c.llm_refine_model, "llama3.2");
        assert_eq!(
            c.llm_refine_base_url.as_deref(),
            Some("http://localhost:11434/v1")
        );
        assert_eq!(c.llm_refine_timeout_ms, 2500);
        assert_eq!(c.llm_refine_min_chars, 4);
    }

    #[test]
    fn test_load_toml_with_continuous_section() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "[continuous]").unwrap();
        writeln!(f, "min_speech_ms = 500").unwrap();
        writeln!(f, "silence_threshold_ms = 1000").unwrap();
        writeln!(f, "max_chunk_ms = 15000").unwrap();
        writeln!(f, "worker_count = 3").unwrap();
        writeln!(f, "max_queue_size = 20").unwrap();

        let c = Config::load_toml_file(f.path()).unwrap();
        assert_eq!(c.continuous_min_speech_ms, 500);
        assert_eq!(c.continuous_silence_threshold_ms, 1000);
        assert_eq!(c.continuous_max_chunk_ms, 15_000);
        assert_eq!(c.continuous_worker_count, 3);
        assert_eq!(c.continuous_max_queue_size, 20);
    }

    #[test]
    fn test_continuous_worker_count_clamped_from_toml() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "[continuous]").unwrap();
        writeln!(f, "worker_count = 99").unwrap();

        let c = Config::load_toml_file(f.path()).unwrap();
        assert_eq!(c.continuous_worker_count, 4);
    }

    #[test]
    fn test_continuous_defaults_preserved_when_section_absent() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "[openai]").unwrap();
        writeln!(f, "api_key = \"x\"").unwrap();

        let c = Config::load_toml_file(f.path()).unwrap();
        assert_eq!(c.continuous_min_speech_ms, 300);
        assert_eq!(c.continuous_silence_threshold_ms, 700);
        assert_eq!(c.continuous_max_chunk_ms, 30_000);
        assert_eq!(c.continuous_worker_count, 2);
        assert_eq!(c.continuous_max_queue_size, 10);
    }

    #[test]
    fn test_load_nonexistent_toml_file() {
        let result = Config::load_toml_file("/nonexistent/path/config.toml");
        assert!(result.is_err());
    }

    #[test]
    fn test_load_toml_rejects_unknown_keys() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "[openai]").unwrap();
        writeln!(f, "api_key = \"sk-x\"").unwrap();
        writeln!(f, "typo_field = \"oops\"").unwrap();
        let result = Config::load_toml_file(f.path());
        assert!(result.is_err(), "unknown keys must fail");
    }

    #[tokio::test]
    async fn test_env_overrides_toml_values() {
        #[allow(clippy::await_holding_lock)]
        {
            let _lock = ENV_MUTEX.lock().await;
            clear_env_vars();

            let mut f = NamedTempFile::new().unwrap();
            writeln!(f, "[openai]").unwrap();
            writeln!(f, "api_key = \"from-file\"").unwrap();
            writeln!(f, "[whisper]").unwrap();
            writeln!(f, "model = \"whisper-file\"").unwrap();

            env::set_var("OPENAI_API_KEY", "from-env");
            env::set_var("WHISPER_MODEL", "whisper-env");

            let mut c = Config::load_toml_file(f.path()).unwrap();
            c.apply_env_overrides();

            assert_eq!(c.openai_api_key.as_deref(), Some("from-env"));
            assert_eq!(c.whisper_model, "whisper-env");

            clear_env_vars();
        }
    }

    #[tokio::test]
    async fn test_unset_env_does_not_clobber_toml() {
        #[allow(clippy::await_holding_lock)]
        {
            let _lock = ENV_MUTEX.lock().await;
            clear_env_vars();

            let mut f = NamedTempFile::new().unwrap();
            writeln!(f, "[openai]").unwrap();
            writeln!(f, "api_key = \"keep-me\"").unwrap();

            // OPENAI_API_KEY is NOT set — apply_env_overrides must leave
            // the file-loaded value alone.
            let mut c = Config::load_toml_file(f.path()).unwrap();
            c.apply_env_overrides();

            assert_eq!(c.openai_api_key.as_deref(), Some("keep-me"));

            clear_env_vars();
        }
    }

    #[test]
    fn test_config_validation_success() {
        let config = Config {
            openai_api_key: Some("test-key".to_string()),
            ..Default::default()
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validation_missing_api_key() {
        let config = Config::default(); // No API key

        let result = config.validate();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("OPENAI_API_KEY is required"));
    }

    #[test]
    fn test_config_validation_invalid_duration() {
        let config = Config {
            openai_api_key: Some("test-key".to_string()),
            audio_buffer_duration_seconds: 0,
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("AUDIO_BUFFER_DURATION_SECONDS"));
    }

    #[test]
    fn test_config_validation_invalid_sample_rate() {
        let config = Config {
            openai_api_key: Some("test-key".to_string()),
            audio_sample_rate: 0,
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("AUDIO_SAMPLE_RATE"));
    }

    #[test]
    fn test_config_validation_invalid_channels() {
        let config = Config {
            openai_api_key: Some("test-key".to_string()),
            audio_channels: 0,
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("AUDIO_CHANNELS"));
    }

    #[test]
    fn test_config_validation_invalid_beep_volume() {
        // Test negative volume
        let config = Config {
            openai_api_key: Some("test-key".to_string()),
            beep_volume: -0.1,
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("BEEP_VOLUME"));

        // Test volume > 1.0
        let config2 = Config {
            openai_api_key: Some("test-key".to_string()),
            beep_volume: 1.1,
            ..Default::default()
        };
        let result = config2.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("BEEP_VOLUME"));
    }

    #[tokio::test]
    async fn test_config_audio_feedback_env_vars() {
        #[allow(clippy::await_holding_lock)]
        {
            let _lock = ENV_MUTEX.lock().await;
            clear_env_vars();

            // Test enabled audio feedback
            env::set_var("ENABLE_AUDIO_FEEDBACK", "true");
            env::set_var("BEEP_VOLUME", "0.5");

            let config = Config::from_env();
            assert!(config.enable_audio_feedback);
            assert!((config.beep_volume - 0.5).abs() < f32::EPSILON);

            clear_env_vars();

            // Test disabled audio feedback
            env::set_var("ENABLE_AUDIO_FEEDBACK", "false");
            env::set_var("BEEP_VOLUME", "0.8");

            let config = Config::from_env();
            assert!(!config.enable_audio_feedback);
            assert!((config.beep_volume - 0.8).abs() < f32::EPSILON);

            clear_env_vars();
        }
    }

    #[tokio::test]
    async fn test_config_audio_feedback_invalid_env_vars() {
        #[allow(clippy::await_holding_lock)]
        {
            let _lock = ENV_MUTEX.lock().await;
            clear_env_vars();

            // Test invalid volume values
            env::set_var("BEEP_VOLUME", "invalid");
            let config = Config::from_env();
            assert!((config.beep_volume - 0.1).abs() < f32::EPSILON); // Should use default

            // Test volume clamping
            env::set_var("BEEP_VOLUME", "2.0");
            let config = Config::from_env();
            assert!((config.beep_volume - 1.0).abs() < f32::EPSILON); // Should be clamped to 1.0

            env::set_var("BEEP_VOLUME", "-0.5");
            let config = Config::from_env();
            assert!(config.beep_volume.abs() < f32::EPSILON); // Should be clamped to 0.0

            clear_env_vars();
        }
    }

    #[tokio::test]
    async fn test_transcription_provider_configuration() {
        #[allow(clippy::await_holding_lock)]
        {
            let _lock = ENV_MUTEX.lock().await;
            clear_env_vars();

            // Test default provider
            let config = Config::from_env();
            assert_eq!(config.transcription_provider, "openai");

            // Test custom provider
            env::set_var("TRANSCRIPTION_PROVIDER", "google");
            let config = Config::from_env();
            assert_eq!(config.transcription_provider, "google");

            clear_env_vars();
        }
    }

    #[tokio::test]
    async fn test_backward_compatibility_validation() {
        #[allow(clippy::await_holding_lock)]
        {
            let _lock = ENV_MUTEX.lock().await;
            clear_env_vars();

            // Test that OpenAI provider requires API key
            env::set_var("TRANSCRIPTION_PROVIDER", "openai");
            let config = Config::from_env();
            assert!(config.validate().is_err());

            // Test that OpenAI provider works with API key
            env::set_var("OPENAI_API_KEY", "test-key");
            let config = Config::from_env();
            assert!(config.validate().is_ok());

            // Test that Google provider requires Google credentials (but not OpenAI key)
            env::remove_var("OPENAI_API_KEY");
            env::set_var("TRANSCRIPTION_PROVIDER", "google");
            let config = Config::from_env();
            // This should fail validation without Google credentials
            assert!(config.validate().is_err());

            // Test that Google provider works with credentials
            env::set_var("GOOGLE_APPLICATION_CREDENTIALS", "/path/to/creds.json");
            let config = Config::from_env();
            // This should pass validation with Google credentials (no OpenAI key needed)
            assert!(config.validate().is_ok());

            clear_env_vars();
        }
    }

    #[tokio::test]
    async fn test_google_config_from_env() {
        #[allow(clippy::await_holding_lock)]
        {
            let _lock = ENV_MUTEX.lock().await;
            clear_env_vars();

            // Set Google-specific environment variables
            env::set_var("TRANSCRIPTION_PROVIDER", "google");
            env::set_var(
                "GOOGLE_APPLICATION_CREDENTIALS",
                "/path/to/credentials.json",
            );
            env::set_var("GOOGLE_SPEECH_LANGUAGE_CODE", "es-ES");
            env::set_var("GOOGLE_SPEECH_MODEL", "latest_short");
            env::set_var("GOOGLE_SPEECH_ALTERNATIVE_LANGUAGES", "en-US,fr-FR,de-DE");

            let config = Config::from_env();
            assert_eq!(config.transcription_provider, "google");
            assert_eq!(
                config.google_application_credentials,
                Some("/path/to/credentials.json".to_string())
            );
            assert_eq!(config.google_speech_language_code, "es-ES");
            assert_eq!(config.google_speech_model, "latest_short");
            assert_eq!(
                config.google_speech_alternative_languages,
                vec!["en-US", "fr-FR", "de-DE"]
            );

            clear_env_vars();
        }
    }

    #[tokio::test]
    async fn test_google_alternative_languages_parsing() {
        #[allow(clippy::await_holding_lock)]
        {
            let _lock = ENV_MUTEX.lock().await;
            clear_env_vars();

            // Test with spaces and empty entries
            env::set_var(
                "GOOGLE_SPEECH_ALTERNATIVE_LANGUAGES",
                "en-US, fr-FR , , de-DE,",
            );
            let config = Config::from_env();
            assert_eq!(
                config.google_speech_alternative_languages,
                vec!["en-US", "fr-FR", "de-DE"]
            );

            // Test empty string
            env::set_var("GOOGLE_SPEECH_ALTERNATIVE_LANGUAGES", "");
            let config = Config::from_env();
            assert!(config.google_speech_alternative_languages.is_empty());

            clear_env_vars();
        }
    }

    #[test]
    fn test_config_validation_google_missing_credentials() {
        let config = Config {
            transcription_provider: "google".to_string(),
            google_application_credentials: None,
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("GOOGLE_APPLICATION_CREDENTIALS"));
    }

    #[test]
    fn test_config_validation_google_success() {
        let config = Config {
            transcription_provider: "google".to_string(),
            google_application_credentials: Some("/path/to/creds.json".to_string()),
            ..Default::default()
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validation_unsupported_provider() {
        let config = Config {
            transcription_provider: "azure".to_string(),
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unsupported transcription provider: azure"));
    }

    #[tokio::test]
    async fn test_config_validation_local_missing_model() {
        use crate::test_utils::ENV_MUTEX;
        let _lock = ENV_MUTEX.lock().await;
        let tmp_home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp_home.path());

        let config = Config {
            transcription_provider: "local".to_string(),
            whisper_model: "missing.bin".to_string(),
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_refine_defaults() {
        let c = Config::default();
        assert!(!c.llm_refine_enabled);
        assert!(c.llm_refine_apply_batch);
        assert!(c.llm_refine_apply_continuous);
        assert_eq!(c.llm_refine_model, "gpt-4o-mini");
        assert_eq!(c.llm_refine_timeout_ms, 5000);
        assert_eq!(c.llm_refine_min_chars, 0);
        assert!(!c.refine_enabled_for_batch());
        assert!(!c.refine_enabled_for_continuous());
    }

    #[tokio::test]
    async fn test_refine_env_parsing() {
        #[allow(clippy::await_holding_lock)]
        {
            let _lock = ENV_MUTEX.lock().await;
            clear_env_vars();

            env::set_var("LLM_REFINE_ENABLED", "true");
            env::set_var("LLM_REFINE_APPLY_BATCH", "false");
            env::set_var("LLM_REFINE_APPLY_CONTINUOUS", "true");
            env::set_var("LLM_REFINE_BASE_URL", "http://localhost:11434/v1");
            env::set_var("LLM_REFINE_API_KEY", "sk-test");
            env::set_var("LLM_REFINE_MODEL", "llama3.2");
            env::set_var("LLM_REFINE_TIMEOUT_MS", "2500");
            env::set_var("LLM_REFINE_SYSTEM_PROMPT", "Clean it up.");
            env::set_var("LLM_REFINE_MAX_TOKENS", "200");
            env::set_var("LLM_REFINE_MIN_CHARS", "5");

            let c = Config::from_env();
            assert!(c.llm_refine_enabled);
            assert!(!c.llm_refine_apply_batch);
            assert!(c.llm_refine_apply_continuous);
            assert_eq!(
                c.llm_refine_base_url.as_deref(),
                Some("http://localhost:11434/v1")
            );
            assert_eq!(c.llm_refine_api_key.as_deref(), Some("sk-test"));
            assert_eq!(c.llm_refine_model, "llama3.2");
            assert_eq!(c.llm_refine_timeout_ms, 2500);
            assert_eq!(c.llm_refine_system_prompt.as_deref(), Some("Clean it up."));
            assert_eq!(c.llm_refine_max_tokens, Some(200));
            assert_eq!(c.llm_refine_min_chars, 5);
            assert!(!c.refine_enabled_for_batch());
            assert!(c.refine_enabled_for_continuous());

            clear_env_vars();
        }
    }

    #[tokio::test]
    async fn test_refine_env_invalid_numbers_fall_back() {
        #[allow(clippy::await_holding_lock)]
        {
            let _lock = ENV_MUTEX.lock().await;
            clear_env_vars();

            env::set_var("LLM_REFINE_ENABLED", "true");
            env::set_var("LLM_REFINE_TIMEOUT_MS", "oops");
            env::set_var("LLM_REFINE_MIN_CHARS", "nope");
            env::set_var("LLM_REFINE_MAX_TOKENS", "bad");

            let c = Config::from_env();
            assert_eq!(c.llm_refine_timeout_ms, 5000);
            assert_eq!(c.llm_refine_min_chars, 0);
            assert_eq!(c.llm_refine_max_tokens, None);

            clear_env_vars();
        }
    }

    #[tokio::test]
    async fn test_config_validation_local_success() {
        use crate::test_utils::ENV_MUTEX;
        let _lock = ENV_MUTEX.lock().await;
        let tmp_home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp_home.path());

        let model_path = Config::model_path("dummy.bin");
        std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
        std::fs::write(&model_path, b"test").unwrap();

        let config = Config {
            transcription_provider: "local".to_string(),
            whisper_model: "dummy.bin".to_string(),
            ..Default::default()
        };

        assert!(config.validate().is_ok());
    }
}
