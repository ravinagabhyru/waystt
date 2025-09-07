use anyhow::Result;
use futures::stream::StreamExt;

use crate::audio::AudioRecorder;
use crate::beep::{BeepConfig, BeepPlayer, BeepType};
use crate::cli::RunOptions;
use crate::command;
use crate::config::Config;
use crate::pipeline::AudioPipeline;
use crate::signals;
use crate::transcription::{TranscriptionError, TranscriptionProvider};

pub struct App {
    config: Config,
    recorder: AudioRecorder,
    beeps: BeepPlayer,
    pipeline: AudioPipeline,
    provider: Box<dyn TranscriptionProvider>,
    pipe_to: Option<Vec<String>>,
    daemon: bool,
}

impl App {
    /// Initialize the application
    ///
    /// # Errors
    ///
    /// Returns an error if audio devices cannot be initialized or configured
    #[allow(clippy::unused_async)]
    pub async fn init(
        options: RunOptions,
        config: Config,
        provider: Box<dyn TranscriptionProvider>,
    ) -> Result<Self> {
        let beep_config = BeepConfig {
            enabled: config.enable_audio_feedback,
            volume: config.beep_volume,
        };
        let beeps = BeepPlayer::new(beep_config)?;
        let recorder = AudioRecorder::new()?;
        let pipeline = AudioPipeline::new(config.audio_sample_rate);

        Ok(Self {
            config,
            recorder,
            beeps,
            pipeline,
            provider,
            pipe_to: options.pipe_to,
            daemon: options.daemon,
        })
    }

    /// Run the application main loop
    ///
    /// # Errors
    ///
    /// Returns an error if signal handling fails or audio recording cannot be started
    pub async fn run(mut self) -> Result<i32> {
        eprintln!("waystt - Wayland Speech-to-Text Tool");

        // App state machine modes
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum RecState {
            Idle,
            Recording,
        }

        let mut state = if self.daemon {
            eprintln!("Daemon mode: waiting for SIGUSR2 to start recording");
            RecState::Idle
        } else {
            eprintln!("One-shot mode: starting audio recording...");
            if let Err(e) = self.beeps.play_async(BeepType::RecordingStart).await {
                eprintln!("Warning: Failed to play recording start beep: {e}");
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(600)).await;
            if let Err(e) = self.recorder.start_recording() {
                eprintln!("Failed to start recording: {e}");
                return Ok(1);
            }
            RecState::Recording
        };

        // Signals
        let mut signals = signals::build_signal_stream()?;

        loop {
            // Drive background audio events
            if let Err(e) = self.recorder.process_audio_events() {
                eprintln!("Audio event processing error: {e}");
            }

            // Poll signals with timeout to keep loop responsive
            match tokio::time::timeout(tokio::time::Duration::from_millis(100), signals.next()).await {
                Ok(Some(signal)) => {
                    if signal == signals::START_SIG {
                        match state {
                            RecState::Recording => {
                                eprintln!("SIGUSR2 received but already recording; ignoring");
                            }
                            RecState::Idle => {
                                eprintln!("Received SIGUSR2: Starting recording");
                                // Ensure fresh buffer
                                if let Err(e) = self.recorder.clear_buffer() {
                                    eprintln!("Failed to clear audio buffer before start: {e}");
                                }
                                if let Err(e) = self.beeps.play_async(BeepType::RecordingStart).await {
                                    eprintln!("Warning: Failed to play recording start beep: {e}");
                                }
                                tokio::time::sleep(tokio::time::Duration::from_millis(600)).await;
                                if let Err(e) = self.recorder.start_recording() {
                                    eprintln!("Failed to start recording: {e}");
                                } else {
                                    state = RecState::Recording;
                                }
                            }
                        }
                    } else if signal == signals::TRANSCRIBE_SIG {
                        match state {
                            RecState::Idle => {
                                eprintln!("SIGUSR1 received while idle; nothing to transcribe");
                            }
                            RecState::Recording => {
                                // Stop recording for processing
                                if let Err(e) = self.recorder.stop_recording() {
                                    eprintln!("Failed to stop recording: {e}");
                                }
                                // Play stop beep to signal end of capture
                                if let Err(e) = self.beeps.play_async(BeepType::RecordingStop).await {
                                    eprintln!("Warning: Failed to play recording stop beep: {e}");
                                }

                                let duration = self
                                    .recorder
                                    .get_recording_duration_seconds()
                                    .unwrap_or_default();
                                eprintln!(
                                    "Received SIGUSR1: Starting transcription for {duration:.2}s buffer"
                                );

                                let audio_data = match self.recorder.get_audio_data() {
                                    Ok(d) => d,
                                    Err(e) => {
                                        eprintln!("Failed to get audio data: {e}");
                                        if self.daemon {
                                            state = RecState::Idle;
                                            continue;
                                        }
                                        return Ok(1);
                                    }
                                };

                                let res = self.process_and_transcribe(audio_data).await;

                                // Clear buffer to free memory regardless of outcome
                                if let Err(e) = self.recorder.clear_buffer() {
                                    eprintln!("Failed to clear audio buffer: {e}");
                                }

                                match res {
                                    Ok(code) => {
                                        if self.daemon {
                                            state = RecState::Idle;
                                            // stay alive for next cycle
                                        } else {
                                            return Ok(code);
                                        }
                                    }
                                    Err(_) => {
                                        if self.daemon {
                                            state = RecState::Idle;
                                        }
                                        if !self.daemon {
                                            return Ok(1);
                                        }
                                    }
                                }
                            }
                        }
                    } else if signal == signals::SHUTDOWN_SIG {
                        eprintln!("Received SIGTERM: Shutting down gracefully");
                        if let Err(e) = self.recorder.stop_recording() {
                            eprintln!("Failed to stop recording: {e}");
                        }
                        // Play stop beep on shutdown as well
                        if let Err(e) = self.beeps.play_async(BeepType::RecordingStop).await {
                            eprintln!("Warning: Failed to play recording stop beep: {e}");
                        }
                        if let Err(e) = self.recorder.clear_buffer() {
                            eprintln!("Failed to clear audio buffer during shutdown: {e}");
                        }
                        return Ok(0);
                    } else {
                        eprintln!("Received unexpected signal: {signal}");
                    }
                }
                Ok(None) => break, // stream ended
                Err(_) => continue, // timeout
            }
        }

        eprintln!("Exiting waystt");
        Ok(0)
    }

    async fn process_and_transcribe(&self, audio_data: Vec<f32>) -> Result<i32> {
        let len = audio_data.len();
        eprintln!("Processing audio: {len} samples");

        // Preprocess
        let processed = match self.pipeline.preprocess(&audio_data) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Audio processing failed: {e}");
                let _ = self.beeps.play_async(BeepType::Error).await;
                return Ok(1);
            }
        };

        // Encode WAV
        let wav = match self.pipeline.to_wav(&processed) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("Failed to encode WAV: {e}");
                return Ok(1);
            }
        };

        // Transcribe
        // Normalize language: treat "auto" or empty as None for providers like OpenAI
        let language_opt = {
            let s = self.config.whisper_language.trim();
            if s.is_empty() || s.eq_ignore_ascii_case("auto") {
                None
            } else {
                Some(s.to_string())
            }
        };

        match self
            .pipeline
            .transcribe(wav, self.provider.as_ref(), language_opt)
            .await
        {
            Ok(text) => {
                if text.is_empty() {
                    println!();
                    let _ = self.beeps.play_async(BeepType::Success).await;
                    return Ok(0);
                }
                eprintln!("Transcription successful: \"{text}\"");
                let exit_code = if let Some(cmd) = &self.pipe_to {
                    match command::execute_with_input(cmd, &text).await {
                        Ok(code) => code,
                        Err(e) => {
                            eprintln!("Failed to execute pipe command: {e}");
                            let _ = self.beeps.play_async(BeepType::Error).await;
                            1
                        }
                    }
                } else {
                    println!("{text}");
                    0
                };
                let _ = self.beeps.play_async(BeepType::Success).await;
                Ok(exit_code)
            }
            Err(e) => {
                eprintln!("❌ Transcription failed: {e}");
                let _ = self.beeps.play_async(BeepType::Error).await;
                // Provide helpful hints based on error type (minimal version)
                match &e {
                    TranscriptionError::AuthenticationFailed { provider, .. } => {
                        eprintln!("💡 Check your {provider} API key configuration");
                    }
                    TranscriptionError::NetworkError(details) => {
                        let error_type = &details.error_type;
                        let error_message = &details.error_message;
                        eprintln!(
                            "🌐 Network details: {error_type} - {error_message}"
                        );
                    }
                    TranscriptionError::FileTooLarge(size) => {
                        eprintln!("💡 Audio file too large: {size} bytes (max 25MB)");
                    }
                    TranscriptionError::ConfigurationError(_) => {
                        eprintln!("💡 Check your transcription provider configuration");
                    }
                    TranscriptionError::UnsupportedProvider(provider) => {
                        eprintln!(
                            "💡 Unsupported provider: {provider}. Check TRANSCRIPTION_PROVIDER setting"
                        );
                    }
                    TranscriptionError::ApiError(details) => {
                        if let Some(status) = details.status_code {
                            eprintln!("📡 API Response: HTTP {status}");
                        }
                        if let Some(code) = &details.error_code {
                            eprintln!("🏷️  Error Code: {code}");
                        }
                    }
                    TranscriptionError::JsonError(_) => {
                        eprintln!("💡 Failed to parse API response");
                    }
                }
                Ok(1)
            }
        }
    }

    pub(crate) fn is_recording(&self) -> bool {
        self.recorder.is_recording()
    }

    // IPC helpers: used by the Unix socket server
    pub(crate) async fn ipc_start(&mut self) -> Result<()> {
        if let Err(e) = self.recorder.clear_buffer() { eprintln!("Buffer clear failed before start: {}", e); }
        if let Err(e) = self.beeps.play_async(BeepType::RecordingStart).await {
            eprintln!("Start beep failed: {}", e);
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(600)).await;
        self.recorder.start_recording()?;
        Ok(())
    }

    pub(crate) async fn ipc_cancel(&mut self) -> Result<()> {
        let _ = self.recorder.stop_recording();
        if let Err(e) = self.beeps.play_async(BeepType::RecordingStop).await {
            eprintln!("Stop beep failed: {}", e);
        }
        self.recorder.clear_buffer()?;
        Ok(())
    }

    pub(crate) async fn ipc_stop_and_transcribe(
        &mut self,
        options: crate::ipc::IpcOptions,
    ) -> Result<(String, u64, crate::ipc::OutputMode)> {
        let start = std::time::Instant::now();
        let _ = self.recorder.stop_recording();
        if let Err(e) = self.beeps.play_async(BeepType::RecordingStop).await {
            eprintln!("Stop beep failed: {}", e);
        }
        let audio_data = self.recorder.get_audio_data()?;
        let text = self.ipc_transcribe_text(audio_data).await?;

        match options.output {
            crate::ipc::OutputMode::Stdout => {}
            crate::ipc::OutputMode::Clipboard => {
                crate::ipc::copy_to_clipboard(&text).await?;
            }
            crate::ipc::OutputMode::Type => {
                crate::ipc::type_text(&text, options.type_newlines).await?;
            }
        }
        let _ = self.recorder.clear_buffer();
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok((text, duration_ms, options.output))
    }

    pub(crate) fn ipc_status(&self) -> crate::ipc::IpcResult {
        let state = if self.recorder.is_recording() { "Recording" } else { "Idle" };
        crate::ipc::IpcResult {
            state: state.to_string(),
            provider: format!("{:?}", self.config.provider_kind()),
            model: self.config.whisper_model.clone(),
            ..crate::ipc::IpcResult::default()
        }
    }

    async fn ipc_transcribe_text(&self, audio_data: Vec<f32>) -> Result<String> {
        let processed = self.pipeline.preprocess(&audio_data)?;
        let wav = self.pipeline.to_wav(&processed)?;
        let language_opt = {
            let s = self.config.whisper_language.trim();
            if s.is_empty() || s.eq_ignore_ascii_case("auto") { None } else { Some(s.to_string()) }
        };
        let text = self
            .pipeline
            .transcribe(wav, self.provider.as_ref(), language_opt)
            .await?;
        Ok(text)
    }

    /// Start capture and wait until trailing silence is detected, then stop.
    /// Heuristics: require at least min_ms of capture after first voice; stop after silence_ms of trailing silence;
    /// and cap total at max_ms to avoid indefinite capture.
    pub(crate) async fn ipc_capture_until_silence(
        &mut self,
        min_ms: u64,
        silence_ms: u64,
        max_ms: u64,
    ) -> Result<()> {
        use crate::audio_processing::AudioProcessor;
        let sr = self.config.audio_sample_rate;
        let window_ms: u64 = 200;
        let poll_ms: u64 = 50;
        // Start if not already
        if !self.recorder.is_recording() {
            self.ipc_start().await?;
        }

        let start = std::time::Instant::now();
        let mut first_voice_time: Option<std::time::Instant> = None;
        let mut last_voice_time: Option<std::time::Instant> = None;
        let mut peak_rms: f32 = 0.0;
        let proc = AudioProcessor::new(sr);
        let win_samples = ((sr as u64 * window_ms) / 1000) as usize;

        loop {
            // Safety cap
            if start.elapsed().as_millis() as u64 >= max_ms {
                break;
            }

            let data = match self.recorder.get_audio_data() {
                Ok(d) => d,
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
                    continue;
                }
            };

            if data.len() >= win_samples {
                let start_idx = data.len() - win_samples;
                let window = &data[start_idx..];
                let rms = proc.calculate_rms(window);
                if rms > peak_rms { peak_rms = rms; }
                let threshold = (peak_rms * 0.1).max(0.005);
                let now = std::time::Instant::now();

                if rms > threshold {
                    if first_voice_time.is_none() { first_voice_time = Some(now); }
                    last_voice_time = Some(now);
                }

                // Check stop condition: have voice, min duration satisfied, and silence for silence_ms
                if let (Some(first), Some(last)) = (first_voice_time, last_voice_time) {
                    let since_first = now.duration_since(first).as_millis() as u64;
                    let since_last = now.duration_since(last).as_millis() as u64;
                    if since_first >= min_ms && since_last >= silence_ms {
                        break;
                    }
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
        }

        // Stop recording without clearing buffer
        let _ = self.recorder.stop_recording();
        if let Err(e) = self.beeps.play_async(BeepType::RecordingStop).await { eprintln!("Stop beep failed: {}", e); }
        Ok(())
    }
}
