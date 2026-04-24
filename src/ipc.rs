use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::app::App;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    #[default]
    Stdout,
    Clipboard,
    Type,
    Wtype,
    Ydotool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TypeNewlines {
    #[default]
    Spaces,
    Enter,
    Literal,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IpcOptions {
    #[serde(default)]
    pub output: OutputMode,
    #[serde(default)]
    pub type_newlines: TypeNewlines,
    #[serde(default)]
    pub silence_ms: Option<u64>,
    /// Silence threshold in ms for continuous mode chunk extraction
    #[serde(default)]
    pub continuous_silence_ms: Option<u64>,
    /// Number of parallel transcription workers for continuous mode (1-4)
    #[serde(default)]
    pub continuous_workers: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub id: String,
    pub cmd: String,
    #[serde(default)]
    pub options: IpcOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IpcResult {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<IpcResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<IpcError>,
}

fn ensure_runtime_dir() -> Result<PathBuf> {
    let dir_base = if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(xdg)
    } else {
        let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
        let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
        PathBuf::from(tmp).join(format!("waystt-{user}"))
    };
    let dir = dir_base.join("waystt");
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

#[must_use]
pub fn default_socket_path() -> PathBuf {
    ensure_runtime_dir()
        .unwrap_or_else(|_| PathBuf::from("/tmp/waystt"))
        .join("waystt.sock")
}

/// Serve IPC requests on a Unix socket
///
/// # Errors
///
/// Returns an error if the socket cannot be bound or if an I/O error occurs
pub async fn serve(mut app: App, socket_path: PathBuf) -> Result<()> {
    // Clean stale socket
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }

    let listener = UnixListener::bind(&socket_path)
        .map_err(|e| anyhow!("Failed to bind socket {}: {}", socket_path.display(), e))?;

    eprintln!("✅ waystt IPC listening on {}", socket_path.display());

    let audio_notify = app.audio_notify();

    loop {
        // Use select! to handle both IPC connections and audio-driven
        // continuous-mode processing.
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _)) => {
                        // Handle connections sequentially to avoid reentrancy for now
                        if let Err(e) = handle_connection(stream, &mut app).await {
                            eprintln!("IPC connection error: {e}");
                        }
                    }
                    Err(e) => {
                        eprintln!("Accept error: {e}");
                    }
                }
            }
            // Wake immediately when CPAL delivers a new audio buffer.
            // `notify_one` on the CPAL side coalesces bursts, so one wake
            // reliably drains all samples queued since the last pass.
            () = audio_notify.notified() => {
                if let Err(e) = app.ipc_continuous_process().await {
                    eprintln!("Continuous processing error: {e}");
                }
            }
        }
    }
}

async fn handle_connection(stream: UnixStream, app: &mut App) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut buf = String::new();

    while reader.read_line(&mut buf).await? != 0 {
        let line = buf.trim_end_matches(['\r', '\n']).to_string();
        buf.clear();

        if line.is_empty() {
            continue;
        }

        let req: IpcRequest = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let resp = IpcResponse {
                    id: String::new(),
                    ok: false,
                    result: None,
                    error: Some(IpcError {
                        code: "bad_request".into(),
                        message: e.to_string(),
                    }),
                };
                let payload = serde_json::to_string(&resp)? + "\n";
                writer.write_all(payload.as_bytes()).await?;
                continue;
            }
        };

        let resp = dispatch_request(app, req).await;
        let payload = serde_json::to_string(&resp)? + "\n";
        writer.write_all(payload.as_bytes()).await?;
        writer.flush().await?;
    }
    Ok(())
}

async fn dispatch_request(app: &mut App, req: IpcRequest) -> IpcResponse {
    let id = req.id.clone();
    let cmd = req.cmd.as_str();
    let opts = req.options.clone();

    let fail = |code: &str, message: String| IpcResponse {
        id: id.clone(),
        ok: false,
        result: None,
        error: Some(IpcError {
            code: code.into(),
            message,
        }),
    };

    match cmd {
        "ping" | "status" => IpcResponse {
            id,
            ok: true,
            result: Some(app.ipc_status()),
            error: None,
        },
        "start" => match app.ipc_start().await {
            Ok(()) => IpcResponse {
                id,
                ok: true,
                result: Some(app.ipc_status()),
                error: None,
            },
            Err(e) => fail("invalid_state", e.to_string()),
        },
        "cancel" => match app.ipc_cancel().await {
            Ok(()) => IpcResponse {
                id,
                ok: true,
                result: Some(app.ipc_status()),
                error: None,
            },
            Err(e) => fail("invalid_state", e.to_string()),
        },
        "stop" => match app.ipc_stop_and_transcribe(opts).await {
            Ok((text, duration_ms, output_mode)) => IpcResponse {
                id,
                ok: true,
                result: Some(IpcResult {
                    state: app.ipc_status().state,
                    text,
                    output: format!("{output_mode:?}").to_lowercase(),
                    duration_ms,
                    provider: app.ipc_status().provider,
                    model: app.ipc_status().model,
                }),
                error: None,
            },
            Err(e) => fail("internal", e.to_string()),
        },
        "transcribe" => {
            // If not recording, start and capture until trailing silence
            if !app.is_recording() {
                if let Err(e) = app.ipc_start().await {
                    return fail("invalid_state", format!("failed to start recording: {e}"));
                }
                // Capture until trailing silence:
                // - at least 300ms of speech after first voice
                // - configurable trailing silence (default 3000ms)
                // - max 10 seconds cap
                let silence = opts.silence_ms.unwrap_or(3000).clamp(100, 30_000);
                if let Err(e) = app.ipc_capture_until_silence(300, silence, 10_000).await {
                    return fail("internal", format!("capture error: {e}"));
                }
            }
            match app.ipc_stop_and_transcribe(opts).await {
                Ok((text, duration_ms, output_mode)) => IpcResponse {
                    id,
                    ok: true,
                    result: Some(IpcResult {
                        state: app.ipc_status().state,
                        text,
                        output: format!("{output_mode:?}").to_lowercase(),
                        duration_ms,
                        provider: app.ipc_status().provider,
                        model: app.ipc_status().model,
                    }),
                    error: None,
                },
                Err(e) => fail("internal", e.to_string()),
            }
        }
        "continuous_start" => {
            // Start continuous speech recognition mode
            match app.ipc_continuous_start(opts).await {
                Ok(()) => IpcResponse {
                    id,
                    ok: true,
                    result: Some(IpcResult {
                        state: "Continuous".to_string(),
                        ..IpcResult::default()
                    }),
                    error: None,
                },
                Err(e) => fail("invalid_state", e.to_string()),
            }
        }
        "continuous_stop" => {
            // Stop continuous mode and return stats
            match app.ipc_continuous_stop().await {
                Ok(stats) => IpcResponse {
                    id,
                    ok: true,
                    result: Some(IpcResult {
                        state: "Idle".to_string(),
                        text: format!(
                            "Captured {} chunks, transcribed {}, failed {}, total {:.1}s audio",
                            stats.chunks_captured,
                            stats.chunks_transcribed,
                            stats.chunks_failed,
                            stats.total_audio_seconds
                        ),
                        ..IpcResult::default()
                    }),
                    error: None,
                },
                Err(e) => fail("invalid_state", e.to_string()),
            }
        }
        "continuous_status" => {
            // Get continuous mode status
            let continuous_state = app.ipc_continuous_status();
            let state_str = match continuous_state {
                Some(crate::continuous::ContinuousState::Running) => "ContinuousRunning",
                Some(crate::continuous::ContinuousState::Stopping) => "ContinuousStopping",
                Some(crate::continuous::ContinuousState::Stopped) | None => {
                    if app.is_recording() {
                        "Recording"
                    } else {
                        "Idle"
                    }
                }
            };
            IpcResponse {
                id,
                ok: true,
                result: Some(IpcResult {
                    state: state_str.to_string(),
                    provider: app.ipc_status().provider,
                    model: app.ipc_status().model,
                    ..IpcResult::default()
                }),
                error: None,
            }
        }
        _ => fail("unknown_cmd", format!("Unknown command: {cmd}")),
    }
}

/// Copy text to clipboard using wl-copy or xclip
///
/// # Errors
///
/// Returns an error if neither wl-copy nor xclip are available
pub async fn copy_to_clipboard(text: &str) -> Result<()> {
    // Try wl-copy
    let wl = vec!["wl-copy".to_string()];
    if crate::command::execute_with_input(&wl, text)
        .await
        .unwrap_or(-1)
        == 0
    {
        return Ok(());
    }
    // Try xclip if DISPLAY exists
    if std::env::var("DISPLAY").is_ok() {
        let x = vec![
            "xclip".to_string(),
            "-selection".to_string(),
            "clipboard".to_string(),
        ];
        if crate::command::execute_with_input(&x, text)
            .await
            .unwrap_or(-1)
            == 0
        {
            return Ok(());
        }
    }
    Err(anyhow!("no_backend: neither wl-copy nor xclip available"))
}

/// Chunk size for typing text (characters per chunk)
const TYPE_CHUNK_SIZE: usize = 50;
/// Delay between chunks in milliseconds
const TYPE_CHUNK_DELAY_MS: u64 = 15;

/// Type text using ydotool or wtype
///
/// # Errors
///
/// Returns an error if neither ydotool nor wtype are available
pub async fn type_text(text: &str, nl: TypeNewlines) -> Result<()> {
    let mapped = match nl {
        TypeNewlines::Spaces => text.replace(['\r', '\n'], " "),
        TypeNewlines::Enter | TypeNewlines::Literal => text.to_string(),
    };

    // Try ydotool first (types all at once, has its own internal delay)
    let y = vec![
        "ydotool".to_string(),
        "type".to_string(),
        "--file".to_string(),
        "-".to_string(),
    ];
    let code = crate::command::execute_with_input(&y, &mapped)
        .await
        .unwrap_or(-1);
    if code == 0 {
        return Ok(());
    }

    // Try wtype with chunking to prevent character dropping
    type_text_chunked_wtype(&mapped).await
}

/// Type text using wtype in chunks with delays between chunks
async fn type_text_chunked_wtype(text: &str) -> Result<()> {
    let chunks = split_wtype_chunks(text, TYPE_CHUNK_SIZE);

    for (i, chunk) in chunks.iter().enumerate() {
        // Add delay between chunks (not before the first one)
        if i > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(TYPE_CHUNK_DELAY_MS)).await;
        }

        // Use -d 3 for 3ms delay between keystrokes within the chunk
        let w = vec![
            "wtype".to_string(),
            "-d".to_string(),
            "3".to_string(),
            "-".to_string(),
        ];
        let code = crate::command::execute_with_input(&w, chunk)
            .await
            .unwrap_or(-1);
        if code != 0 {
            return Err(anyhow!("wtype failed or not available"));
        }
    }

    Ok(())
}

fn split_wtype_chunks(text: &str, max_chars: usize) -> Vec<String> {
    assert!(max_chars > 0, "max_chars must be positive");

    let chars: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut start = 0;

    while start < chars.len() {
        let mut end = (start + max_chars).min(chars.len());

        // wtype + Electron can drop spaces when a burst starts with
        // whitespace. Keep boundary whitespace attached to the previous burst.
        while end < chars.len() && chars[end].is_whitespace() {
            end += 1;
        }

        chunks.push(chars[start..end].iter().collect());
        start = end;
    }

    chunks
}

/// Type text explicitly using wtype
///
/// # Errors
///
/// Returns an error if wtype is not available or fails
pub async fn type_text_wtype(text: &str, nl: TypeNewlines) -> Result<()> {
    let mapped = match nl {
        TypeNewlines::Spaces => text.replace(['\r', '\n'], " "),
        TypeNewlines::Enter | TypeNewlines::Literal => text.to_string(),
    };
    type_text_chunked_wtype(&mapped).await
}

/// Type text explicitly using ydotool
///
/// # Errors
///
/// Returns an error if ydotool is not available or fails
pub async fn type_text_ydotool(text: &str, nl: TypeNewlines) -> Result<()> {
    let mapped = match nl {
        TypeNewlines::Spaces => text.replace(['\r', '\n'], " "),
        TypeNewlines::Enter | TypeNewlines::Literal => text.to_string(),
    };

    let y = vec![
        "ydotool".to_string(),
        "type".to_string(),
        "--file".to_string(),
        "-".to_string(),
    ];
    let code = crate::command::execute_with_input(&y, &mapped)
        .await
        .unwrap_or(-1);
    if code == 0 {
        Ok(())
    } else {
        Err(anyhow!("ydotool failed or not available"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn join(chunks: &[String]) -> String {
        chunks.concat()
    }

    #[test]
    fn split_wtype_chunks_preserves_text() {
        let text = "hello world this is a browser typing regression test";
        let chunks = split_wtype_chunks(text, 10);

        assert_eq!(join(&chunks), text);
    }

    #[test]
    fn split_wtype_chunks_does_not_start_later_chunk_with_boundary_space() {
        let text = "abcdefghij klmnopqrst uvwxyz";
        let chunks = split_wtype_chunks(text, 10);

        assert_eq!(join(&chunks), text);
        assert_eq!(chunks[0], "abcdefghij ");
        assert!(chunks
            .iter()
            .skip(1)
            .all(|chunk| { !chunk.chars().next().is_some_and(|c| c.is_whitespace()) }));
    }

    #[test]
    fn split_wtype_chunks_keeps_runs_of_boundary_whitespace_on_previous_chunk() {
        let text = "abcdefghij   klmnopqrst";
        let chunks = split_wtype_chunks(text, 10);

        assert_eq!(join(&chunks), text);
        assert_eq!(chunks[0], "abcdefghij   ");
        assert_eq!(chunks[1], "klmnopqrst");
    }
}
