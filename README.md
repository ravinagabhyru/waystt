# waystt - Wayland Speech-to-Text Tool

Press a keybind, speak, and get instant text output. A speech-to-text tool that transcribes audio using OpenAI Whisper and outputs to stdout. Now includes a daemon + control client (wayctl) for robust IPC-driven workflows.

## Features

- **Daemon + control client**: `waystt --daemon` + `wayctl` to start/stop/transcribe
- **Signal-driven**: Legacy flow still supported (SIGUSR1)
- **UNIX philosophy**: Outputs transcribed text to stdout for piping to other tools
- **On-demand operation**: Starts when called, processes audio, then exits
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
# Create config directory and file
mkdir -p ~/.config/waystt
echo "OPENAI_API_KEY=your_api_key_here" > ~/.config/waystt/.env
```

2. **Test the application (one-shot):**
```bash
# Run waystt and pipe output to see it working
waystt | tee /tmp/waystt-output.txt
```

3. **Use with signals (legacy):**
```bash
# Transcribe and output to stdout
pkill --signal SIGUSR1 waystt
```

4. **Daemon + wayctl (recommended):**
```bash
# Terminal A: Start daemon
waystt --daemon

# Terminal B: Control the daemon
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

# Start waystt and save output to file
waystt > output.txt

# Start waystt and copy output to clipboard
waystt --pipe-to wl-copy

# Start waystt and type output directly
waystt --pipe-to ydotool type --file -

# Trigger transcription (if waystt is running)
pkill --signal SIGUSR1 waystt
```

### Keybinding Pattern (legacy signals)

Most keybindings follow this pattern:
```bash
pgrep -x waystt >/dev/null && pkill --signal SIGUSR1 waystt || (waystt [OPTIONS] &)
```

This means: "If waystt is running, send signal to transcribe. Otherwise, start waystt with specified options."

### Keybinding Pattern (daemon + wayctl)

Recommended approach using the daemon and wayctl:

```bash
# Start daemon on login (see systemd user unit below)

# Keybind examples
bind = SUPER, R, exec, wayctl start
bind = SUPER SHIFT, R, exec, wayctl stop --output type
bind = SUPER CTRL, R, exec, wayctl stop --output clipboard
```

## Keyboard Shortcuts Setup

### Hyprland

Add to your `~/.config/hypr/hyprland.conf` to use signals instead of daemon:

```bash
# waystt - Speech to Text (direct typing)
bind = SUPER, R, exec, pgrep -x waystt >/dev/null && pkill --signal SIGUSR1 waystt || (waystt --pipe-to ydotool type --file - &)

# waystt - Speech to Text (clipboard copy)  
bind = SUPER SHIFT, R, exec, pgrep -x waystt >/dev/null && pkill --signal SIGUSR1 waystt || (waystt --pipe-to wl-copy &)
```

### Niri

Add to your `~/.config/niri/config.kdl` to use signals instead of daemon:

```kdl
binds {
    // waystt - Speech to Text (direct typing)
    Mod+R { spawn "sh" "-c" "pgrep -x waystt >/dev/null && pkill --signal SIGUSR1 waystt || (waystt --pipe-to ydotool type --file - &)"; }
    
    // waystt - Speech to Text (clipboard copy)
    Mod+Shift+R { spawn "sh" "-c" "pgrep -x waystt >/dev/null && pkill --signal SIGUSR1 waystt || (waystt --pipe-to wl-copy &)"; }
}
```

**Keybinding Functions:**
- **Super+R** (Hyprland) / **Mod+R** (Niri): Direct typing via ydotool
- **Super+Shift+R** (Hyprland) / **Mod+Shift+R** (Niri): Copy to clipboard

## Usage Examples

waystt starts on-demand, records audio, transcribes it, outputs to stdout, then exits:

### Basic Usage (stdout)

```bash
# Terminal 1: Start waystt with output to file
waystt > transcription.txt

# Terminal 2: Trigger transcription (or use keyboard shortcut)
pkill --signal SIGUSR1 waystt
```

### Using --pipe-to Option (one-shot)

The `--pipe-to` option allows you to pipe transcribed text directly to another command:

```bash
# Copy transcription to clipboard
waystt --pipe-to wl-copy
pkill --signal SIGUSR1 waystt

# Type transcription directly into focused window
waystt --pipe-to ydotool type --file -
pkill --signal SIGUSR1 waystt

# Process transcription with sed and copy to clipboard
waystt --pipe-to sh -c "sed 's/hello/hi/g' | wl-copy"
pkill --signal SIGUSR1 waystt

# Save to file with timestamp
waystt --pipe-to sh -c "echo \"$(date): $(cat)\" >> speech-log.txt"
pkill --signal SIGUSR1 waystt
```


## Daemon + wayctl

Run a long-lived daemon and control it with `wayctl`.

### Socket path
- Default: `$XDG_RUNTIME_DIR/waystt/waystt.sock`
- If `XDG_RUNTIME_DIR` is not set, the daemon falls back to `/tmp/waystt-<user>/waystt.sock`.
- You can override with `--socket` on both `waystt --daemon` and `wayctl`.

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
ExecStart=%h/.local/bin/waystt --daemon
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

Configuration is read from `~/.config/waystt/.env` by default. You can override this location using the `--envfile` flag:

```bash
waystt --envfile /path/to/custom/.env
```

waystt supports two transcription providers: **OpenAI Whisper** (default) and **Google Speech-to-Text**. Choose the one that best fits your needs.

### OpenAI Whisper (Default)

OpenAI Whisper offers excellent accuracy and supports automatic language detection.

**Required:** Create `~/.config/waystt/.env` with your OpenAI API key:

```bash
OPENAI_API_KEY=your_api_key_here
```

**Optional OpenAI settings:**
```bash
# Whisper model (whisper-1 is default, most cost-effective)
WHISPER_MODEL=whisper-1

# Force specific language (default: auto-detect)
WHISPER_LANGUAGE=en

# API timeout in seconds
WHISPER_TIMEOUT_SECONDS=60

# Max retry attempts
WHISPER_MAX_RETRIES=3
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

```bash
# Switch to Google provider
TRANSCRIPTION_PROVIDER=google

# Path to your service account JSON file
GOOGLE_APPLICATION_CREDENTIALS=/path/to/your/service-account-key.json

# Primary language (default: en-US)
GOOGLE_SPEECH_LANGUAGE_CODE=en-US

# Model selection (latest_long for longer audio, latest_short for shorter)
GOOGLE_SPEECH_MODEL=latest_long

# Optional: Alternative languages for auto-detection (comma-separated)
GOOGLE_SPEECH_ALTERNATIVE_LANGUAGES=es-ES,fr-FR,de-DE
```

### Local Whisper (whisper-rs)

Run transcription locally without sending audio to external APIs. Models are downloaded from [Hugging Face](https://huggingface.co/ggerganov/whisper.cpp) in GGML format.

```bash
# Switch to local provider
TRANSCRIPTION_PROVIDER=local

# Model file name stored in ~/.local/share/applications/waystt/models/
WHISPER_MODEL=ggml-base.en.bin

# Download the model and exit
waystt --download-model
```

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

**Audio and system settings (apply to both providers):**
```bash
# Disable audio beeps
ENABLE_AUDIO_FEEDBACK=false

# Adjust beep volume (0.0 to 1.0)
BEEP_VOLUME=0.1

# Debug logging
RUST_LOG=debug
```


## Troubleshooting

### IPC (daemon + wayctl)

- Verify the daemon is running and note the socket path it prints on startup (defaults to `$XDG_RUNTIME_DIR/waystt/waystt.sock`).
  - Check the socket exists: `ls -l "$XDG_RUNTIME_DIR/waystt/waystt.sock"`
  - If `XDG_RUNTIME_DIR` is not set, the daemon falls back to `/tmp/waystt-<user>/waystt.sock`. Pass this path to `wayctl` with `--socket`.
- Ensure `waystt` and `wayctl` are using the same socket:
  - `wayctl --socket "$XDG_RUNTIME_DIR/waystt/waystt.sock" ping`
- Remove stale sockets and restart the daemon if needed:
  - `rm -f "$XDG_RUNTIME_DIR/waystt/waystt.sock" && waystt --daemon`
- Permissions: the socket directory should be `0700`, socket file `0600`, and both owned by your user.
- Debug logs: run the daemon with `RUST_LOG=debug waystt --daemon` and re-run `wayctl`.
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
# Using default config location (~/.config/waystt/.env)
RUST_LOG=debug cargo run

# Or using project-local .env file for development
RUST_LOG=debug cargo run -- --envfile .env
```

## Building from Source

```bash
git clone https://github.com/sevos/waystt.git
cd waystt

# Create config directory and copy example configuration
mkdir -p ~/.config/waystt
cp .env.example ~/.config/waystt/.env
# Edit ~/.config/waystt/.env with your API key

# Build the project
cargo build --release

# Install to local bin
mkdir -p ~/.local/bin
cp ./target/release/waystt ~/.local/bin/
```

## License

Licensed under GPL v3.0 or later. Source code: https://github.com/sevos/waystt

See [LICENSE](LICENSE) for full terms.
