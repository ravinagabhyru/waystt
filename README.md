# waystt - Wayland Speech-to-Text Tool

Press a keybind, speak, and get instant text output. A speech-to-text tool that runs as a small Wayland-friendly daemon and is controlled with `wayctl`.

## Features

- **Daemon + control client**: `waystt` + `wayctl` to start/stop/transcribe
- **UNIX philosophy**: Outputs transcribed text to stdout for piping to other tools
- **On-demand transcription**: `wayctl transcribe` records until trailing silence and prints/copies/types the result
- **Audio feedback**: Beeps confirm recording start/stop and success
- **Wayland native**: Works with modern Linux desktops (Hyprland, Niri, etc.)
- **Optional local transcription**: Run Whisper locally using whisper-rs

## Requirements

- **Wayland desktop** (Hyprland, Niri, GNOME, KDE, etc.)
- **OpenAI API key** (for Whisper transcription)
- **System packages**:

```bash
# Arch Linux
sudo pacman -S pipewire

# Ubuntu/Debian  
sudo apt install pipewire-pulse

# Fedora
sudo dnf install pipewire-pulseaudio
```

**Optional tools (for output actions):**
```bash
# Arch Linux
sudo pacman -S wl-clipboard wtype ydotool

# Ubuntu/Debian  
sudo apt install wl-clipboard wtype ydotool xclip

# Fedora
sudo dnf install wl-clipboard wtype ydotool xclip

# Setup ydotool permissions and service:
sudo usermod -a -G input $USER

# Enable and start ydotool daemon service
sudo systemctl enable --now ydotool.service

# Set socket environment variable (add to ~/.bashrc or ~/.zshrc)
echo 'export YDOTOOL_SOCKET=/tmp/.ydotool_socket' >> ~/.bashrc

# Log out and back in (or source ~/.bashrc)
```

## Installation

### From AUR (Arch Linux)

```bash
# Using your preferred AUR helper
yay -S waystt-bin
# or
paru -S waystt-bin
```

### Download Binary

1. Download from [GitHub Releases](https://github.com/sevos/waystt/releases)
2. Install:

```bash
wget https://github.com/sevos/waystt/releases/latest/download/waystt-linux-x86_64
mkdir -p ~/.local/bin
mv waystt-linux-x86_64 ~/.local/bin/waystt
chmod +x ~/.local/bin/waystt

# Add to PATH (add to ~/.bashrc or ~/.zshrc)
export PATH="$HOME/.local/bin:$PATH"
```

## Quick Start

1. **Setup configuration:**
```bash
mkdir -p ~/.config/waystt
cp config.toml.example ~/.config/waystt/config.toml
# Edit ~/.config/waystt/config.toml or export OPENAI_API_KEY for OpenAI.
```

2. **Start the daemon:**
```bash
waystt
```

3. **Control it with `wayctl`:**
```bash
wayctl ping           # prints state/provider/model
wayctl status         # prints state/provider/model
wayctl start          # begin recording
wayctl stop           # stop + transcribe to stdout (default)
wayctl transcribe     # one-shot: auto-start, stop on trailing silence, transcribe

# Copy to clipboard or type directly
wayctl transcribe --output clipboard
wayctl transcribe --output type

# Configure trailing silence (default 3000ms)
wayctl transcribe --silence-ms 5000
```

## Quick Reference

### Common Commands

```bash
# Download local model and exit
waystt --download-model

# Start daemon in a terminal
waystt

# One-shot transcription, printing text to stdout
wayctl transcribe

# Copy or type the result
wayctl transcribe --output clipboard
wayctl transcribe --output type

# Start and stop manually
wayctl start
wayctl stop --output type
```

### Keybinding Pattern

Start `waystt` on login, then bind compositor shortcuts to `wayctl`:

```bash
# Start daemon on login (see systemd user unit below)

# Keybind examples
bind = SUPER, R, exec, wayctl start
bind = SUPER SHIFT, R, exec, wayctl stop --output type
bind = SUPER CTRL, R, exec, wayctl stop --output clipboard
```

## Keyboard Shortcuts Setup

### Hyprland

Add to your `~/.config/hypr/hyprland.conf`:

```bash
# waystt - Speech to Text (direct typing)
bind = SUPER, R, exec, wayctl transcribe --output type

# waystt - Speech to Text (clipboard copy)
bind = SUPER SHIFT, R, exec, wayctl transcribe --output clipboard
```

### Niri

Add to your `~/.config/niri/config.kdl`:

```kdl
binds {
    // waystt - Speech to Text (direct typing)
    Mod+R { spawn "wayctl" "transcribe" "--output" "type"; }
    
    // waystt - Speech to Text (clipboard copy)
    Mod+Shift+R { spawn "wayctl" "transcribe" "--output" "clipboard"; }
}
```

**Keybinding Functions:**
- **Super+R** (Hyprland) / **Mod+R** (Niri): Direct typing via ydotool
- **Super+Shift+R** (Hyprland) / **Mod+Shift+R** (Niri): Copy to clipboard

## Usage Examples

Start `waystt` once as a daemon, then use `wayctl` for recording and output actions.

### Basic Usage (stdout)

```bash
# Terminal 1: Start waystt
waystt

# Terminal 2: Record until trailing silence and print the transcript
wayctl transcribe | tee transcription.txt
```

### Output Actions

Use `wayctl` output modes to choose where text goes:

```bash
# Copy transcription to clipboard
wayctl transcribe --output clipboard

# Type transcription directly into focused window
wayctl transcribe --output type

# Save to file with timestamp
printf '%s: %s\n' "$(date)" "$(wayctl transcribe)" >> speech-log.txt
```


## Daemon + wayctl

Run a long-lived daemon and control it with `wayctl`.

### Socket path
- Default: `$XDG_RUNTIME_DIR/waystt/waystt.sock`
- If `XDG_RUNTIME_DIR` is not set, the daemon falls back to `/tmp/waystt-<user>/waystt.sock`.
- You can override the control client path with `wayctl --socket`.

### Commands
- `wayctl ping` → liveness + status summary
- `wayctl status` → state/provider/model
- `wayctl start` → begin recording
- `wayctl stop [--output stdout|clipboard|type]` → stop + transcribe
- `wayctl transcribe [--silence-ms 3000] [--output ...]` → auto-start, stop after trailing silence, transcribe
- `wayctl cancel` → stop without transcription

### Output modes
- `stdout` (default): print text to wayctl stdout
- `clipboard`: copy text using `wl-copy` (fallback: `xclip` on X11)
- `type`: type text into the focused window using `wtype` (fallback: `ydotool`)

Notes:
- Ensure `wl-clipboard` is installed for clipboard operations on Wayland.
- For typing, `wtype` is preferred; `ydotool` may require uinput permissions and a running daemon.

### Systemd user unit (optional)

Create `~/.config/systemd/user/waystt.service`:

```
[Unit]
Description=waystt daemon
After=pipewire.service

[Service]
ExecStart=%h/.local/bin/waystt
Restart=on-failure
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
```

Then:
```bash
systemctl --user daemon-reload
systemctl --user enable --now waystt.service
```

You can now bind `wayctl` commands in your compositor to control the daemon.

## Configuration

Configuration is read from `~/.config/waystt/config.toml` by default. Override the path with `--config`:

```bash
waystt --config /path/to/custom/config.toml
```

Environment variables always override any value set in the file, so you can keep secrets like API keys out of the file and export them at runtime. The env-var name mirrors the legacy naming: `[section].key` maps to `SECTION_KEY` (e.g. `[openai] api_key` → `OPENAI_API_KEY`, `[llm_refine] api_key` → `LLM_REFINE_API_KEY`).

See [`config.toml.example`](config.toml.example) for the full annotated template.

waystt supports four transcription providers: **Parakeet** (local ONNX, default), **OpenAI Whisper**, **Google Speech-to-Text**, and **local Whisper** (whisper-rs).

### OpenAI Whisper

OpenAI Whisper offers excellent accuracy and supports automatic language detection.

```toml
transcription_provider = "openai"

[openai]
api_key = "sk-..."                 # or export OPENAI_API_KEY

[whisper]
model = "whisper-1"                # default
language = "auto"                  # or "en", "es", ...
timeout_seconds = 60
max_retries = 3
```

### Google Speech-to-Text

Google Speech-to-Text provides fast, accurate transcription with support for many languages and dialects.

**Setup Steps:**

1. **Enable Google Cloud Speech-to-Text API:**
   - Go to [Google Cloud Console](https://console.cloud.google.com/)
   - Create a new project or select existing one
   - Enable the "Cloud Speech-to-Text API"
   - Create a service account and download the JSON key file

2. **Configure waystt for Google:**

```toml
transcription_provider = "google"

[google]
application_credentials = "/path/to/service-account-key.json"
language_code = "en-US"
model = "latest_long"              # or "latest_short"
alternative_languages = ["es-ES", "fr-FR", "de-DE"]   # optional auto-detect
```

### Local Whisper (whisper-rs)

Run transcription locally without sending audio to external APIs. Models are downloaded from [Hugging Face](https://huggingface.co/ggerganov/whisper.cpp) in GGML format.

```toml
transcription_provider = "local"

[whisper]
model = "ggml-base.en.bin"         # stored under ~/.local/share/applications/waystt/models/
```

Download with `waystt --download-model`.

**Available Models (GGML format):**
- `ggml-tiny.bin` - Fastest, least accurate (39 MB)
- `ggml-tiny.en.bin` - English-only tiny model (39 MB)
- `ggml-base.bin` - Small size, good performance (142 MB)
- `ggml-base.en.bin` - English-only base model (142 MB)
- `ggml-small.bin` - Better accuracy than base (466 MB)
- `ggml-small.en.bin` - English-only small model (466 MB)
- `ggml-medium.bin` - Good accuracy/speed balance (1.5 GB)
- `ggml-medium.en.bin` - English-only medium model (1.5 GB)
- `ggml-large.bin` - Best accuracy, slower (2.9 GB)
- `ggml-large-v1.bin` - Large model v1 (2.9 GB)
- `ggml-large-v2.bin` - Large model v2 (2.9 GB)
- `ggml-large-v3.bin` - Latest large model (2.9 GB)

**Recommendations:**
- **For English only**: Use `.en.bin` models for better performance
- **For speed**: `ggml-tiny.en.bin` or `ggml-base.en.bin`
- **For accuracy**: `ggml-large-v3.bin` or `ggml-medium.en.bin`
- **For balance**: `ggml-base.en.bin` (default)

If the configured model is missing, the application will exit with an error. OpenAI remains the default provider.

**Popular Google language codes:**
- `en-US` - English (United States)
- `en-GB` - English (United Kingdom)
- `es-ES` - Spanish (Spain)
- `fr-FR` - French (France)
- `de-DE` - German (Germany)
- `ja-JP` - Japanese
- `zh-CN` - Chinese (Simplified)

### General Settings

**Audio and system settings (apply to all providers):**

```toml
rust_log = "debug"

[beep]
enabled = false                    # disable start/stop beeps
volume = 0.1                       # 0.0 to 1.0
```

### Optional: LLM post-processing

Pipe transcriptions through an LLM to clean up filler words, fix punctuation, and correct misrecognitions before they reach the output. Covers OpenAI, Anthropic, Ollama, and any OpenAI-compatible endpoint via the `genai` crate.

```toml
[llm_refine]
enabled = true
model = "gpt-4o-mini"              # or "claude-haiku-4-5", "llama3.2", ...
# base_url = "http://localhost:11434/v1"   # e.g. Ollama
# api_key = "..."                  # or export LLM_REFINE_API_KEY
timeout_ms = 5000
```

Fails soft — any LLM error logs a warning and the original transcript is emitted.


## Troubleshooting

### IPC (daemon + wayctl)

- Verify the daemon is running and note the socket path it prints on startup (defaults to `$XDG_RUNTIME_DIR/waystt/waystt.sock`).
  - Check the socket exists: `ls -l "$XDG_RUNTIME_DIR/waystt/waystt.sock"`
  - If `XDG_RUNTIME_DIR` is not set, the daemon falls back to `/tmp/waystt-<user>/waystt.sock`. Pass this path to `wayctl` with `--socket`.
- Ensure `waystt` and `wayctl` are using the same socket:
  - `wayctl --socket "$XDG_RUNTIME_DIR/waystt/waystt.sock" ping`
- Remove stale sockets and restart the daemon if needed:
  - `rm -f "$XDG_RUNTIME_DIR/waystt/waystt.sock" && waystt`
- Permissions: the socket directory should be `0700`, socket file `0600`, and both owned by your user.
- Debug logs: run the daemon with `RUST_LOG=debug waystt` and re-run `wayctl`.
- Output actions failing with `no_backend`:
  - Clipboard: install `wl-clipboard` (Wayland) or `xclip` (X11) and re-try.
  - Type: install `wtype` (preferred) or set up `ydotool` (requires input group and running `ydotoold`).

### Audio Issues

If audio recording fails:
- Ensure PipeWire is running: `systemctl --user status pipewire`
- Check microphone permissions
- Verify microphone is not muted


### API Issues

**OpenAI Provider:**
- Verify your OpenAI API key is valid and has sufficient credits
- Check internet connectivity
- Review logs for specific error messages

**Google Provider:**
- Verify your service account JSON file path is correct
- Ensure the Speech-to-Text API is enabled in your Google Cloud project
- Check that your service account has the necessary permissions
- Verify your Google Cloud project has billing enabled
- Review logs for specific error messages

## Development

### Running Tests

```bash
cargo test
```

### Running with Debug Output

```bash
# Using default config location (~/.config/waystt/config.toml)
RUST_LOG=debug cargo run

# Or using a project-local config file for development
RUST_LOG=debug cargo run -- --config config.toml
```

## Building from Source

```bash
git clone https://github.com/sevos/waystt.git
cd waystt

# Create config directory and copy example configuration
mkdir -p ~/.config/waystt
cp config.toml.example ~/.config/waystt/config.toml
# Edit ~/.config/waystt/config.toml with your API key (or export OPENAI_API_KEY)

# Build the project
cargo build --release

# Install to local bin
mkdir -p ~/.local/bin
cp ./target/release/waystt ~/.local/bin/
```

## License

Licensed under GPL v3.0 or later. Source code: https://github.com/sevos/waystt

See [LICENSE](LICENSE) for full terms.
