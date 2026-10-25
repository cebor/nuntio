//! A background thread that works at intervals until its owner drops it.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

/// The thread of a periodic task; it stops when this is dropped.
pub struct Ticker {
    _stop: mpsc::Sender<()>,
}

impl Ticker {
    /// Start the thread `name`. It calls `init` to make the task, then
    /// runs the task at once and again after each delay it returns, until
    /// it returns `None` (the event loop is gone) or the ticker is dropped.
    /// `init` runs on the new thread, so the task need not be `Send`.
    ///
    /// `start_failure` is logged (with the error) if the thread can't be
    /// spawned, and `stopped` when it ended because the ticker was dropped.
    pub fn start<T>(
        name: &str,
        start_failure: &'static str,
        stopped: &'static str,
        init: impl FnOnce() -> T + Send + 'static,
    ) -> Self
    where
        T: FnMut() -> Option<Duration>,
    {
        let (stop, stop_requested) = mpsc::channel();
        let spawned = thread::Builder::new().name(name.into()).spawn(move || {
            let mut tick = init();
            loop {
                let Some(wait) = tick() else {
                    return;
                };
                match stop_requested.recv_timeout(wait) {
                    Err(RecvTimeoutError::Timeout) => {}
                    _ => break,
                }
            }
            tracing::debug!("{stopped}");
        });
        if let Err(err) = spawned {
            tracing::warn!("{start_failure}: {err}");
        }
        Self { _stop: stop }
    }
}
