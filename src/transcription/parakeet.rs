use super::{ApiErrorDetails, TranscriptionError, TranscriptionProvider};
use async_trait::async_trait;
use parakeet_rs::Transcriber;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Model type for Parakeet transcription
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParakeetModelType {
    /// CTC model - English only, faster (batch / one-shot only)
    CTC,
    /// TDT model - Multilingual (25 languages), auto-detection (batch / one-shot only)
    TDT,
    /// EOU model - English only, streaming with end-of-utterance detection
    /// (used only by the streaming path — see `parakeet_streaming.rs`).
    EOU,
}

impl ParakeetModelType {
    /// Parse model type from string
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "tdt" => Self::TDT,
            "eou" => Self::EOU,
            _ => Self::CTC,
        }
    }
}

/// Loaded ONNX model, held across transcription calls. `ort::Session` (inside
/// both variants) is `Send + Sync`, but `transcribe_samples` takes `&mut self`,
/// so we serialize callers with a `Mutex`. waystt transcribes one utterance at
/// a time so this contention is expected, not a bottleneck.
///
/// The CTC variant is `Box`ed because its inner struct is ~4x the size of
/// `ParakeetTDT` — without boxing clippy flags `large_enum_variant`. The
/// heap indirection has no observable cost next to ONNX inference.
enum CachedModel {
    Ctc(Box<parakeet_rs::Parakeet>),
    Tdt(Box<parakeet_rs::ParakeetTDT>),
}

/// Parakeet transcription provider using NVIDIA Parakeet models via ONNX Runtime
pub struct ParakeetProvider {
    model_type: ParakeetModelType,
    model_path: PathBuf,
    intra_threads: Option<usize>,
    inter_threads: Option<usize>,
    /// Lazily-initialized model cache. Built on first transcription (or
    /// explicitly via [`ParakeetProvider::warm_up`]) and reused thereafter.
    cached: Arc<Mutex<Option<CachedModel>>>,
}

impl ParakeetProvider {
    /// Create a new Parakeet provider.
    ///
    /// `intra_threads` / `inter_threads` map directly onto ONNX Runtime's
    /// intra-op / inter-op thread pools. Pass `None` to keep parakeet-rs's
    /// defaults (4 / 1).
    ///
    /// # Errors
    ///
    /// Returns an error if the model directory does not exist
    pub fn new(
        model_path: &Path,
        model_type: ParakeetModelType,
        intra_threads: Option<usize>,
        inter_threads: Option<usize>,
    ) -> Result<Self, TranscriptionError> {
        if !model_path.exists() {
            return Err(TranscriptionError::ConfigurationError(format!(
                "Parakeet model directory not found: {}",
                model_path.display()
            )));
        }

        Ok(Self {
            model_type,
            model_path: model_path.to_path_buf(),
            intra_threads,
            inter_threads,
            cached: Arc::new(Mutex::new(None)),
        })
    }

    /// Build the ONNX execution config from raw thread overrides. Called on
    /// the blocking thread because `ExecutionConfig` holds a non-`Send`
    /// `Rc<dyn Fn>` for arbitrary session-builder hooks (parakeet-rs 0.3.2+).
    fn build_exec_config(
        intra_threads: Option<usize>,
        inter_threads: Option<usize>,
    ) -> parakeet_rs::ExecutionConfig {
        let mut cfg = parakeet_rs::ExecutionConfig::default();
        if let Some(n) = intra_threads {
            cfg = cfg.with_intra_threads(n);
        }
        if let Some(n) = inter_threads {
            cfg = cfg.with_inter_threads(n);
        }
        cfg
    }

    /// Pre-load the ONNX model so the first transcription doesn't pay the
    /// cold-load cost. Safe to call multiple times; subsequent calls are
    /// no-ops.
    ///
    /// EOU is rejected here (it goes through the streaming provider).
    ///
    /// # Errors
    ///
    /// Returns an error if the model files cannot be loaded.
    pub fn warm_up(&self) -> Result<(), TranscriptionError> {
        if self.model_type == ParakeetModelType::EOU {
            return Ok(());
        }
        let mut guard = self.cached.lock().map_err(|_| {
            TranscriptionError::ConfigurationError(
                "Parakeet model cache mutex poisoned".to_string(),
            )
        })?;
        if guard.is_some() {
            return Ok(());
        }
        let exec_config = Self::build_exec_config(self.intra_threads, self.inter_threads);
        *guard = Some(load_model(&self.model_path, self.model_type, exec_config)?);
        Ok(())
    }
}

#[async_trait]
impl TranscriptionProvider for ParakeetProvider {
    async fn transcribe_with_language(
        &self,
        audio_data: Vec<u8>,
        _language: Option<String>,
    ) -> Result<String, TranscriptionError> {
        if self.model_type == ParakeetModelType::EOU {
            return Err(TranscriptionError::ConfigurationError(
                "EOU model requires continuous or daemon-streaming mode; \
                 use PARAKEET_MODEL_TYPE=ctc or tdt for one-shot transcription"
                    .to_string(),
            ));
        }

        if let Ok(dump_path) = std::env::var("WAYSTT_DUMP_WAV") {
            match std::fs::write(&dump_path, &audio_data) {
                Ok(()) => eprintln!("🎧 Wrote captured WAV to {dump_path} ({} bytes)", audio_data.len()),
                Err(e) => eprintln!("⚠️  Failed to dump WAV to {dump_path}: {e}"),
            }
        }

        // Decode WAV to PCM samples (same pattern as local.rs)
        let reader = hound::WavReader::new(std::io::Cursor::new(audio_data)).map_err(|e| {
            TranscriptionError::ConfigurationError(format!("Failed to read WAV data: {e}"))
        })?;

        let spec = reader.spec();
        let sample_rate = spec.sample_rate;
        let channels = spec.channels;

        let samples: Result<Vec<f32>, _> = reader
            .into_samples::<i16>()
            .map(|s| s.map(|v| f32::from(v) / f32::from(i16::MAX)))
            .collect();

        let samples = samples.map_err(|e| {
            TranscriptionError::ConfigurationError(format!("Failed to parse WAV samples: {e}"))
        })?;

        let cached = Arc::clone(&self.cached);
        let model_path = self.model_path.clone();
        let model_type = self.model_type;
        let intra_threads = self.intra_threads;
        let inter_threads = self.inter_threads;

        // ONNX inference is CPU-bound and blocking; keep it off the tokio worker.
        let result = tokio::task::spawn_blocking(move || {
            let mut guard = cached.lock().map_err(|_| {
                TranscriptionError::ConfigurationError(
                    "Parakeet model cache mutex poisoned".to_string(),
                )
            })?;
            if guard.is_none() {
                let exec_config = Self::build_exec_config(intra_threads, inter_threads);
                *guard = Some(load_model(&model_path, model_type, exec_config)?);
            }
            // Unwrap: just populated above.
            let model = guard.as_mut().expect("cached model present");
            transcribe_with_cached(model, samples, sample_rate, channels)
        })
        .await
        .map_err(|e| {
            TranscriptionError::ApiError(ApiErrorDetails {
                provider: "Parakeet".to_string(),
                status_code: None,
                error_code: Some("TASK_JOIN_ERROR".to_string()),
                error_message: format!("Failed to join blocking task: {e}"),
                raw_response: None,
            })
        })??;

        Ok(result)
    }
}

fn load_model(
    model_path: &Path,
    model_type: ParakeetModelType,
    exec_config: parakeet_rs::ExecutionConfig,
) -> Result<CachedModel, TranscriptionError> {
    let model_path_str = model_path
        .to_str()
        .ok_or_else(|| TranscriptionError::ConfigurationError("Invalid model path".to_string()))?;

    match model_type {
        ParakeetModelType::CTC => {
            let model = parakeet_rs::Parakeet::from_pretrained(model_path_str, Some(exec_config))
                .map_err(|e| {
                    TranscriptionError::ApiError(ApiErrorDetails {
                        provider: "Parakeet".to_string(),
                        status_code: None,
                        error_code: Some("MODEL_LOAD_ERROR".to_string()),
                        error_message: format!("Failed to load CTC model: {e}"),
                        raw_response: None,
                    })
                })?;
            Ok(CachedModel::Ctc(Box::new(model)))
        }
        ParakeetModelType::TDT => {
            let model =
                parakeet_rs::ParakeetTDT::from_pretrained(model_path_str, Some(exec_config))
                    .map_err(|e| {
                        TranscriptionError::ApiError(ApiErrorDetails {
                            provider: "Parakeet".to_string(),
                            status_code: None,
                            error_code: Some("MODEL_LOAD_ERROR".to_string()),
                            error_message: format!("Failed to load TDT model: {e}"),
                            raw_response: None,
                        })
                    })?;
            Ok(CachedModel::Tdt(Box::new(model)))
        }
        ParakeetModelType::EOU => Err(TranscriptionError::ConfigurationError(
            "EOU model does not support batch transcription".to_string(),
        )),
    }
}

fn transcribe_with_cached(
    model: &mut CachedModel,
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u16,
) -> Result<String, TranscriptionError> {
    match model {
        CachedModel::Ctc(m) => {
            let result = m
                .transcribe_samples(
                    samples,
                    sample_rate,
                    channels,
                    Some(parakeet_rs::TimestampMode::Words),
                )
                .map_err(|e| {
                    TranscriptionError::ApiError(ApiErrorDetails {
                        provider: "Parakeet".to_string(),
                        status_code: None,
                        error_code: Some("TRANSCRIPTION_ERROR".to_string()),
                        error_message: format!("CTC transcription failed: {e}"),
                        raw_response: None,
                    })
                })?;
            Ok(result.text)
        }
        CachedModel::Tdt(m) => {
            let result = m
                .transcribe_samples(
                    samples,
                    sample_rate,
                    channels,
                    Some(parakeet_rs::TimestampMode::Sentences),
                )
                .map_err(|e| {
                    TranscriptionError::ApiError(ApiErrorDetails {
                        provider: "Parakeet".to_string(),
                        status_code: None,
                        error_code: Some("TRANSCRIPTION_ERROR".to_string()),
                        error_message: format!("TDT transcription failed: {e}"),
                        raw_response: None,
                    })
                })?;
            Ok(result.text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parakeet_model_type_parsing() {
        assert_eq!(ParakeetModelType::parse("ctc"), ParakeetModelType::CTC);
        assert_eq!(ParakeetModelType::parse("CTC"), ParakeetModelType::CTC);
        assert_eq!(ParakeetModelType::parse("tdt"), ParakeetModelType::TDT);
        assert_eq!(ParakeetModelType::parse("TDT"), ParakeetModelType::TDT);
        assert_eq!(ParakeetModelType::parse("eou"), ParakeetModelType::EOU);
        assert_eq!(ParakeetModelType::parse("EOU"), ParakeetModelType::EOU);
        // Default to CTC for unknown
        assert_eq!(ParakeetModelType::parse("unknown"), ParakeetModelType::CTC);
        assert_eq!(ParakeetModelType::parse(""), ParakeetModelType::CTC);
    }

    #[tokio::test]
    async fn test_eou_model_rejects_batch_transcription() {
        let tmp = tempfile::tempdir().unwrap();
        // Directory exists so the provider itself initializes.
        let provider =
            ParakeetProvider::new(tmp.path(), ParakeetModelType::EOU, None, None).expect("provider");
        let result = provider.transcribe_with_language(vec![], None).await;
        match result {
            Err(TranscriptionError::ConfigurationError(msg)) => {
                assert!(msg.contains("EOU model requires continuous or daemon-streaming mode"));
            }
            other => panic!("expected ConfigurationError, got {other:?}"),
        }
    }

    #[test]
    fn test_parakeet_provider_missing_model() {
        let model_path = std::path::Path::new("/nonexistent/parakeet/model");
        let result = ParakeetProvider::new(model_path, ParakeetModelType::CTC, None, None);
        assert!(result.is_err());

        if let Err(TranscriptionError::ConfigurationError(msg)) = result {
            assert!(msg.contains("not found"));
        } else {
            panic!("Expected ConfigurationError");
        }
    }

    #[test]
    fn test_exec_config_overrides_thread_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let provider =
            ParakeetProvider::new(tmp.path(), ParakeetModelType::CTC, Some(12), Some(2))
                .expect("provider");
        let cfg = ParakeetProvider::build_exec_config(provider.intra_threads, provider.inter_threads);
        // We only expose Debug on ExecutionConfig; round-trip through the
        // struct's fields by inspecting its Debug repr since the fields
        // are pub(crate). This guards against accidentally dropping the
        // overrides in exec_config().
        let repr = format!("{cfg:?}");
        assert!(
            repr.contains("intra_threads: 12"),
            "expected intra_threads=12 in {repr}"
        );
        assert!(
            repr.contains("inter_threads: 2"),
            "expected inter_threads=2 in {repr}"
        );
    }

    #[test]
    fn test_exec_config_defaults_when_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let provider = ParakeetProvider::new(tmp.path(), ParakeetModelType::CTC, None, None)
            .expect("provider");
        let cfg = ParakeetProvider::build_exec_config(provider.intra_threads, provider.inter_threads);
        let repr = format!("{cfg:?}");
        // parakeet-rs defaults: intra=4, inter=1. See
        // `execution.rs` ModelConfig::default in the crate.
        assert!(
            repr.contains("intra_threads: 4"),
            "expected parakeet-rs default intra_threads=4 in {repr}"
        );
    }
}
