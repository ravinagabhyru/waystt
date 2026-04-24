# Waystt - Wayland Speech-to-Text Tool Roadmap

## Project Overview
**Waystt** is a minimal speech-to-text tool for Wayland environments. It runs as a local daemon and transcribes on demand through the `wayctl` Unix-socket control client. Built in Rust for minimal dependencies and optimal performance.

## Architecture: IPC-Based Transcription
1. **Launch**: Start the `waystt` daemon
2. **Command**: Receive a `wayctl` command to start, stop, or transcribe
3. **Transcribe**: Process recorded audio via OpenAI Whisper
4. **Output**: Inject text into active Wayland window (clipboard + paste)
5. **Continue**: Daemon stays available for the next command

## Core Goals
- **Ultra-minimal**: Single binary, no configuration files required
- **IPC-driven**: Local Unix-socket control with `wayctl`
- **Fast text injection**: Clipboard + paste instead of character-by-character typing
- **Privacy-first**: Optional local transcription, no persistent storage
- **Resource-efficient**: <50MB memory, minimal CPU when idle

## Simplified Workflow
```
waystt [daemon starts]
├── wayctl start: Begin recording
├── wayctl stop --output type: Stop → Transcribe → Type into active window
├── wayctl stop --output clipboard: Stop → Transcribe → Copy to clipboard
├── wayctl transcribe: Record until trailing silence → Transcribe → Print result
└── SIGTERM/SIGINT: Clean shutdown and exit
```

## Implementation Phases

### Phase 1: Core Tool (Week 1)
**Priority: Critical - MVP**

1. **Audio Recording Loop** ✅
   - CPAL integration for cross-platform continuous recording
   - Memory-managed buffer for audio data (5-minute max)
   - Lifecycle handlers for TERM/INT

2. **IPC Processing**
   - Unix-socket command handling for transcription triggers
   - Safe audio buffer extraction on command
   - Graceful shutdown on SIGTERM

3. **Basic Transcription**
   - OpenAI Whisper API integration
   - WAV encoding for API submission
   - Error handling and retry logic

### Phase 2: Text Injection (Week 2)
**Priority: High - User Experience**

4. **Fast Text Output**
   - Clipboard integration via wl-clipboard
   - Automatic paste simulation (Ctrl+V)
   - Fallback to character-by-character if paste fails

5. **Window Management**
   - Active window detection
   - Focus preservation during transcription
   - Notification on transcription completion

### Phase 3: Polish & Distribution (Week 3)
**Priority: Medium - Production Ready**

6. **Local Transcription (Optional)**
   - whisper.cpp integration as fallback
   - Automatic fallback when API unavailable
   - Model management for offline use

7. **Deployment**
   - Optional systemd user service template
   - Installation script and documentation
   - Package manager integration (AUR)

## Technical Details

### Text Injection Strategy
Instead of slow character-by-character typing:
1. **Primary**: Clipboard + paste simulation
   - `wl-copy` to set clipboard content
   - Simulate `Ctrl+V` keypress via Wayland
   - Instantaneous for any text length

2. **Fallback**: Direct text-input protocol
   - Wayland text-input protocol for direct injection
   - Used when clipboard method fails

### Control Interface
- **wayctl start**: Begin recording
- **wayctl stop**: Stop recording, transcribe, and emit to the selected output
- **wayctl transcribe**: Start recording, stop after trailing silence, and emit the result
- **SIGTERM/SIGINT**: Clean shutdown with buffer cleanup and exit

### Memory Management
- Circular audio buffer (default 5 minutes)
- No persistent storage - everything in memory
- Automatic buffer cleanup after transcription

## Minimal Dependencies
- **cpal**: Cross-platform audio recording (works with PipeWire, ALSA, etc.)
- **reqwest**: HTTP client for OpenAI API
- **wayland-client**: Window management and input simulation
- **signal-hook**: Lifecycle signal handling
- **serde**: Configuration (if needed)

## Usage Examples

```bash
# Single keybinding one-liners for compositor hotkeys:

# Transcribe and type result
bindkey "Super+R" "wayctl transcribe --output type"

# Transcribe and copy result
bindkey "Super+Shift+R" "wayctl transcribe --output clipboard"
```

## Technical Milestones

### v0.1.0 - MVP ✅ COMPLETED
- [x] Continuous audio recording via CPAL (cross-platform audio library)
- [x] Signal-based transcription with OpenAI Whisper
- [x] Clipboard + paste text injection
- [x] Basic error handling and logging
- [x] Audio feedback system with musical beeps
- [x] Direct text typing via ydotool

### v0.1.1 - Multi-Provider Support ✅ COMPLETED
- [x] Google Speech-to-Text provider integration
- [x] Transcription provider abstraction
- [x] Comprehensive configuration documentation
- [x] Provider-specific troubleshooting guides

### v0.2.0 - Simplified Architecture ✅ COMPLETED
- [x] Removed clipboard and dual-mode functionality
- [x] Simplified to stdout-only output
- [x] Added --pipe-to command flag
- [x] Enhanced test infrastructure

### v0.3.0 - Enhanced (Future)
- [ ] Local transcription fallback
- [ ] Systemd service integration
- [ ] Installation and distribution packages
- [ ] Performance optimizations

### v1.0.0 - Production
- [ ] Comprehensive error handling
- [ ] Multiple audio device support
- [ ] Configuration options
- [ ] Documentation and examples

## Success Metrics
- **Startup time**: <500ms to ready state
- **Memory usage**: <50MB during recording
- **Transcription latency**: <3s for 30-second clips
- **Text injection speed**: <200ms for any text length
- **Reliability**: 99%+ successful transcriptions

## Advantages of This Approach
1. **Simplicity**: No HTTP servers, no complex APIs, single keybinding operation
2. **Speed**: Instant text injection via clipboard
3. **Integration**: Works with any hotkey system, toggle-style workflow
4. **Reliability**: Fewer moving parts, less to break
5. **Privacy**: No network activity except during transcription
6. **User Experience**: Natural start/stop workflow with single key

This minimal approach provides maximum value with minimal complexity, perfect for a single-binary tool that "just works."
