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

    loop {
        let (stream, _) = listener.accept().await?;
        // Handle connections sequentially to avoid reentrancy for now
        if let Err(e) = handle_connection(stream, &mut app).await {
            eprintln!("IPC connection error: {e}");
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
                    error: Some(IpcError { code: "bad_request".into(), message: e.to_string() }),
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
        error: Some(IpcError { code: code.into(), message }),
    };

    match cmd {
        "ping" | "status" => IpcResponse {
            id,
            ok: true,
            result: Some(app.ipc_status()),
            error: None,
        },
        "start" => match app.ipc_start().await {
            Ok(()) => IpcResponse { id, ok: true, result: Some(app.ipc_status()), error: None },
            Err(e) => fail("invalid_state", e.to_string()),
        },
        "cancel" => match app.ipc_cancel().await {
            Ok(()) => IpcResponse { id, ok: true, result: Some(app.ipc_status()), error: None },
            Err(e) => fail("invalid_state", e.to_string()),
        },
        "stop" => {
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
    if crate::command::execute_with_input(&wl, text).await.unwrap_or(-1) == 0 {
        return Ok(());
    }
    // Try xclip if DISPLAY exists
    if std::env::var("DISPLAY").is_ok() {
        let x = vec!["xclip".to_string(), "-selection".to_string(), "clipboard".to_string()];
        if crate::command::execute_with_input(&x, text).await.unwrap_or(-1) == 0 {
            return Ok(());
        }
    }
    Err(anyhow!("no_backend: neither wl-copy nor xclip available"))
}

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
    // Try ydotool first
    let y = vec![
        "ydotool".to_string(),
        "type".to_string(),
        "--file".to_string(),
        "-".to_string(),
    ];
    let code = crate::command::execute_with_input(&y, &mapped).await.unwrap_or(-1);
    if code == 0 { return Ok(()); }

    // Try wtype --file - (not all versions support it)
    let w = vec!["wtype".to_string(), "--file".to_string(), "-".to_string()];
    let code = crate::command::execute_with_input(&w, &mapped).await.unwrap_or(-1);
    if code == 0 { return Ok(()); }

    Err(anyhow!("no_backend: neither ydotool nor wtype available/working"))
}
