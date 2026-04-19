## Project Overview

waystt is a Wayland speech-to-text tool that emits transcribed text to stdout
or a configurable output sink (clipboard / typed keystrokes / piped command).
It runs in one of two modes:
- **Daemon** (default): listens on a Unix socket for `wayctl` commands.
  Hotkeys should call `wayctl stop-and-transcribe` or `wayctl continuous-start`
  instead of sending signals.
- **Continuous** (`waystt --continuous`): starts capturing audio on launch
  and streams finalized utterances to the configured output until SIGTERM /
  SIGINT / Ctrl-C. When `PARAKEET_MODEL_TYPE=eou`, utterances are segmented
  by the model's own end-of-utterance detection for the lowest latency.

### Historical note (breaking change)
waystt previously used SIGUSR1 / SIGUSR2 as a control channel. Those signals
are no longer handled. Any hotkey or script that relied on them must be
migrated to `wayctl` subcommands. SIGTERM and SIGINT still shut down cleanly.

## Audio Feedback System

Configuration:
- `ENABLE_AUDIO_FEEDBACK=true/false` - Enable/disable beeps
- `BEEP_VOLUME=0.0-1.0` - Volume control (default: 0.1)

## Testing

### Environment Variables and Race Conditions

**Critical**: Tests that modify environment variables must use proper mutex protection to prevent race conditions when running in parallel.

#### Test Mutex System
The project uses a dual mutex system in `src/test_utils.rs`:

- `ENV_MUTEX` (sync): For synchronous tests
- `ASYNC_ENV_MUTEX` (async): For async tests that need to hold locks across await points

#### Synchronous Test Pattern
```rust
#[test]
fn test_name() {
    let _lock = ENV_MUTEX.lock().unwrap();
    
    // Save current environment state
    let original_value = std::env::var("ENV_VAR").ok();
    
    // Modify environment for test
    std::env::set_var("ENV_VAR", "test-value");
    
    // Run test logic
    let result = some_function();
    
    // Restore environment state
    if let Some(value) = original_value {
        std::env::set_var("ENV_VAR", value);
    } else {
        std::env::remove_var("ENV_VAR");
    }
    
    // Assertions
    assert!(result.is_ok());
}
```

#### Async Test Pattern
```rust
#[tokio::test]
async fn test_name() {
    #[allow(clippy::await_holding_lock)]
    {
        let _lock = ASYNC_ENV_MUTEX.lock().await;
        
        // Save current environment state
        let original_value = std::env::var("ENV_VAR").ok();
        
        // Modify environment for test
        std::env::set_var("ENV_VAR", "test-value");
        
        // Run async test logic
        let result = some_async_function().await;
        
        // Restore environment state
        if let Some(value) = original_value {
            std::env::set_var("ENV_VAR", value);
        } else {
            std::env::remove_var("ENV_VAR");
        }
        
        // Assertions
        assert!(result.is_ok());
    }
}
```

#### Key Principles
1. **Entire test must be protected**: Hold the mutex for the complete test duration, not just environment manipulation
2. **Always save and restore**: Capture original environment state and restore it after the test
3. **Pedantic lint compliance**: Use `#[allow(clippy::await_holding_lock)]` for async tests - this is intentional and necessary
4. **Import from test_utils**: `use crate::test_utils::{ENV_MUTEX, ASYNC_ENV_MUTEX};`

#### Why This Approach
- **Prevents race conditions**: No gaps between environment setup and async operations
- **Proper test isolation**: Each test has exclusive access to environment variables
- **Parallel execution safe**: Tests can run in parallel without interfering with each other
- **Lint compliant**: Explicitly acknowledges that holding async locks is intentional for test correctness

### Running Tests
- Always set the beep volume to 0, when running tests `BEEP_VOLUME=0.0 cargo test...`
- When developing/testing, use `--config config.toml` to use the project-local config file instead of ~/.config/waystt/config.toml
- Example: `BEEP_VOLUME=0.0 cargo run -- --config config.toml`
- Env vars still work and always override file values (handy for secrets like `OPENAI_API_KEY`).

## QA Testing Workflow

### Daemon mode (wayctl-controlled)
- Launch detached:
  ```bash
  nohup ./target/release/waystt --config config.toml > /tmp/waystt.log 2>&1 & disown
  ```
- Drive with `wayctl` from a separate shell:
  - `wayctl start` — begin recording (listen for the start beep)
  - speak a few seconds
  - `wayctl stop-and-transcribe` — stop + transcribe + emit
  - or `wayctl transcribe` — auto-detect trailing silence and transcribe
- For streaming continuous capture under the daemon:
  - `wayctl continuous-start` — utterances stream to stdout / clipboard / typed
  - `wayctl continuous-stop` — flushes and emits stats
- Tail logs: `tail -f /tmp/waystt.log`

### Continuous mode (no daemon)
```bash
nohup ./target/release/waystt --continuous --config config.toml > /tmp/waystt.log 2>&1 & disown
```
- Start beep plays on launch, utterances appear on stdout as they finalize
- `pkill -SIGTERM waystt` (or Ctrl-C if attached) for clean shutdown — expect
  the stop beep and a final stats line

### Streaming Parakeet smoke test
1. `transcription_provider = "parakeet"` and `[parakeet] model_type = "eou"` in `config.toml`
2. Ensure EOU model files (`encoder.onnx`, `decoder_joint.onnx`, `tokenizer.json`)
   exist under `~/.local/share/applications/waystt/parakeet/eou/`
3. `waystt --continuous --config config.toml` — expect a single "Pre-loading Parakeet
   EOU model..." log line at startup (warm-up), then instant first-utterance
   latency
4. Speak two sentences separated by ~1 second of silence; confirm two separate
   stdout lines (not one merged blob)

## Configuration Files

Key files for future development:
- `src/lib.rs`: Library entrypoint; picks between daemon IPC and continuous mode
- `src/app.rs`: App orchestration (init, continuous run loop, IPC handlers)
- `src/continuous.rs`: Silence-based batching + streaming-session driver
- `src/signals.rs`: Lifecycle signals only (SIGTERM, SIGINT)
- `src/ipc.rs`: Unix-socket daemon protocol for `wayctl`
- `src/beep.rs`: Musical audio feedback system with CPAL
- `src/audio.rs`: Audio recording via PipeWire/CPAL
- `src/config.rs`: Environment variable configuration
- `src/transcription/parakeet.rs`: Parakeet CTC / TDT batch provider
- `src/transcription/parakeet_streaming.rs`: Parakeet EOU streaming provider
- `config.toml.example`: Configuration template (sectioned TOML; env vars override)