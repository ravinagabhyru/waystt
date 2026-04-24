# Technology Stack Documentation

## Overview
This document outlines the technology choices for **waystt**, an IPC-controlled speech-to-text daemon built with Rust. The stack prioritizes minimal dependencies, optimal performance, and reliable operation.

## Core Technologies

### Language: Rust (Edition 2021)
**Rationale**: 
- **Memory Safety**: Prevents audio buffer overruns and concurrency bugs
- **Zero-cost Abstractions**: Minimal runtime overhead for real-time audio processing
- **Single Binary**: No runtime dependencies, easy deployment
- **Excellent Async**: Perfect for handling audio streams and HTTP requests concurrently
- **Async I/O**: Unix-socket IPC, lifecycle handling, and transcription requests

### Architecture: Event-Driven Tool
**Design**: Single-threaded event loop with async I/O
- **Main Thread**: Unix-socket daemon loop with audio notifications
- **Async Tasks**: HTTP requests for transcription
- **Memory Management**: Circular buffer for audio data
- **State Management**: Simple state machine (Recording → Transcribing → Ready)

## Audio Pipeline

### Audio Capture: CPAL (Cross-Platform)
**Crate**: `cpal`
**Features**:
- Cross-platform audio capture (works with PipeWire, ALSA, PulseAudio)
- Automatic device discovery and format negotiation
- Low-latency audio capture with reliable stream management
- Native integration with Linux audio stack via multiple backends

**Configuration**: Audio settings optimized for Whisper API (16kHz mono, f32 samples)

**Backend Support**: Automatically uses best available backend (PipeWire → ALSA → PulseAudio)

### Audio Processing: Minimal Native
**No external audio libraries needed**:
- **Format**: Record directly to WAV format for API compatibility
- **Buffering**: Circular buffer implementation in safe Rust
- **Encoding**: Simple WAV header generation for API submission


## Lifecycle Handling

### Unix Signals: signal-hook
**Crate**: `signal-hook` + `signal-hook-tokio`
**Signals**:
- **SIGTERM/SIGINT**: Graceful shutdown with buffer cleanup

**Implementation**: Non-blocking lifecycle signal handling in async context using signal-hook-tokio. Recording control uses `wayctl` over the Unix socket.

## Transcription Services

### Primary: OpenAI Whisper API
**Crate**: `reqwest` (HTTP client)
**Features**:
- **Async HTTP**: Non-blocking API calls
- **Multipart Upload**: Direct WAV file upload
- **Retry Logic**: Exponential backoff for network failures
- **Error Handling**: Comprehensive error types

**API Integration**: Async multipart form upload to OpenAI Whisper API with retry logic

### Fallback: Local Transcription (Optional)
**Crate**: `candle-whisper` or direct `whisper.cpp` bindings
**Models**: Optimized for speed vs accuracy trade-off
- **tiny.en**: Ultra-fast, basic accuracy
- **base.en**: Balanced speed/accuracy
- **small.en**: High accuracy, slower

## Text Injection System

### Primary: Clipboard + Paste
**Approach**: Fastest method for any text length
**Crates**: 
- `wl-clipboard-rs`: Wayland clipboard integration
- `enigo`: Cross-platform input simulation

**Implementation**:
```rust
use wl_clipboard_rs::copy::{MimeType, Options, Source};

async fn inject_text(text: &str) -> Result<()> {
    // 1. Copy to clipboard
    let opts = Options::new();
    opts.copy(Source::Bytes(text.as_bytes().into()), MimeType::Text)?;
    
    // 2. Simulate Ctrl+V
    simulate_paste().await?;
    
    Ok(())
}
```

### Fallback: Direct Text Input
**Crate**: `wayland-client` + `wayland-protocols`
**Protocol**: `text-input-unstable-v3`
**Usage**: When clipboard method fails or is unavailable

## Configuration Management

### File-Based Configuration with Env Overrides
**Format**: Sectioned TOML file, overlaid by environment variables at runtime.
**Implementation**:
- **toml** + **serde**: Parse `config.toml` into a typed `ConfigFile`
- **clap**: CLI parsing with `--config PATH` override
- **Default path**: `~/.config/waystt/config.toml` (silently skipped if missing)
- Env vars are applied after file load and always win — handy for keeping
  secrets like API keys out of the checked-in file.

**Key variables** (see `docs/environment-configuration.md` for the full map):
- `[openai].api_key` / `OPENAI_API_KEY`: Required for OpenAI provider
- `[audio].buffer_duration_seconds` / `AUDIO_BUFFER_DURATION_SECONDS`: Ring-buffer size
- `[whisper].model` / `WHISPER_MODEL`: Model selection
- `[llm_refine].*` / `LLM_REFINE_*`: Optional LLM post-processing knobs
- `rust_log` / `RUST_LOG`: Logging configuration

**Rationale**: Structured, typed config for humans; env-var escape hatch for secrets and per-invocation tweaks.

## Dependency Minimization Strategy

### Core Dependencies (Essential)
- **cpal**: Cross-platform audio capture
- **reqwest**: HTTP client with multipart support
- **signal-hook-tokio**: Async signal handling
- **wl-clipboard-rs**: Wayland clipboard integration
- **tokio**: Async runtime
- **anyhow**: Error handling
- **clap**: Command line argument parsing
- **toml**: TOML config file parsing
- **serde**: Typed config deserialization

### Optional Dependencies (Features)
- **candle-whisper**: Local transcription support
- **enigo**: Input simulation fallback
- **serde**: Advanced configuration support

## Performance Optimizations

### Memory Management
- **Zero-copy Audio**: Direct buffer management without unnecessary allocations
- **Circular Buffer**: Fixed-size buffer prevents memory growth
- **Streaming Upload**: Stream audio data directly to API without temporary files

### CPU Efficiency
- **Single Thread**: No thread synchronization overhead
- **Async I/O**: Non-blocking operations for network and file I/O
- **Lazy Initialization**: Load transcription backends only when needed

### Binary Size Optimization
- **Size optimization**: Minimize binary footprint through compiler flags
- **Link-time optimization**: Enable LTO for smaller binaries
- **Symbol stripping**: Remove debug symbols in release builds

## Security Considerations

### Memory Safety
- **Buffer Overflows**: Prevented by Rust's ownership system
- **Signal Safety**: signal-hook provides async-signal-safe operations
- **API Key Handling**: Never logged or stored persistently

### Process Security
- **Minimal Privileges**: Runs as user process, no root required
- **Sandboxing Ready**: Compatible with systemd service restrictions
- **No Network Storage**: Audio data never written to disk

## Testing Strategy

### Unit Tests
- **Audio Buffer**: Circular buffer correctness
- **Signal Handling**: Mock signal delivery
- **Text Injection**: Mock clipboard operations

### Integration Tests
- **Audio Recording**: Test with generated audio
- **API Integration**: Mock OpenAI API responses
- **End-to-End**: Automated workflow testing

### Performance Benchmarks
- **Memory Usage**: Continuous monitoring during recording
- **Latency Measurement**: Signal → transcription → text injection
- **Audio Quality**: Ensure no dropouts or corruption

## Deployment Strategy

### Single Binary Distribution
**Target**: `x86_64-unknown-linux-gnu`
**Size Goal**: <10MB statically linked binary
**Dependencies**: Only glibc and Linux kernel APIs

### Installation Methods
1. **Direct Download**: Single binary from GitHub releases
2. **Cargo Install**: `cargo install waystt`
3. **Package Managers**: AUR package for Arch Linux
4. **Systemd Service**: Template service file included

This technology stack ensures **waystt** remains a lightweight, reliable, and fast speech-to-text solution with minimal dependencies while leveraging Rust's strengths for systems programming.
