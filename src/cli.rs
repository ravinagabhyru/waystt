//! CLI-facing options decoupled from parsing.
//! The actual clap parsing lives in the binary and maps into this struct.

use std::path::PathBuf;

/// How the waystt process should run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunMode {
    /// Daemon mode (default): listen on the Unix-socket IPC for wayctl.
    #[default]
    Daemon,
    /// Continuous mode: start capturing audio immediately on launch and
    /// stream utterances to the configured output until SIGTERM / SIGINT.
    Continuous,
}

/// Options passed from the CLI into the library entrypoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOptions {
    pub envfile: Option<PathBuf>,
    pub pipe_to: Option<Vec<String>>,
    pub download_model: bool,
    pub mode: RunMode,
}

/// Default path for the env file.
#[must_use]
pub fn default_envfile_path() -> PathBuf {
    crate::config::default_envfile()
}
