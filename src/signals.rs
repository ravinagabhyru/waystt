//! Lifecycle signal utilities.
//!
//! waystt previously used Unix user signals as a recording control channel.
//! That was removed in favour of the `wayctl` Unix-socket IPC.
//! Only lifecycle signals are handled here: SIGTERM (process managers, pkill)
//! and SIGINT (Ctrl-C) both trigger graceful shutdown.

pub const SHUTDOWN_SIG: i32 = signal_hook::consts::SIGTERM;

/// Build a signal stream for async handling of lifecycle signals.
///
/// Registers SIGTERM and SIGINT. Both are treated as shutdown requests and
/// surface through the returned stream.
///
/// # Errors
///
/// Returns an error if signal registration fails.
pub fn build_signal_stream() -> anyhow::Result<signal_hook_tokio::Signals> {
    use signal_hook::consts::signal::{SIGINT, SIGTERM};
    let signals = signal_hook_tokio::Signals::new([SIGINT, SIGTERM])?;
    Ok(signals)
}

/// Returns true if `sig` is a lifecycle shutdown signal (SIGTERM or SIGINT).
#[must_use]
pub fn is_shutdown_signal(sig: i32) -> bool {
    sig == signal_hook::consts::SIGTERM || sig == signal_hook::consts::SIGINT
}
