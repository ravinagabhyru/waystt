use std::sync::Arc;

use anyhow::Result;
use futures::stream::StreamExt;

use crate::audio::AudioRecorder;
use crate::beep::{BeepConfig, BeepPlayer, BeepType};
use crate::cli::RunOptions;
use crate::config::Config;
use crate::continuous::{ContinuousConfig, ContinuousModeController, ContinuousState};
use crate::pipeline::AudioPipeline;
use crate::refine::{self, RefineScope, TextRefiner};
use crate::signals;
use crate::transcription::{StreamingTranscriptionProvider, TranscriptionProvider};

pub struct App {
    config: Config,
    recorder: AudioRecorder,
    beeps: BeepPlayer,
    pipeline: Arc<AudioPipeline>,
    provider: Arc<dyn TranscriptionProvider>,
    /// Streaming provider, when the configured kind/model supports it
    /// (currently only Parakeet EOU). `None` for batch-only providers.
    streaming_provider: Option<Arc<dyn StreamingTranscriptionProvider>>,
    pipe_to: Option<Vec<String>>,
    /// Optional LLM post-processor applied after transcription.
    text_refiner: Option<Arc<dyn TextRefiner>>,
    /// Controller for continuous speech recognition mode
    continuous: Option<ContinuousModeController>,
}

impl App {
    /// Initialize the application
    ///
    /// # Errors
    ///
    /// Returns an error if audio devices cannot be initialized or configured
    pub async fn init(
        options: RunOptions,
        config: Config,
        provider: Box<dyn TranscriptionProvider>,
        streaming_provider: Option<Arc<dyn StreamingTranscriptionProvider>>,
        text_refiner: Option<Arc<dyn TextRefiner>>,
    ) -> Result<Self> {
        let beep_config = BeepConfig {
            enabled: config.enable_audio_feedback,
            volume: config.beep_volume,
        };
        let beeps = BeepPlayer::new(beep_config)?;
        let recorder = AudioRecorder::new()?;
        let pipeline = Arc::new(AudioPipeline::new(config.audio_sample_rate));

        // Eager model load for streaming providers so first-utterance latency
        // is paid at startup rather than on the user's first utterance.
        #[cfg(feature = "parakeet")]
        if streaming_provider.is_some()
            && config.provider_kind() == crate::transcription::ProviderKind::Parakeet
            && crate::transcription::parakeet::ParakeetModelType::parse(&config.parakeet_model_type)
                == crate::transcription::parakeet::ParakeetModelType::EOU
        {
            eprintln!("Pre-loading Parakeet EOU model...");
            let model_path = if let Some(ref custom_path) = config.parakeet_model_path {
                std::path::PathBuf::from(custom_path)
            } else {
                crate::config::Config::parakeet_model_path(&config.parakeet_model_type)
            };
            let provider_for_warmup =
                crate::transcription::parakeet_streaming::ParakeetStreamingProvider::new(
                    &model_path,
                )?;
            if let Err(e) = provider_for_warmup.warm_up() {
                eprintln!(
                    "Warning: Parakeet EOU warm-up failed: {e}. First utterance may be slower."
                );
            }
        }

        Ok(Self {
            config,
            recorder,
            beeps,
            pipeline,
            provider: Arc::from(provider),
            streaming_provider,
            pipe_to: options.pipe_to,
            text_refiner,
            continuous: None,
        })
    }

    /// Run the app in continuous mode: start capturing immediately, stream
    /// utterances to the configured output, and shut down on SIGTERM / SIGINT.
    ///
    /// # Errors
    ///
    /// Returns an error if signal registration or recording setup fails.
    pub async fn run_continuous(mut self) -> Result<i32> {
        eprintln!("waystt - Wayland Speech-to-Text Tool (continuous mode)");

        let opts = crate::ipc::IpcOptions::default();
        if let Err(e) = self.ipc_continuous_start(opts).await {
            eprintln!("Failed to start continuous mode: {e}");
            return Ok(1);
        }

        let mut signals = signals::build_signal_stream()?;
        let audio_notify = self.recorder.audio_notify();

        loop {
            if let Err(e) = self.recorder.process_audio_events() {
                eprintln!("Audio event processing error: {e}");
            }

            tokio::select! {
                maybe_sig = signals.next() => {
                    match maybe_sig {
                        Some(sig) if signals::is_shutdown_signal(sig) => {
                            eprintln!("Received shutdown signal: exiting continuous mode");
                            break;
                        }
                        Some(other) => {
                            eprintln!("Ignoring unexpected signal: {other}");
                        }
                        None => break,
                    }
                }
                // Wake immediately when CPAL delivers a new audio buffer.
                () = audio_notify.notified() => {
                    if let Err(e) = self.ipc_continuous_process().await {
                        eprintln!("Continuous processing error: {e}");
                    }
                }
            }
        }

        match self.ipc_continuous_stop().await {
            Ok(stats) => {
                eprintln!(
                    "Continuous mode stopped. Captured {} utterances ({:.1}s of audio).",
                    stats.chunks_captured, stats.total_audio_seconds
                );
            }
            Err(e) => {
                eprintln!("Failed to stop continuous mode cleanly: {e}");
                return Ok(1);
            }
        }
        Ok(0)
    }

    pub(crate) fn is_recording(&self) -> bool {
        self.recorder.is_recording()
    }

    /// Handle to the recorder's new-audio notifier. Exposed so the IPC loop
    /// can wake on CPAL callbacks instead of polling.
    pub(crate) fn audio_notify(&self) -> std::sync::Arc<tokio::sync::Notify> {
        self.recorder.audio_notify()
    }

    // IPC helpers: used by the Unix socket server
    pub(crate) async fn ipc_start(&mut self) -> Result<()> {
        if let Err(e) = self.recorder.clear_buffer() {
            eprintln!("Buffer clear failed before start: {e}");
        }
        // Start recording FIRST, then play beep so we don't miss initial speech
        self.recorder.start_recording()?;
        if let Err(e) = self.beeps.play_async(BeepType::RecordingStart).await {
            eprintln!("Start beep failed: {e}");
        }
        Ok(())
    }

    pub(crate) async fn ipc_cancel(&mut self) -> Result<()> {
        let _ = self.recorder.stop_recording();
        if let Err(e) = self.beeps.play_async(BeepType::RecordingStop).await {
            eprintln!("Stop beep failed: {e}");
        }
        self.recorder.clear_buffer()?;
        Ok(())
    }

    #[allow(clippy::cast_possible_truncation)]
    pub(crate) async fn ipc_stop_and_transcribe(
        &mut self,
        options: crate::ipc::IpcOptions,
    ) -> Result<(String, u64, crate::ipc::OutputMode)> {
        let start = std::time::Instant::now();
        let _ = self.recorder.stop_recording();
        if let Err(e) = self.beeps.play_async(BeepType::RecordingStop).await {
            eprintln!("Stop beep failed: {e}");
        }
        let audio_data = self.recorder.get_audio_data()?;
        let text = self.ipc_transcribe_text(audio_data).await?;
        // Strip leading whitespace for typing sinks: wtype + Electron drops
        // every space in a burst whose first event is a space. Keep the raw
        // text for stdout/clipboard where the bug doesn't apply.
        let typed_text = match options.output {
            crate::ipc::OutputMode::Type
            | crate::ipc::OutputMode::Wtype
            | crate::ipc::OutputMode::Ydotool => text.trim_start().to_string(),
            crate::ipc::OutputMode::Stdout | crate::ipc::OutputMode::Clipboard => text.clone(),
        };

        match options.output {
            crate::ipc::OutputMode::Stdout => {}
            crate::ipc::OutputMode::Clipboard => {
                crate::ipc::copy_to_clipboard(&typed_text).await?;
            }
            crate::ipc::OutputMode::Type => {
                crate::ipc::type_text(&typed_text, options.type_newlines).await?;
            }
            crate::ipc::OutputMode::Wtype => {
                crate::ipc::type_text_wtype(&typed_text, options.type_newlines).await?;
            }
            crate::ipc::OutputMode::Ydotool => {
                crate::ipc::type_text_ydotool(&typed_text, options.type_newlines).await?;
            }
        }
        let _ = self.recorder.clear_buffer();
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok((text, duration_ms, options.output))
    }

    pub(crate) fn ipc_status(&self) -> crate::ipc::IpcResult {
        let state = if self.recorder.is_recording() {
            "Recording"
        } else {
            "Idle"
        };
        let model = match self.config.provider_kind() {
            crate::transcription::ProviderKind::OpenAI => self.config.whisper_model.clone(),
            crate::transcription::ProviderKind::Google => {
                self.config.google_speech_model.clone()
            }
            crate::transcription::ProviderKind::Local => self.config.whisper_model.clone(),
            #[cfg(feature = "parakeet")]
            crate::transcription::ProviderKind::Parakeet => {
                format!("parakeet-{}", self.config.parakeet_model_type)
            }
        };
        crate::ipc::IpcResult {
            state: state.to_string(),
            provider: format!("{:?}", self.config.provider_kind()),
            model,
            ..crate::ipc::IpcResult::default()
        }
    }

    async fn ipc_transcribe_text(&self, audio_data: Vec<f32>) -> Result<String> {
        let processed = self.pipeline.preprocess(&audio_data)?;
        let wav = self.pipeline.to_wav(&processed)?;
        let language_opt = {
            let s = self.config.whisper_language.trim();
            if s.is_empty() || s.eq_ignore_ascii_case("auto") {
                None
            } else {
                Some(s.to_string())
            }
        };
        let text = self
            .pipeline
            .transcribe(wav, self.provider.as_ref(), language_opt)
            .await?;
        let text = if self.config.refine_enabled_for_batch() {
            refine::refine_or_fallback(
                self.text_refiner.as_ref(),
                text,
                self.config.llm_refine_min_chars,
                RefineScope::Batch,
                self.config.llm_refine_log_text,
            )
            .await
        } else {
            text
        };
        Ok(text)
    }

    /// Start capture and wait until trailing silence is detected, then stop.
    /// Heuristics: require at least `min_ms` of capture after first voice; stop after `silence_ms` of trailing silence;
    /// and cap total at `max_ms` to avoid indefinite capture.
    #[allow(clippy::cast_possible_truncation)]
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
        let win_samples = ((u64::from(sr) * window_ms) / 1000) as usize;

        loop {
            // Safety cap
            if start.elapsed().as_millis() as u64 >= max_ms {
                break;
            }

            let Ok(data) = self.recorder.get_audio_data() else {
                tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
                continue;
            };

            if data.len() >= win_samples {
                let start_idx = data.len() - win_samples;
                let window = &data[start_idx..];
                let rms = proc.calculate_rms(window);
                if rms > peak_rms {
                    peak_rms = rms;
                }
                let threshold = (peak_rms * 0.1).max(0.005);
                let now = std::time::Instant::now();

                if rms > threshold {
                    if first_voice_time.is_none() {
                        first_voice_time = Some(now);
                    }
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
        if let Err(e) = self.beeps.play_async(BeepType::RecordingStop).await {
            eprintln!("Stop beep failed: {e}");
        }
        Ok(())
    }

    /// Start continuous speech recognition mode
    ///
    /// Continuously captures audio, detects silence gaps, and transcribes
    /// chunks in the background. Results are output in capture order.
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) async fn ipc_continuous_start(
        &mut self,
        options: crate::ipc::IpcOptions,
    ) -> Result<()> {
        // Check if already in continuous mode
        if let Some(ref controller) = self.continuous {
            if controller.state() != ContinuousState::Stopped {
                return Err(anyhow::anyhow!("Continuous mode already running"));
            }
        }

        // Create continuous config: config-file values are the baseline; IPC
        // `options` overrides take precedence for the two knobs wayctl exposes.
        let config = ContinuousConfig {
            min_speech_ms: self.config.continuous_min_speech_ms,
            silence_threshold_ms: options
                .continuous_silence_ms
                .unwrap_or(self.config.continuous_silence_threshold_ms),
            max_chunk_ms: self.config.continuous_max_chunk_ms,
            worker_count: options
                .continuous_workers
                .map_or(self.config.continuous_worker_count, |w| {
                    (w as usize).clamp(1, 4)
                }),
            max_queue_size: self.config.continuous_max_queue_size,
            sample_rate: self.config.audio_sample_rate,
        };

        // Clear buffer and start recording
        if let Err(e) = self.recorder.clear_buffer() {
            eprintln!("Buffer clear failed before continuous start: {e}");
        }
        self.recorder.start_recording()?;

        // Play start beep
        if let Err(e) = self.beeps.play_async(BeepType::RecordingStart).await {
            eprintln!("Start beep failed: {e}");
        }

        // Get language setting
        let language = {
            let s = self.config.whisper_language.trim();
            if s.is_empty() || s.eq_ignore_ascii_case("auto") {
                None
            } else {
                Some(s.to_string())
            }
        };

        // Create and start controller
        let mut controller = ContinuousModeController::new(config);
        let mut output_rx = controller
            .start(
                Arc::clone(&self.pipeline),
                Arc::clone(&self.provider),
                self.streaming_provider.as_ref().map(Arc::clone),
                language,
            )
            .await?;

        self.continuous = Some(controller);

        // Spawn task to handle output
        let pipe_to = self.pipe_to.clone();
        let output_mode = options.output;
        let type_newlines = options.type_newlines;
        let refine_enabled = self.config.refine_enabled_for_continuous();
        let refine_min_chars = self.config.llm_refine_min_chars;
        let refine_log_text = self.config.llm_refine_log_text;
        let text_refiner = self.text_refiner.clone();
        tokio::spawn(async move {
            while let Some(text) = output_rx.recv().await {
                if text.is_empty() {
                    continue;
                }
                eprintln!("[Continuous] Transcribed: \"{text}\"");
                let text = if refine_enabled {
                    refine::refine_or_fallback(
                        text_refiner.as_ref(),
                        text,
                        refine_min_chars,
                        RefineScope::Continuous,
                        refine_log_text,
                    )
                    .await
                } else {
                    text
                };
                if text.is_empty() {
                    continue;
                }
                // Sinks that concatenate successive utterances (type/pipe_to)
                // need an explicit separator — without one, "Hello." followed
                // by "World." lands as "Hello.World.". Stdout uses println
                // (newline-separated) and clipboard overwrites, so neither
                // needs a separator.
                //
                // We append a trailing space rather than prepending one,
                // because wtype + Electron (Slack) drops *every* space in a
                // burst whose first event is a space — verified by typing
                // " hello world" vs "hello world" into Slack directly
                // (former lands as "helloworld", latter is correct).
                // A leading space in the payload triggers the bug; a
                // trailing space does not.
                let is_typing_sink = matches!(
                    output_mode,
                    crate::ipc::OutputMode::Type
                        | crate::ipc::OutputMode::Wtype
                        | crate::ipc::OutputMode::Ydotool
                );
                let needs_separator = pipe_to.is_some() || is_typing_sink;
                // Defensively strip leading whitespace for typing sinks so a
                // stray leading space from the transcription model can't
                // re-trigger the Electron/wtype drop.
                let text = if is_typing_sink {
                    text.trim_start().to_string()
                } else {
                    text
                };
                if text.is_empty() {
                    continue;
                }
                let needs_space_suffix =
                    needs_separator && !text.ends_with(|c: char| c.is_whitespace());
                let text = if needs_space_suffix {
                    format!("{text} ")
                } else {
                    text
                };
                if let Some(ref cmd) = pipe_to {
                    match crate::command::execute_with_input(cmd, &text).await {
                        Ok(code) => {
                            if code != 0 {
                                eprintln!("[Continuous] Pipe command exited with code {code}");
                            }
                        }
                        Err(e) => {
                            eprintln!("[Continuous] Failed to execute pipe command: {e}");
                        }
                    }
                } else {
                    // Use output mode from options
                    match output_mode {
                        crate::ipc::OutputMode::Stdout => {
                            println!("{text}");
                        }
                        crate::ipc::OutputMode::Clipboard => {
                            if let Err(e) = crate::ipc::copy_to_clipboard(&text).await {
                                eprintln!("[Continuous] Failed to copy to clipboard: {e}");
                            }
                        }
                        crate::ipc::OutputMode::Type => {
                            if let Err(e) = crate::ipc::type_text(&text, type_newlines).await {
                                eprintln!("[Continuous] Failed to type text: {e}");
                            }
                        }
                        crate::ipc::OutputMode::Wtype => {
                            if let Err(e) = crate::ipc::type_text_wtype(&text, type_newlines).await
                            {
                                eprintln!("[Continuous] Failed to type text with wtype: {e}");
                            }
                        }
                        crate::ipc::OutputMode::Ydotool => {
                            if let Err(e) =
                                crate::ipc::type_text_ydotool(&text, type_newlines).await
                            {
                                eprintln!("[Continuous] Failed to type text with ydotool: {e}");
                            }
                        }
                    }
                }
            }
        });

        eprintln!("[Continuous] Started continuous speech recognition");
        Ok(())
    }

    /// Process continuous mode audio (call this in the IPC server loop)
    pub(crate) async fn ipc_continuous_process(&mut self) -> Result<bool> {
        let Some(ref mut controller) = self.continuous else {
            return Ok(false);
        };

        if controller.state() != ContinuousState::Running {
            return Ok(false);
        }

        controller.process_audio(&self.recorder).await
    }

    /// Stop continuous speech recognition mode
    ///
    /// Waits for all pending transcriptions to complete and returns stats.
    pub(crate) async fn ipc_continuous_stop(
        &mut self,
    ) -> Result<crate::continuous::ContinuousStats> {
        let Some(ref mut controller) = self.continuous else {
            return Err(anyhow::anyhow!("Continuous mode not running"));
        };

        // Stop recording
        let _ = self.recorder.stop_recording();

        // Play stop beep
        if let Err(e) = self.beeps.play_async(BeepType::RecordingStop).await {
            eprintln!("Stop beep failed: {e}");
        }

        // Stop the controller and wait for completion
        let stats = controller.stop(&self.recorder).await?;

        // Clean up
        self.continuous = None;

        // Play success beep
        if let Err(e) = self.beeps.play_async(BeepType::Success).await {
            eprintln!("Success beep failed: {e}");
        }

        eprintln!(
            "[Continuous] Stopped. Captured {} chunks, transcribed {}, failed {}",
            stats.chunks_captured, stats.chunks_transcribed, stats.chunks_failed
        );

        Ok(stats)
    }

    /// Get status of continuous mode
    pub(crate) fn ipc_continuous_status(&self) -> Option<ContinuousState> {
        self.continuous.as_ref().map(|c| c.state())
    }
}
