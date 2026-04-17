use super::{ApiErrorDetails, TranscriptionError, TranscriptionProvider};
use async_trait::async_trait;
use std::path::{Path, PathBuf};

/// Model type for Parakeet transcription
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParakeetModelType {
    /// CTC model - English only, faster
    CTC,
    /// TDT model - Multilingual (25 languages), auto-detection
    TDT,
}

impl ParakeetModelType {
    /// Parse model type from string
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "tdt" => Self::TDT,
            _ => Self::CTC,
        }
    }
}

/// Parakeet transcription provider using NVIDIA Parakeet models via ONNX Runtime
pub struct ParakeetProvider {
    model_type: ParakeetModelType,
    model_path: PathBuf,
}

impl ParakeetProvider {
    /// Create a new Parakeet provider
    ///
    /// # Errors
    ///
    /// Returns an error if the model directory does not exist
    pub fn new(
        model_path: &Path,
        model_type: ParakeetModelType,
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
        })
    }
}

#[async_trait]
impl TranscriptionProvider for ParakeetProvider {
    async fn transcribe_with_language(
        &self,
        audio_data: Vec<u8>,
        _language: Option<String>,
    ) -> Result<String, TranscriptionError> {
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

        // Clone values needed for the blocking task
        let model_path = self.model_path.clone();
        let model_type = self.model_type;

        // Run transcription in blocking task since parakeet-rs may not be Send
        let result = tokio::task::spawn_blocking(move || {
            transcribe_blocking(&model_path, model_type, samples, sample_rate, channels)
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

/// Blocking transcription function to run in spawn_blocking
fn transcribe_blocking(
    model_path: &Path,
    model_type: ParakeetModelType,
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u16,
) -> Result<String, TranscriptionError> {
    let model_path_str = model_path
        .to_str()
        .ok_or_else(|| TranscriptionError::ConfigurationError("Invalid model path".to_string()))?;

    match model_type {
        ParakeetModelType::CTC => {
            let mut parakeet = parakeet_rs::Parakeet::from_pretrained(model_path_str, None)
                .map_err(|e| {
                    TranscriptionError::ApiError(ApiErrorDetails {
                        provider: "Parakeet".to_string(),
                        status_code: None,
                        error_code: Some("MODEL_LOAD_ERROR".to_string()),
                        error_message: format!("Failed to load CTC model: {e}"),
                        raw_response: None,
                    })
                })?;

            let result = parakeet
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
        ParakeetModelType::TDT => {
            let mut parakeet = parakeet_rs::ParakeetTDT::from_pretrained(model_path_str, None)
                .map_err(|e| {
                    TranscriptionError::ApiError(ApiErrorDetails {
                        provider: "Parakeet".to_string(),
                        status_code: None,
                        error_code: Some("MODEL_LOAD_ERROR".to_string()),
                        error_message: format!("Failed to load TDT model: {e}"),
                        raw_response: None,
                    })
                })?;

            let result = parakeet
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
        // Default to CTC for unknown
        assert_eq!(ParakeetModelType::parse("unknown"), ParakeetModelType::CTC);
        assert_eq!(ParakeetModelType::parse(""), ParakeetModelType::CTC);
    }

    #[test]
    fn test_parakeet_provider_missing_model() {
        let model_path = std::path::Path::new("/nonexistent/parakeet/model");
        let result = ParakeetProvider::new(model_path, ParakeetModelType::CTC);
        assert!(result.is_err());

        if let Err(TranscriptionError::ConfigurationError(msg)) = result {
            assert!(msg.contains("not found"));
        } else {
            panic!("Expected ConfigurationError");
        }
    }
}
