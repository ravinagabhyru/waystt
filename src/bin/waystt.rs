use clap::Parser;

use waystt::cli::{default_envfile_path, RunMode, RunOptions};

#[derive(Parser)]
#[command(name = "waystt")]
#[command(
    about = "Wayland Speech-to-Text Tool - IPC daemon with optional continuous capture mode"
)]
#[command(version)]
struct Args {
    /// Path to environment file
    #[arg(long)]
    envfile: Option<std::path::PathBuf>,

    /// Pipe transcribed text to the specified command
    /// Usage: waystt --pipe-to command args
    /// Example: waystt --pipe-to wl-copy
    /// Example: waystt --pipe-to ydotool type --file -
    #[arg(long, short = 'p', num_args = 1.., value_name = "COMMAND", allow_hyphen_values = true, trailing_var_arg = true)]
    pipe_to: Option<Vec<String>>,

    /// Download the configured local model and exit
    #[arg(long)]
    download_model: bool,

    /// Start capturing audio immediately and stream utterances until
    /// SIGTERM / SIGINT. When omitted, waystt runs as an IPC daemon waiting
    /// for wayctl commands on its Unix socket.
    #[arg(long, short = 'c')]
    continuous: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let envfile = args.envfile.or_else(|| Some(default_envfile_path()));
    let mode = if args.continuous {
        RunMode::Continuous
    } else {
        RunMode::Daemon
    };
    let options = RunOptions {
        envfile,
        pipe_to: args.pipe_to,
        download_model: args.download_model,
        mode,
    };

    let code = waystt::run(options).await?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}
