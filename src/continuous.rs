//! Continuous speech recognition with background transcription queue.
//!
//! This module implements continuous speech-to-text that:
//! - Records audio continuously, detecting silence gaps to extract chunks
//! - Queues chunks for background transcription while capture continues
//! - Uses parallel workers for concurrent transcription
//! - Outputs results in capture order (preserving sequence)

#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_precision_loss)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use tokio::sync::{mpsc, Mutex, Notify};

use crate::audio::AudioRecorder;
use crate::audio_processing::AudioProcessor;
use crate::pipeline::AudioPipeline;
use crate::transcription::TranscriptionProvider;

/// Configuration for continuous speech recognition mode
#[derive(Debug, Clone)]
pub struct ContinuousConfig {
    /// Minimum speech duration before considering a chunk complete (ms)
    pub min_speech_ms: u64,
    /// Trailing silence duration to trigger chunk extraction (ms)
    pub silence_threshold_ms: u64,
    /// Maximum chunk duration before forced extraction (ms)
    pub max_chunk_ms: u64,
    /// Number of parallel transcription workers
    pub worker_count: usize,
    /// Maximum pending chunks in queue before backpressure
    pub max_queue_size: usize,
    /// Audio sample rate
    pub sample_rate: u32,
}

impl Default for ContinuousConfig {
    fn default() -> Self {
        Self {
            min_speech_ms: 300,
            silence_threshold_ms: 700,
            max_chunk_ms: 30_000,
            worker_count: 2,
            max_queue_size: 10,
            sample_rate: 16000,
        }
    }
}

/// A chunk of audio with its sequence number for ordering
#[derive(Debug)]
pub struct AudioChunk {
    /// Sequence number for ordering (monotonically increasing)
    pub seq: u64,
    /// Raw audio samples (f32, 16kHz mono)
    pub samples: Vec<f32>,
}

/// Result of transcription with sequence number for ordering
#[derive(Debug)]
pub struct TranscriptionResult {
    /// Sequence number matching the AudioChunk
    pub seq: u64,
    /// Transcribed text (Ok) or error message (Err)
    pub result: Result<String, String>,
}

/// State of the continuous mode controller
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuousState {
    /// Not running
    Stopped,
    /// Actively capturing and transcribing
    Running,
    /// Stop requested, flushing remaining chunks
    Stopping,
}

/// Statistics for monitoring continuous mode
#[derive(Debug, Clone, Default)]
pub struct ContinuousStats {
    pub chunks_captured: u64,
    pub chunks_transcribed: u64,
    pub chunks_failed: u64,
    pub total_audio_seconds: f32,
}

/// Internal state for silence detection
struct SilenceDetectionState {
    first_voice_time: Option<Instant>,
    last_voice_time: Option<Instant>,
    peak_rms: f32,
    processor: AudioProcessor,
    window_samples: usize,
}

impl SilenceDetectionState {
    fn new(sample_rate: u32) -> Self {
        let window_ms: u64 = 200;
        let window_samples = ((u64::from(sample_rate) * window_ms) / 1000) as usize;
        Self {
            first_voice_time: None,
            last_voice_time: None,
            peak_rms: 0.0,
            processor: AudioProcessor::new(sample_rate),
            window_samples,
        }
    }

    fn reset(&mut self) {
        self.first_voice_time = None;
        self.last_voice_time = None;
        self.peak_rms = 0.0;
    }
}

/// Controller for continuous speech recognition mode
pub struct ContinuousModeController {
    /// Current state
    state: ContinuousState,
    /// Configuration
    config: ContinuousConfig,
    /// Next sequence number for chunks
    next_seq: u64,
    /// Channel for sending chunks to workers
    chunk_tx: Option<mpsc::Sender<AudioChunk>>,
    /// Results buffer keyed by sequence number
    results: Arc<Mutex<BTreeMap<u64, TranscriptionResult>>>,
    /// Next sequence number to output (for ordered output)
    next_output_seq: Arc<Mutex<u64>>,
    /// Notification when new results are ready
    result_notify: Arc<Notify>,
    /// Statistics
    stats: ContinuousStats,
    /// Worker task handles for cleanup
    worker_handles: Vec<tokio::task::JoinHandle<()>>,
    /// Output ordering task handle
    output_handle: Option<tokio::task::JoinHandle<()>>,
    /// Output channel sender (kept to prevent channel closing)
    _output_tx: Option<mpsc::Sender<String>>,
    /// Silence detection state
    silence_state: SilenceDetectionState,
}

impl ContinuousModeController {
    /// Create a new controller with configuration
    #[must_use]
    pub fn new(config: ContinuousConfig) -> Self {
        let silence_state = SilenceDetectionState::new(config.sample_rate);
        Self {
            state: ContinuousState::Stopped,
            config,
            next_seq: 0,
            chunk_tx: None,
            results: Arc::new(Mutex::new(BTreeMap::new())),
            next_output_seq: Arc::new(Mutex::new(0)),
            result_notify: Arc::new(Notify::new()),
            stats: ContinuousStats::default(),
            worker_handles: Vec::new(),
            output_handle: None,
            _output_tx: None,
            silence_state,
        }
    }

    /// Start continuous mode with the given provider and pipeline
    /// Returns a receiver for transcription results (in order)
    pub async fn start(
        &mut self,
        pipeline: Arc<AudioPipeline>,
        provider: Arc<dyn TranscriptionProvider>,
        language: Option<String>,
    ) -> Result<mpsc::Receiver<String>> {
        // Check if already running
        if self.state != ContinuousState::Stopped {
            return Err(anyhow!("Continuous mode already running"));
        }

        // Reset state
        self.next_seq = 0;
        *self.next_output_seq.lock().await = 0;
        self.results.lock().await.clear();
        self.stats = ContinuousStats::default();
        self.silence_state.reset();

        // Create chunk queue channel
        let (chunk_tx, chunk_rx) = mpsc::channel::<AudioChunk>(self.config.max_queue_size);
        self.chunk_tx = Some(chunk_tx);

        // Create output channel
        let (output_tx, output_rx) = mpsc::channel::<String>(100);
        self._output_tx = Some(output_tx.clone());

        // Spawn worker pool
        let chunk_rx = Arc::new(Mutex::new(chunk_rx));
        for worker_id in 0..self.config.worker_count {
            let rx = Arc::clone(&chunk_rx);
            let results = Arc::clone(&self.results);
            let notify = Arc::clone(&self.result_notify);
            let pipeline = Arc::clone(&pipeline);
            let provider = Arc::clone(&provider);
            let lang = language.clone();

            let handle = tokio::spawn(async move {
                transcription_worker(worker_id, rx, results, notify, pipeline, provider, lang)
                    .await;
            });
            self.worker_handles.push(handle);
        }

        // Spawn output ordering task
        {
            let results = Arc::clone(&self.results);
            let next_output_seq = Arc::clone(&self.next_output_seq);
            let notify = Arc::clone(&self.result_notify);
            let tx = output_tx;

            let handle = tokio::spawn(async move {
                output_ordered_results(results, next_output_seq, notify, tx).await;
            });
            self.output_handle = Some(handle);
        }

        // Set state to running
        self.state = ContinuousState::Running;

        Ok(output_rx)
    }

    /// Process one iteration of silence detection.
    /// Call this repeatedly in a loop while in continuous mode.
    /// Returns true if a chunk was extracted and queued.
    pub async fn process_audio(&mut self, recorder: &AudioRecorder) -> Result<bool> {
        if self.state != ContinuousState::Running {
            return Ok(false);
        }

        // Get current buffer state
        let buffer_len = recorder.buffer_len().unwrap_or(0);

        // Need enough samples for analysis
        if buffer_len < self.silence_state.window_samples {
            return Ok(false);
        }

        // Get the tail window for analysis
        let window = recorder
            .peek_tail(self.silence_state.window_samples)
            .unwrap_or_default();
        if window.is_empty() {
            return Ok(false);
        }

        // Analyze the latest window
        let rms = self.silence_state.processor.calculate_rms(&window);
        self.silence_state.peak_rms = self.silence_state.peak_rms.max(rms);
        let threshold = (self.silence_state.peak_rms * 0.1).max(0.005);
        let now = Instant::now();

        // Voice activity detection
        if rms > threshold {
            if self.silence_state.first_voice_time.is_none() {
                self.silence_state.first_voice_time = Some(now);
            }
            self.silence_state.last_voice_time = Some(now);
        }

        // Check chunk extraction conditions
        let should_extract = if let (Some(first), Some(last)) = (
            self.silence_state.first_voice_time,
            self.silence_state.last_voice_time,
        ) {
            let since_first = now.duration_since(first).as_millis() as u64;
            let since_last = now.duration_since(last).as_millis() as u64;

            // Condition 1: Sufficient speech followed by silence
            let silence_triggered = since_first >= self.config.min_speech_ms
                && since_last >= self.config.silence_threshold_ms;

            // Condition 2: Max chunk duration reached
            let max_duration_reached = since_first >= self.config.max_chunk_ms;

            silence_triggered || max_duration_reached
        } else {
            false
        };

        if should_extract {
            // Extract all samples from buffer
            let samples = recorder.drain_samples(buffer_len).unwrap_or_default();

            if !samples.is_empty() {
                // Get next sequence number
                let seq = self.next_seq;
                self.next_seq += 1;

                // Update stats
                self.stats.chunks_captured += 1;
                self.stats.total_audio_seconds +=
                    samples.len() as f32 / self.config.sample_rate as f32;

                let chunk = AudioChunk { seq, samples };

                // Queue the chunk
                if let Some(ref tx) = self.chunk_tx {
                    if tx.send(chunk).await.is_err() {
                        // Channel closed
                        self.state = ContinuousState::Stopping;
                        return Err(anyhow!("Worker queue closed"));
                    }
                }

                eprintln!(
                    "[Continuous] Extracted chunk seq={seq}, {} samples",
                    buffer_len
                );
            }

            // Reset for next chunk
            self.silence_state.reset();

            return Ok(true);
        }

        Ok(false)
    }

    /// Request stop and wait for all pending chunks to complete
    pub async fn stop(&mut self, recorder: &AudioRecorder) -> Result<ContinuousStats> {
        if self.state == ContinuousState::Stopped {
            return Ok(self.stats.clone());
        }

        self.state = ContinuousState::Stopping;

        // Extract any remaining audio as final chunk
        let buffer_len = recorder.buffer_len().unwrap_or(0);
        if buffer_len > 0 {
            let samples = recorder.drain_samples(buffer_len).unwrap_or_default();
            if !samples.is_empty() {
                let seq = self.next_seq;
                self.next_seq += 1;

                self.stats.chunks_captured += 1;
                self.stats.total_audio_seconds +=
                    samples.len() as f32 / self.config.sample_rate as f32;

                let chunk = AudioChunk { seq, samples };
                if let Some(ref tx) = self.chunk_tx {
                    let _ = tx.send(chunk).await;
                }
                eprintln!("[Continuous] Extracted final chunk seq={seq}");
            }
        }

        // Close chunk channel to signal workers to drain and exit
        self.chunk_tx.take();

        // Wait for all workers to complete
        for handle in self.worker_handles.drain(..) {
            let _ = handle.await;
        }

        // Update stats from results
        {
            let results = self.results.lock().await;
            for result in results.values() {
                match &result.result {
                    Ok(_) => self.stats.chunks_transcribed += 1,
                    Err(_) => self.stats.chunks_failed += 1,
                }
            }
        }

        // Notify output task in case it's waiting
        self.result_notify.notify_waiters();

        // Give output task time to flush (small delay)
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Cancel output task if still running
        if let Some(handle) = self.output_handle.take() {
            handle.abort();
        }

        // Close output channel
        self._output_tx.take();

        // Set state to stopped
        self.state = ContinuousState::Stopped;

        Ok(self.stats.clone())
    }

    /// Get current state
    #[must_use]
    pub fn state(&self) -> ContinuousState {
        self.state
    }

    /// Get current statistics
    #[must_use]
    pub fn stats(&self) -> &ContinuousStats {
        &self.stats
    }
}

/// Worker task that pulls chunks from queue and transcribes them
async fn transcription_worker(
    worker_id: usize,
    chunk_rx: Arc<Mutex<mpsc::Receiver<AudioChunk>>>,
    results: Arc<Mutex<BTreeMap<u64, TranscriptionResult>>>,
    result_notify: Arc<Notify>,
    pipeline: Arc<AudioPipeline>,
    provider: Arc<dyn TranscriptionProvider>,
    language: Option<String>,
) {
    eprintln!("[Worker {worker_id}] Started");

    loop {
        // Try to receive a chunk
        let chunk = {
            let mut rx = chunk_rx.lock().await;
            rx.recv().await
        };

        let Some(chunk) = chunk else {
            // Channel closed, exit
            break;
        };

        let seq = chunk.seq;
        eprintln!(
            "[Worker {worker_id}] Processing chunk seq={seq}, {} samples",
            chunk.samples.len()
        );

        // Process the chunk
        let result = process_chunk(&chunk, &pipeline, &provider, language.clone()).await;

        // Store result
        {
            let mut results_guard = results.lock().await;
            let transcription_result = TranscriptionResult {
                seq,
                result: result.map_err(|e| e.to_string()),
            };
            results_guard.insert(seq, transcription_result);
        }

        // Notify that a result is ready
        result_notify.notify_waiters();

        eprintln!("[Worker {worker_id}] Completed chunk seq={seq}");
    }

    eprintln!("[Worker {worker_id}] Shutting down");
}

/// Process a single audio chunk through the pipeline
async fn process_chunk(
    chunk: &AudioChunk,
    pipeline: &AudioPipeline,
    provider: &Arc<dyn TranscriptionProvider>,
    language: Option<String>,
) -> Result<String> {
    // Preprocess
    let processed = pipeline.preprocess(&chunk.samples)?;

    // Encode to WAV
    let wav = pipeline.to_wav(&processed)?;

    // Transcribe
    let text = provider
        .transcribe_with_language(wav, language)
        .await
        .map_err(|e| anyhow!("{}", e))?;

    Ok(text)
}

/// Task that outputs results in sequence order
async fn output_ordered_results(
    results: Arc<Mutex<BTreeMap<u64, TranscriptionResult>>>,
    next_output_seq: Arc<Mutex<u64>>,
    result_notify: Arc<Notify>,
    output_tx: mpsc::Sender<String>,
) {
    loop {
        // Wait for notification of new results
        result_notify.notified().await;

        // Output all ready results in sequence order
        loop {
            // Check what sequence we need next
            let needed_seq = *next_output_seq.lock().await;

            // Try to get and output the next result in sequence
            let result_opt = {
                let mut results_guard = results.lock().await;
                results_guard.remove(&needed_seq)
            };

            if let Some(result) = result_opt {
                match result.result {
                    Ok(text) => {
                        if !text.is_empty() && output_tx.send(text).await.is_err() {
                            // Output channel closed
                            return;
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "[Continuous] Transcription error for seq={}: {e}",
                            result.seq
                        );
                    }
                }

                // Advance to next sequence
                *next_output_seq.lock().await += 1;
            } else {
                // Result not ready yet, wait for next notification
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_continuous_config_default() {
        let config = ContinuousConfig::default();
        assert_eq!(config.min_speech_ms, 300);
        assert_eq!(config.silence_threshold_ms, 700);
        assert_eq!(config.max_chunk_ms, 30_000);
        assert_eq!(config.worker_count, 2);
        assert_eq!(config.max_queue_size, 10);
        assert_eq!(config.sample_rate, 16000);
    }

    #[test]
    fn test_continuous_state_initial() {
        let controller = ContinuousModeController::new(ContinuousConfig::default());
        assert_eq!(controller.state(), ContinuousState::Stopped);
        assert!(controller.chunk_tx.is_none());
    }

    #[test]
    fn test_audio_chunk_creation() {
        let chunk = AudioChunk {
            seq: 42,
            samples: vec![0.1, 0.2, 0.3],
        };
        assert_eq!(chunk.seq, 42);
        assert_eq!(chunk.samples.len(), 3);
    }

    #[test]
    fn test_transcription_result_ok() {
        let result = TranscriptionResult {
            seq: 1,
            result: Ok("Hello world".to_string()),
        };
        assert_eq!(result.seq, 1);
        assert!(result.result.is_ok());
    }

    #[test]
    fn test_transcription_result_err() {
        let result = TranscriptionResult {
            seq: 2,
            result: Err("Network error".to_string()),
        };
        assert_eq!(result.seq, 2);
        assert!(result.result.is_err());
    }

    #[test]
    fn test_continuous_stats_default() {
        let stats = ContinuousStats::default();
        assert_eq!(stats.chunks_captured, 0);
        assert_eq!(stats.chunks_transcribed, 0);
        assert_eq!(stats.chunks_failed, 0);
        assert!((stats.total_audio_seconds - 0.0).abs() < f32::EPSILON);
    }
}
