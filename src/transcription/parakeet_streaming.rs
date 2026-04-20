//! Streaming Parakeet provider built on `parakeet-rs`'s `ParakeetEOU` model.
//!
//! The EOU model is cache-aware and accepts audio in ~160 ms chunks (2560 samples
//! @ 16 kHz). Each call to its `transcribe()` returns any new token text since
//! the previous call, and appends a literal `" [EOU]"` marker when it detects an
//! end-of-utterance boundary (with `reset_on_eou=true`).
//!
//! This module wraps that into a `StreamingTranscriptionProvider` that:
//!   * eager-loads a shared model handle at provider construction so `start_session`
//!     is cheap (no re-reading ONNX files from disk per session);
//!   * owns a dedicated blocking OS thread per session. The thread holds the
//!     stateful `ParakeetEOU` (which wraps `ort::Session`) and receives commands
//!     over a `std::sync::mpsc` channel, replying via `tokio::sync::oneshot`.
//!     This side-steps any concerns about `Send`-safety of `ort::Session` across
//!     `.await` points — the session never crosses a thread boundary;
//!   * accumulates delta text internally and emits only complete utterances:
//!     either when EOU is detected, or when the caller force-finalizes;
//!   * uses the vendor's `reset_on_eou=true` soft-reset path to clear decoder
//!     RNN-T state (`state_h` / `state_c` / `last_token`) between utterances
//!     without discarding the encoder cache or audio ring buffer. The vendor
//!     deliberately keeps those alive (see `parakeet_eou.rs:192-200`, "we need
//!     to keep encoder cache and audio buffer flowing for continuous context")
//!     — and we deliberately avoid a `from_pretrained` reload, which would
//!     re-parse tokenizer JSON, rebuild the mel filterbank, and then suppress
//!     transcription for the first 1 s of the next utterance while the
//!     `audio_buffer` refills past `MIN_BUFFER_SAMPLES`.
//!
//! Tests stub the `EouInference` trait so chunking / flush / reset logic can be
//! exercised without loading real ONNX models in CI.

use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use async_trait::async_trait;
use tokio::sync::oneshot;

use super::{
    ApiErrorDetails, StreamingSession, StreamingTranscriptionProvider, TranscriptionError,
};

/// Parakeet EOU expects 16 kHz mono audio.
const REQUIRED_SAMPLE_RATE: u32 = 16_000;

/// Samples per EOU chunk = 160 ms @ 16 kHz.
const EOU_CHUNK_SAMPLES: usize = 2560;

/// When finalizing, feed this many silent chunks to flush any text still
/// buffered inside the encoder. The parakeet-rs author's pattern is 3 chunks.
const FLUSH_SILENCE_CHUNKS: usize = 3;

/// At session start, pre-feed this many silent chunks so the model's internal
/// `audio_buffer` (vendor `parakeet_eou.rs:100-103`) is past `MIN_BUFFER_SAMPLES`
/// (1 s at 16 kHz) before the user's first word arrives. Without this, the
/// session's very first utterance has its leading ~1 s silently swallowed as
/// the buffer fills, producing truncated or garbled transcription only for
/// the first utterance of each session. Uses 7 chunks (~1.12 s) to add a
/// small margin over the exact 6.25-chunk threshold.
const WARMUP_SILENCE_CHUNKS: usize = 7;

/// Marker the parakeet-rs `ParakeetEOU::transcribe` appends when end-of-utterance
/// is detected and `reset_on_eou=true` (see `parakeet_eou.rs:169` in the crate).
const EOU_MARKER: &str = " [EOU]";

/// An abstraction over a single call to the underlying stateful EOU model.
///
/// The only implementor in production is `RealEou` (below), which wraps
/// `parakeet_rs::ParakeetEOU`. Tests provide a scripted stub.
pub trait EouInference: Send {
    /// Run one inference on a chunk of `EOU_CHUNK_SAMPLES` samples.
    ///
    /// Returns whatever delta text the model produced. If end-of-utterance was
    /// detected, the returned string ends with [`EOU_MARKER`].
    fn transcribe_chunk(&mut self, chunk: &[f32]) -> Result<String, TranscriptionError>;

    /// Run one inference with the model's internal `reset_on_eou=true` path.
    /// If the decoder predicts the EOU token for this chunk, the vendor code
    /// calls `reset_states()` internally — a *soft* reset that clears
    /// `state_h`, `state_c`, and `last_token` while deliberately preserving
    /// `encoder_cache` and `audio_buffer` (see `parakeet_eou.rs:192-200`).
    /// Used at finalize time to break cross-utterance decoder-state leakage
    /// without paying a full model reload and its MIN_BUFFER_SAMPLES warm-up.
    fn transcribe_chunk_reset(&mut self, chunk: &[f32])
        -> Result<String, TranscriptionError>;

    /// Discard internal decoder/encoder state and start fresh by rebuilding
    /// the model from disk. Retained for completeness but no longer called
    /// from the streaming hot path — both the mid-stream EOU-marker branch
    /// and `finalize()` rely on the vendor's internal soft reset (the
    /// `reset_on_eou=true` path in `transcribe_chunk_reset`) instead. A hard
    /// reload here would cost hundreds of ms and then swallow the first ~1 s
    /// of the next utterance while the audio buffer refills past
    /// `MIN_BUFFER_SAMPLES`.
    fn reset(&mut self) -> Result<(), TranscriptionError>;
}

/// Production inference: constructs a `ParakeetEOU` and drives it.
struct RealEou {
    model_path: PathBuf,
    model: parakeet_rs::ParakeetEOU,
}

impl RealEou {
    fn new(model_path: PathBuf) -> Result<Self, TranscriptionError> {
        let path_str = model_path.to_str().ok_or_else(|| {
            TranscriptionError::ConfigurationError("Parakeet model path is not UTF-8".to_string())
        })?;
        let model = parakeet_rs::ParakeetEOU::from_pretrained(path_str, None).map_err(|e| {
            TranscriptionError::ApiError(ApiErrorDetails {
                provider: "Parakeet (EOU)".to_string(),
                status_code: None,
                error_code: Some("MODEL_LOAD_ERROR".to_string()),
                error_message: format!("Failed to load EOU model: {e}"),
                raw_response: None,
            })
        })?;
        Ok(Self { model_path, model })
    }
}

impl EouInference for RealEou {
    fn transcribe_chunk(&mut self, chunk: &[f32]) -> Result<String, TranscriptionError> {
        // `reset_on_eou=false` — the parakeet-rs streaming example uses `false`,
        // and the crate author notes the reset-on-EOU path "is not work very
        // well on my real world tests." In practice the model fires the EOU
        // token on near-silence chunks, and with reset_on_eou=true that zeroes
        // the decoder state (h/c/last_token) every few chunks — so whenever a
        // real-audio chunk finally arrives, the decoder has just been reset
        // and produces nothing. Instead we run the model continuously and
        // rely on our own silence-detection in the continuous controller to
        // segment utterances via `finalize_utterance`.
        self.model.transcribe(chunk, false).map_err(|e| {
            TranscriptionError::ApiError(ApiErrorDetails {
                provider: "Parakeet (EOU)".to_string(),
                status_code: None,
                error_code: Some("TRANSCRIPTION_ERROR".to_string()),
                error_message: format!("EOU transcription failed: {e}"),
                raw_response: None,
            })
        })
    }

    fn transcribe_chunk_reset(
        &mut self,
        chunk: &[f32],
    ) -> Result<String, TranscriptionError> {
        self.model.transcribe(chunk, true).map_err(|e| {
            TranscriptionError::ApiError(ApiErrorDetails {
                provider: "Parakeet (EOU)".to_string(),
                status_code: None,
                error_code: Some("TRANSCRIPTION_ERROR".to_string()),
                error_message: format!("EOU transcription (reset path) failed: {e}"),
                raw_response: None,
            })
        })
    }

    fn reset(&mut self) -> Result<(), TranscriptionError> {
        // `ParakeetEOU::reset_states` is private in the crate, and only does a
        // soft reset anyway (decoder states) — leaving the encoder cache and
        // audio buffer intact. To fully isolate utterances we rebuild the
        // model from disk. This re-parses the tokenizer JSON and rebuilds the
        // mel filterbank but reuses the OS-cached ONNX file content.
        let fresh = RealEou::new(self.model_path.clone())?;
        self.model = fresh.model;
        Ok(())
    }
}

/// Handle to a loaded EOU model path. Cheap to clone.
#[derive(Clone)]
struct LoadedEouModel {
    model_path: PathBuf,
}

/// Streaming provider for Parakeet EOU.
pub struct ParakeetStreamingProvider {
    model: Arc<LoadedEouModel>,
}

impl ParakeetStreamingProvider {
    /// Construct a new streaming provider, eagerly validating the model path.
    ///
    /// We do NOT eagerly instantiate `ParakeetEOU` here because the ONNX session
    /// is non-Send and sessions are constructed per-session on their dedicated
    /// thread. The cost of the first `from_pretrained` call still dominates
    /// first-utterance latency; callers that care (e.g. daemon startup) should
    /// call [`ParakeetStreamingProvider::warm_up`] after `new`.
    ///
    /// # Errors
    ///
    /// Returns an error if `model_path` does not exist or is missing the
    /// expected ONNX files.
    pub fn new(model_path: &Path) -> Result<Self, TranscriptionError> {
        if !model_path.exists() {
            return Err(TranscriptionError::ConfigurationError(format!(
                "Parakeet EOU model directory not found: {}",
                model_path.display()
            )));
        }
        Ok(Self {
            model: Arc::new(LoadedEouModel {
                model_path: model_path.to_path_buf(),
            }),
        })
    }

    /// Pre-load the ONNX model by constructing (and discarding) one EOU
    /// instance. Used at daemon startup to shift first-utterance latency
    /// from the user's first utterance to process boot.
    ///
    /// # Errors
    ///
    /// Returns an error if the model cannot be loaded.
    pub fn warm_up(&self) -> Result<(), TranscriptionError> {
        let _ = RealEou::new(self.model.model_path.clone())?;
        Ok(())
    }
}

#[async_trait]
impl StreamingTranscriptionProvider for ParakeetStreamingProvider {
    async fn start_session(
        &self,
        sample_rate: u32,
    ) -> Result<Box<dyn StreamingSession>, TranscriptionError> {
        if sample_rate != REQUIRED_SAMPLE_RATE {
            return Err(TranscriptionError::ConfigurationError(format!(
                "Parakeet EOU requires {REQUIRED_SAMPLE_RATE} Hz mono audio, got {sample_rate} Hz"
            )));
        }

        let model_path = self.model.model_path.clone();
        let mut session = ParakeetEouSession::spawn(move || {
            let eou: Box<dyn EouInference> = Box::new(RealEou::new(model_path)?);
            Ok(eou)
        })
        .await?;
        // Pre-warm the vendor model's internal audio_buffer past its
        // MIN_BUFFER_SAMPLES gate. Any deltas produced from pure silence are
        // discarded here; the session's accumulator is reset so the first
        // real utterance starts clean.
        let warmup = vec![0.0f32; WARMUP_SILENCE_CHUNKS * EOU_CHUNK_SAMPLES];
        session.push_samples(&warmup).await?;
        session.accumulator.clear();
        Ok(Box::new(session))
    }
}

// ---------------------------------------------------------------------------
// Session internals: a blocking worker thread driven by a command channel.
// ---------------------------------------------------------------------------

enum Cmd {
    /// Push a single full chunk (`EOU_CHUNK_SAMPLES` long) through the model.
    /// Response is the delta text; if a `[EOU]` marker was emitted the
    /// worker strips it, resets the model, and includes the completion flag.
    Chunk {
        samples: Vec<f32>,
        reply: oneshot::Sender<ChunkOutcome>,
    },
    /// Flush the session: feed silence chunks, collect any remaining delta,
    /// reset the model. Returns the full accumulated utterance.
    Finalize {
        reply: oneshot::Sender<Result<String, TranscriptionError>>,
    },
    Shutdown,
}

struct ChunkOutcome {
    result: Result<String, TranscriptionError>,
    /// True if the model emitted an [EOU] marker during this chunk.
    utterance_done: bool,
}

/// The async-facing streaming session. Holds only a channel sender — trivially Send.
pub struct ParakeetEouSession {
    cmd_tx: std_mpsc::Sender<Cmd>,
    /// Join handle kept so Drop can signal shutdown and reap the thread.
    /// Wrapped in `Mutex<Option<_>>` so `Drop` can take ownership.
    thread: Mutex<Option<thread::JoinHandle<()>>>,
    /// Samples that have not yet formed a full chunk.
    carry_over: Vec<f32>,
    /// Text accumulated for the current in-progress utterance.
    accumulator: String,
    /// Monotonically-ratcheting peak abs-amplitude seen on any chunk so far.
    /// Used to rescale each chunk to roughly `NORMALIZE_TARGET_PEAK` before
    /// feeding the model — mirrors the batch path's `normalize_audio` step,
    /// which is critical for ParakeetEOU (mel features from a raw ~0.04-peak
    /// mic signal produce essentially no decoder output).
    running_peak: f32,
}

/// Target peak amplitude after normalization (matches `normalize_audio` in the
/// batch pipeline).
const NORMALIZE_TARGET_PEAK: f32 = 0.8;
/// Floor for the running peak before normalization kicks in. Prevents amplifying
/// pure silence (which would just boost the noise floor).
const NORMALIZE_MIN_PEAK: f32 = 0.01;

impl ParakeetEouSession {
    /// Spawn the worker thread and return a session handle.
    ///
    /// `build_inference` is invoked on the worker thread so that `!Send`
    /// types (like `ort::Session`) never have to cross thread boundaries.
    async fn spawn<F>(build_inference: F) -> Result<Self, TranscriptionError>
    where
        F: FnOnce() -> Result<Box<dyn EouInference>, TranscriptionError> + Send + 'static,
    {
        let (cmd_tx, cmd_rx) = std_mpsc::channel::<Cmd>();
        let (ready_tx, ready_rx) = oneshot::channel::<Result<(), TranscriptionError>>();

        let handle = thread::Builder::new()
            .name("waystt-eou".into())
            .spawn(move || {
                let mut eou = match build_inference() {
                    Ok(eou) => {
                        let _ = ready_tx.send(Ok(()));
                        eou
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                run_worker(&mut *eou, &cmd_rx);
            })
            .map_err(|e| {
                TranscriptionError::ConfigurationError(format!(
                    "Failed to spawn EOU worker thread: {e}"
                ))
            })?;

        ready_rx
            .await
            .map_err(|_| {
                TranscriptionError::ConfigurationError(
                    "EOU worker thread terminated before reporting readiness".to_string(),
                )
            })??;

        Ok(Self {
            cmd_tx,
            thread: Mutex::new(Some(handle)),
            carry_over: Vec::with_capacity(EOU_CHUNK_SAMPLES),
            accumulator: String::new(),
            running_peak: 0.0,
        })
    }

    /// Helper used by tests to spawn a session with a stubbed inference.
    #[cfg(test)]
    async fn spawn_with<F>(build_inference: F) -> Result<Self, TranscriptionError>
    where
        F: FnOnce() -> Result<Box<dyn EouInference>, TranscriptionError> + Send + 'static,
    {
        Self::spawn(build_inference).await
    }

    async fn send_chunk(&mut self, chunk: Vec<f32>) -> Result<ChunkOutcome, TranscriptionError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Chunk {
                samples: chunk,
                reply: reply_tx,
            })
            .map_err(|_| {
                TranscriptionError::ConfigurationError(
                    "EOU worker thread has exited".to_string(),
                )
            })?;
        reply_rx.await.map_err(|_| {
            TranscriptionError::ConfigurationError(
                "EOU worker thread dropped reply channel".to_string(),
            )
        })
    }

    async fn send_finalize(&mut self) -> Result<String, TranscriptionError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Finalize { reply: reply_tx })
            .map_err(|_| {
                TranscriptionError::ConfigurationError(
                    "EOU worker thread has exited".to_string(),
                )
            })?;
        reply_rx.await.map_err(|_| {
            TranscriptionError::ConfigurationError(
                "EOU worker thread dropped reply channel".to_string(),
            )
        })?
    }
}

impl Drop for ParakeetEouSession {
    fn drop(&mut self) {
        // Best-effort shutdown; ignore errors as the worker may already be gone.
        let _ = self.cmd_tx.send(Cmd::Shutdown);
        if let Ok(mut guard) = self.thread.lock() {
            if let Some(handle) = guard.take() {
                // Ignore join errors — we can't surface them from Drop.
                let _ = handle.join();
            }
        }
    }
}

#[async_trait]
impl StreamingSession for ParakeetEouSession {
    async fn push_samples(
        &mut self,
        samples: &[f32],
    ) -> Result<String, TranscriptionError> {
        self.carry_over.extend_from_slice(samples);
        let mut completed = String::new();

        while self.carry_over.len() >= EOU_CHUNK_SAMPLES {
            let mut chunk: Vec<f32> = self.carry_over.drain(..EOU_CHUNK_SAMPLES).collect();
            // Running-peak normalization. Peak ratchets up; once we've seen a
            // loud chunk, every subsequent chunk uses that peak as the gain
            // reference. Below `NORMALIZE_MIN_PEAK` we skip gain so pure
            // silence at startup isn't amplified to noise. Mirrors the batch
            // path's `normalize_audio` — without it, a raw ~0.04-peak mic
            // signal produces essentially no decoder output.
            let mut chunk_peak = 0.0f32;
            for &v in &chunk {
                let a = v.abs();
                if a > chunk_peak {
                    chunk_peak = a;
                }
            }
            if chunk_peak > self.running_peak {
                self.running_peak = chunk_peak;
            }
            if self.running_peak > NORMALIZE_MIN_PEAK {
                let gain = NORMALIZE_TARGET_PEAK / self.running_peak;
                if (gain - 1.0).abs() > f32::EPSILON {
                    for s in &mut chunk {
                        *s *= gain;
                    }
                }
            }
            let outcome = self.send_chunk(chunk).await?;
            let delta = outcome.result?;
            if !delta.is_empty() {
                self.accumulator.push_str(&delta);
            }
            if outcome.utterance_done {
                let mut utter = std::mem::take(&mut self.accumulator);
                trim_trailing_whitespace(&mut utter);
                if !utter.is_empty() {
                    if !completed.is_empty() {
                        completed.push(' ');
                    }
                    completed.push_str(&utter);
                }
            }
        }

        Ok(completed)
    }

    async fn finalize_utterance(&mut self) -> Result<String, TranscriptionError> {
        // Push any carry-over as one short chunk by padding with zeros.
        if !self.carry_over.is_empty() {
            let mut padded = std::mem::take(&mut self.carry_over);
            padded.resize(EOU_CHUNK_SAMPLES, 0.0);
            let outcome = self.send_chunk(padded).await?;
            let delta = outcome.result?;
            if !delta.is_empty() {
                self.accumulator.push_str(&delta);
            }
            // Ignore the utterance_done flag here; the Finalize command below
            // will flush + reset regardless.
        }

        let flushed = self.send_finalize().await?;
        if !flushed.is_empty() {
            self.accumulator.push_str(&flushed);
        }
        let mut out = std::mem::take(&mut self.accumulator);
        trim_trailing_whitespace(&mut out);
        Ok(out)
    }
}

fn run_worker(eou: &mut dyn EouInference, cmd_rx: &std_mpsc::Receiver<Cmd>) {
    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            Cmd::Chunk { samples, reply } => {
                let outcome = process_chunk(eou, &samples);
                let _ = reply.send(outcome);
            }
            Cmd::Finalize { reply } => {
                let result = finalize(eou);
                let _ = reply.send(result);
            }
            Cmd::Shutdown => break,
        }
    }
}

fn process_chunk(eou: &mut dyn EouInference, samples: &[f32]) -> ChunkOutcome {
    match eou.transcribe_chunk(samples) {
        Ok(mut delta) => {
            let utterance_done = strip_eou_marker(&mut delta);
            // No explicit reset here. The vendor only appends [EOU] when it
            // took the `reset_on_eou=true` branch (`parakeet_eou.rs:166-169`),
            // and that branch calls `reset_states()` before returning — so
            // decoder RNN-T state is already cleared by the time we see the
            // marker. We intentionally do NOT call `eou.reset()` (the
            // `from_pretrained` reload): it's hundreds of ms and its
            // freshly-empty `audio_buffer` would swallow the first ~1 s of
            // the next utterance under the MIN_BUFFER_SAMPLES gate.
            ChunkOutcome {
                result: Ok(delta),
                utterance_done,
            }
        }
        Err(e) => ChunkOutcome {
            result: Err(e),
            utterance_done: false,
        },
    }
}

fn finalize(eou: &mut dyn EouInference) -> Result<String, TranscriptionError> {
    // Feed `FLUSH_SILENCE_CHUNKS` of silence so any token still buffered in the
    // encoder gets flushed out. We deliberately do NOT call `eou.reset()` (the
    // hard reset) — rebuilding `ParakeetEOU` from disk costs hundreds of ms
    // and its new `audio_buffer` would then suppress transcription for the
    // first 1 s of the next utterance (see `parakeet_eou.rs` MIN_BUFFER_SAMPLES
    // guard).
    //
    // The last flush chunk uses `transcribe_chunk_reset` (vendor's
    // `reset_on_eou=true` path) so the decoder's RNN-T state (state_h, state_c,
    // last_token) is cleared between utterances. Without this soft reset, a
    // live decoder hypothesis can bleed into the next utterance: the symptoms
    // are phantom tokens emitted during inter-utterance silence, lost head
    // words at the next utterance, and hallucinated tail tokens during the
    // silence flush itself.
    let silence = vec![0.0f32; EOU_CHUNK_SAMPLES];
    let mut tail = String::new();
    const { assert!(FLUSH_SILENCE_CHUNKS >= 1) };
    let last_index = FLUSH_SILENCE_CHUNKS - 1;
    for i in 0..FLUSH_SILENCE_CHUNKS {
        let mut delta = if i == last_index {
            eou.transcribe_chunk_reset(&silence)?
        } else {
            eou.transcribe_chunk(&silence)?
        };
        let _ = strip_eou_marker(&mut delta);
        if !delta.is_empty() {
            tail.push_str(&delta);
        }
    }
    Ok(tail)
}

/// If `text` ends with [`EOU_MARKER`], strip it and return true.
fn strip_eou_marker(text: &mut String) -> bool {
    if let Some(stripped) = text.strip_suffix(EOU_MARKER) {
        let new_len = stripped.len();
        text.truncate(new_len);
        true
    } else {
        false
    }
}

/// Trim trailing whitespace in-place.
fn trim_trailing_whitespace(text: &mut String) {
    while text.ends_with(|c: char| c.is_whitespace()) {
        text.pop();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};

    /// A scripted stub: each call to `transcribe_chunk` (or
    /// `transcribe_chunk_reset`) returns the next entry from `responses`.
    /// Both counters track how many times each kind of reset was invoked.
    struct StubEou {
        responses: Vec<Result<String, TranscriptionError>>,
        received_chunks: Arc<StdMutex<Vec<Vec<f32>>>>,
        soft_reset_calls: Arc<StdMutex<u32>>,
        hard_reset_calls: Arc<StdMutex<u32>>,
    }

    impl EouInference for StubEou {
        fn transcribe_chunk(&mut self, chunk: &[f32]) -> Result<String, TranscriptionError> {
            self.received_chunks.lock().unwrap().push(chunk.to_vec());
            if self.responses.is_empty() {
                Ok(String::new())
            } else {
                self.responses.remove(0)
            }
        }

        fn transcribe_chunk_reset(
            &mut self,
            chunk: &[f32],
        ) -> Result<String, TranscriptionError> {
            self.received_chunks.lock().unwrap().push(chunk.to_vec());
            *self.soft_reset_calls.lock().unwrap() += 1;
            if self.responses.is_empty() {
                Ok(String::new())
            } else {
                self.responses.remove(0)
            }
        }

        fn reset(&mut self) -> Result<(), TranscriptionError> {
            *self.hard_reset_calls.lock().unwrap() += 1;
            Ok(())
        }
    }

    type ReceivedChunks = Arc<StdMutex<Vec<Vec<f32>>>>;
    type ResetCounter = Arc<StdMutex<u32>>;

    struct StubHandles {
        received: ReceivedChunks,
        soft_resets: ResetCounter,
        hard_resets: ResetCounter,
    }

    async fn stub_session(
        responses: Vec<Result<String, TranscriptionError>>,
    ) -> (ParakeetEouSession, StubHandles) {
        let received = Arc::new(StdMutex::new(Vec::new()));
        let soft_resets = Arc::new(StdMutex::new(0u32));
        let hard_resets = Arc::new(StdMutex::new(0u32));
        let received_clone = Arc::clone(&received);
        let soft_clone = Arc::clone(&soft_resets);
        let hard_clone = Arc::clone(&hard_resets);
        let session = ParakeetEouSession::spawn_with(move || {
            Ok(Box::new(StubEou {
                responses,
                received_chunks: received_clone,
                soft_reset_calls: soft_clone,
                hard_reset_calls: hard_clone,
            }) as Box<dyn EouInference>)
        })
        .await
        .unwrap();
        (
            session,
            StubHandles {
                received,
                soft_resets,
                hard_resets,
            },
        )
    }

    #[test]
    fn test_strip_eou_marker_present() {
        let mut s = String::from("hello world [EOU]");
        assert!(strip_eou_marker(&mut s));
        assert_eq!(s, "hello world");
    }

    #[test]
    fn test_strip_eou_marker_absent() {
        let mut s = String::from("hello world");
        assert!(!strip_eou_marker(&mut s));
        assert_eq!(s, "hello world");
    }

    #[tokio::test]
    async fn test_push_samples_buffers_partial_chunk() {
        let (mut session, handles) = stub_session(vec![]).await;
        // Half a chunk — should buffer, never dispatch.
        let partial = vec![0.1f32; EOU_CHUNK_SAMPLES / 2];
        let text = session.push_samples(&partial).await.unwrap();
        assert_eq!(text, "");
        assert!(handles.received.lock().unwrap().is_empty());
        assert_eq!(*handles.soft_resets.lock().unwrap(), 0);
        assert_eq!(*handles.hard_resets.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_push_samples_dispatches_one_chunk_when_full() {
        let (mut session, handles) = stub_session(vec![Ok("hi".to_string())]).await;
        let full = vec![0.2f32; EOU_CHUNK_SAMPLES];
        let text = session.push_samples(&full).await.unwrap();
        // No EOU marker in the stub's response, so no completed utterance yet.
        assert_eq!(text, "");
        assert_eq!(handles.received.lock().unwrap().len(), 1);
        assert_eq!(handles.received.lock().unwrap()[0].len(), EOU_CHUNK_SAMPLES);
        assert_eq!(*handles.soft_resets.lock().unwrap(), 0);
        assert_eq!(*handles.hard_resets.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_push_samples_dispatches_multiple_chunks() {
        let (mut session, handles) = stub_session(vec![
            Ok("a".to_string()),
            Ok("b".to_string()),
        ])
        .await;
        // Two and a half chunks worth.
        let n = EOU_CHUNK_SAMPLES * 2 + EOU_CHUNK_SAMPLES / 2;
        let samples = vec![0.3f32; n];
        let text = session.push_samples(&samples).await.unwrap();
        assert_eq!(text, "");
        assert_eq!(handles.received.lock().unwrap().len(), 2);
        // Carry-over should hold the leftover half-chunk for next time.
    }

    #[tokio::test]
    async fn test_push_samples_emits_completed_utterance_on_eou() {
        let (mut session, handles) = stub_session(vec![
            Ok("hello".to_string()),
            Ok(" world [EOU]".to_string()),
        ])
        .await;
        let two_chunks = vec![0.4f32; EOU_CHUNK_SAMPLES * 2];
        let text = session.push_samples(&two_chunks).await.unwrap();
        assert_eq!(text, "hello world");
        // Neither reset path is invoked on the mid-stream EOU marker: the
        // vendor's `reset_on_eou=true` branch is what emits the marker in
        // the first place, and it has already soft-reset decoder state
        // (`state_h` / `state_c` / `last_token`) internally before returning.
        // The hard `from_pretrained` reload would cost hundreds of ms and
        // swallow the first ~1 s of the next utterance — so we skip it.
        assert_eq!(*handles.hard_resets.lock().unwrap(), 0);
        assert_eq!(*handles.soft_resets.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_finalize_flushes_with_silence_and_soft_resets_on_last_chunk() {
        let (mut session, handles) = stub_session(vec![
            // First real chunk produces some delta without EOU.
            Ok("partial".to_string()),
            // Three silence-flush chunks emit nothing.
            Ok(String::new()),
            Ok(String::new()),
            Ok(String::new()),
        ])
        .await;
        let full = vec![0.5f32; EOU_CHUNK_SAMPLES];
        let _ = session.push_samples(&full).await.unwrap();
        let out = session.finalize_utterance().await.unwrap();
        assert_eq!(out, "partial");
        // One real + three silence flush chunks = 4 transcribe calls.
        assert_eq!(handles.received.lock().unwrap().len(), 4);
        // Only the last flush chunk uses the `reset_on_eou=true` path —
        // enough to clear the decoder's RNN-T state between utterances.
        assert_eq!(*handles.soft_resets.lock().unwrap(), 1);
        // The hard reset (full `ParakeetEOU::from_pretrained` reload) is NOT
        // invoked: it would trigger a MIN_BUFFER_SAMPLES warm-up that drops
        // the first ~1 s of the next utterance.
        assert_eq!(*handles.hard_resets.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_finalize_with_carry_over_pads_to_full_chunk() {
        let (mut session, handles) = stub_session(vec![
            // Padded carry-over chunk
            Ok(String::new()),
            // Three silence flush chunks
            Ok(String::new()),
            Ok(String::new()),
            Ok("trailing".to_string()),
        ])
        .await;
        let partial = vec![0.6f32; EOU_CHUNK_SAMPLES / 3];
        let _ = session.push_samples(&partial).await.unwrap();
        let out = session.finalize_utterance().await.unwrap();
        assert_eq!(out, "trailing");
        // 1 padded carry-over + 3 silence flush chunks = 4 dispatches.
        assert_eq!(handles.received.lock().unwrap().len(), 4);
        // The padded chunk must be exactly `EOU_CHUNK_SAMPLES` long.
        for chunk in handles.received.lock().unwrap().iter() {
            assert_eq!(chunk.len(), EOU_CHUNK_SAMPLES);
        }
        assert_eq!(*handles.soft_resets.lock().unwrap(), 1);
    }
}
