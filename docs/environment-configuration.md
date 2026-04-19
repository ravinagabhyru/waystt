# Configuration

waystt reads its configuration from a TOML file and overlays any matching
environment variables on top. Env vars always win, which makes them a
convenient escape hatch for secrets (API keys) that you don't want to check
into the file.

## Quick start

1. **Copy the example file:**
   ```bash
   mkdir -p ~/.config/waystt
   cp config.toml.example ~/.config/waystt/config.toml
   ```

2. **Edit `~/.config/waystt/config.toml`** with your settings (see
   `config.toml.example` for every option).

3. **Run waystt:**
   ```bash
   ./waystt
   # or with a custom config file:
   ./waystt --config /path/to/custom.toml
   ```

## Resolution order

1. The TOML file at `--config PATH` (error if the path is missing).
2. Otherwise `~/.config/waystt/config.toml` if it exists.
3. Otherwise built-in defaults only.
4. Environment variables are overlaid on whatever came out of 1–3.

## Environment-variable mapping

Env var names mirror the legacy naming. The rule is:
`[section].key` → `SECTION_KEY` (upper-cased, flattened).

| TOML                                | Env var                             |
| ----------------------------------- | ----------------------------------- |
| `transcription_provider`            | `TRANSCRIPTION_PROVIDER`            |
| `rust_log`                          | `RUST_LOG`                          |
| `[audio].sample_rate`               | `AUDIO_SAMPLE_RATE`                 |
| `[audio].channels`                  | `AUDIO_CHANNELS`                    |
| `[audio].buffer_duration_seconds`   | `AUDIO_BUFFER_DURATION_SECONDS`     |
| `[beep].enabled`                    | `ENABLE_AUDIO_FEEDBACK`             |
| `[beep].volume`                     | `BEEP_VOLUME`                       |
| `[openai].api_key`                  | `OPENAI_API_KEY`                    |
| `[openai].base_url`                 | `OPENAI_BASE_URL`                   |
| `[whisper].model`                   | `WHISPER_MODEL`                     |
| `[whisper].language`                | `WHISPER_LANGUAGE`                  |
| `[whisper].timeout_seconds`         | `WHISPER_TIMEOUT_SECONDS`           |
| `[whisper].max_retries`             | `WHISPER_MAX_RETRIES`               |
| `[google].application_credentials`  | `GOOGLE_APPLICATION_CREDENTIALS`    |
| `[google].language_code`            | `GOOGLE_SPEECH_LANGUAGE_CODE`       |
| `[google].model`                    | `GOOGLE_SPEECH_MODEL`               |
| `[google].alternative_languages`    | `GOOGLE_SPEECH_ALTERNATIVE_LANGUAGES` (comma-separated) |
| `[parakeet].model_type`             | `PARAKEET_MODEL_TYPE`               |
| `[parakeet].model_path`             | `PARAKEET_MODEL_PATH`               |
| `[llm_refine].enabled`              | `LLM_REFINE_ENABLED`                |
| `[llm_refine].apply_batch`          | `LLM_REFINE_APPLY_BATCH`            |
| `[llm_refine].apply_continuous`     | `LLM_REFINE_APPLY_CONTINUOUS`       |
| `[llm_refine].model`                | `LLM_REFINE_MODEL`                  |
| `[llm_refine].base_url`             | `LLM_REFINE_BASE_URL`               |
| `[llm_refine].api_key`              | `LLM_REFINE_API_KEY`                |
| `[llm_refine].timeout_ms`           | `LLM_REFINE_TIMEOUT_MS`             |
| `[llm_refine].system_prompt`        | `LLM_REFINE_SYSTEM_PROMPT`          |
| `[llm_refine].max_tokens`           | `LLM_REFINE_MAX_TOKENS`             |
| `[llm_refine].min_chars`            | `LLM_REFINE_MIN_CHARS`              |

## Security notes

- **Never commit `config.toml`** (or any file containing API keys) to version control.
- **Prefer env vars for secrets** — set `OPENAI_API_KEY` / `LLM_REFINE_API_KEY` in your shell, systemd unit, or service manager instead of putting the keys in the TOML file.
- `config.toml.example` is safe to share — it contains only placeholders.

## Legacy `.env` files

Older waystt releases read `~/.config/waystt/.env` via `dotenvy`. That path
is no longer loaded. waystt prints a one-line reminder at startup if it
finds a leftover `.env` at the legacy path so you know to migrate the
values into `config.toml` (or export them as env vars directly).
