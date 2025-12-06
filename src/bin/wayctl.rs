use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "wayctl")]
#[command(about = "Control client for waystt daemon")]
struct Cli {
    /// Path to the Unix socket (defaults to `XDG_RUNTIME_DIR/waystt/waystt.sock`)
    #[arg(long)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Health check
    Ping,
    /// Get daemon status
    Status,
    /// Start recording
    Start,
    /// Stop and transcribe
    Stop {
        #[arg(long, value_enum, default_value_t = OutputMode::Stdout)]
        output: OutputMode,
        #[arg(long, value_enum, default_value_t = TypeNewlines::Spaces)]
        type_newlines: TypeNewlines,
    },
    /// One-shot: start then stop+transcribe
    Transcribe {
        #[arg(long, value_enum, default_value_t = OutputMode::Stdout)]
        output: OutputMode,
        #[arg(long, value_enum, default_value_t = TypeNewlines::Spaces)]
        type_newlines: TypeNewlines,
        /// Trailing silence (ms) to stop recording (default 3000)
        #[arg(long)]
        silence_ms: Option<u64>,
    },
    /// Cancel recording (no transcription)
    Cancel,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum OutputMode {
    Stdout,
    Clipboard,
    Type,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum TypeNewlines {
    Spaces,
    Enter,
    Literal,
}

fn default_socket() -> PathBuf {
    waystt::ipc::default_socket_path()
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket_path = cli.socket.unwrap_or_else(default_socket);

    let (cmd, opts) = match cli.command {
        Commands::Ping => ("ping", json!({})),
        Commands::Status => ("status", json!({})),
        Commands::Start => ("start", json!({})),
        Commands::Cancel => ("cancel", json!({})),
        Commands::Stop {
            output,
            type_newlines,
        } => {
            let output_str = to_lower(output);
            let type_newlines_str = to_lower(type_newlines);
            (
                "stop",
                json!({
                    "output": output_str,
                    "type_newlines": type_newlines_str,
                }),
            )
        }
        Commands::Transcribe {
            output,
            type_newlines,
            silence_ms,
        } => {
            let mut opts = serde_json::Map::new();
            opts.insert("output".into(), serde_json::Value::String(to_lower(output)));
            opts.insert(
                "type_newlines".into(),
                serde_json::Value::String(to_lower(type_newlines)),
            );
            if let Some(ms) = silence_ms {
                opts.insert("silence_ms".into(), serde_json::Value::from(ms));
            }
            ("transcribe", serde_json::Value::Object(opts))
        }
    };

    let id = Uuid::new_v4().to_string();
    let req = json!({
        "id": id,
        "cmd": cmd,
        "options": opts,
    });

    let resp = send(&socket_path, &req.to_string()).await?;
    handle_response(&resp)?;
    Ok(())
}

async fn send(socket: &PathBuf, payload: &str) -> Result<serde_json::Value> {
    let socket_display = socket.display();
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|e| anyhow!("Failed to connect to {socket_display}: {e}"))?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    writer.write_all(payload.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let v: serde_json::Value = serde_json::from_str(&line)?;
    Ok(v)
}

fn handle_response(v: &serde_json::Value) -> Result<()> {
    let ok = v
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !ok {
        let err = v.get("error").cloned().unwrap_or_default();
        return Err(anyhow!(format!("error: {err}")));
    }
    let result = v.get("result").cloned().unwrap_or_default();
    // If there's text, print text (stdout mode). Otherwise show a concise status.
    if let Some(text) = result.get("text").and_then(|s| s.as_str()) {
        if !text.is_empty() {
            println!("{text}");
            return Ok(());
        }
    }
    // Print status-like fields for ping/status or empty text
    let state = result.get("state").and_then(|s| s.as_str()).unwrap_or("");
    let provider = result
        .get("provider")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let model = result.get("model").and_then(|s| s.as_str()).unwrap_or("");
    let duration = result
        .get("duration_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if !state.is_empty() || !provider.is_empty() || !model.is_empty() || duration > 0 {
        if !state.is_empty() {
            println!("state: {state}");
        }
        if !provider.is_empty() {
            println!("provider: {provider}");
        }
        if !model.is_empty() {
            println!("model: {model}");
        }
        if duration > 0 {
            println!("duration_ms: {duration}");
        }
    } else {
        // Fallback: print the raw JSON result for visibility
        println!(
            "{}",
            serde_json::to_string_pretty(&result).unwrap_or_default()
        );
    }
    Ok(())
}

fn to_lower<T: std::fmt::Debug>(v: T) -> String {
    format!("{v:?}").to_lowercase()
}
