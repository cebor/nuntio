//! What a pane's IO thread reads from and writes to: a pseudoterminal, or
//! on Windows the pipes to nuntio-wsl in WSL.

use std::io::{self, Read, Write};
use std::process::ExitStatus;
use std::sync::Arc;

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::{self, ChildEvent, EventedPty, EventedReadWrite};
use polling::{PollMode, Poller};

#[cfg(windows)]
use crate::wsl_pipe::WslPipe;

pub(crate) enum Backend {
    Pty(tty::Pty),
    #[cfg(windows)]
    Wsl(WslPipe),
}

pub(crate) enum BackendEvent {
    /// The shell exited.
    Exited(Option<ExitStatus>),
    /// nuntio-wsl exited before it started the shell.
    #[cfg(windows)]
    HelperFailed(Option<ExitStatus>),
}

impl Backend {
    /// # Safety
    ///
    /// The backend must be deregistered before it is dropped.
    pub(crate) unsafe fn register(
        &mut self,
        poller: &Arc<Poller>,
        interest: polling::Event,
        mode: PollMode,
    ) -> io::Result<()> {
        match self {
            // SAFETY: forwarded from the caller.
            Self::Pty(pty) => unsafe { pty.register(poller, interest, mode) },
            #[cfg(windows)]
            Self::Wsl(pipe) => {
                pipe.register(poller, interest);
                Ok(())
            }
        }
    }

    pub(crate) fn reregister(
        &mut self,
        poller: &Arc<Poller>,
        interest: polling::Event,
        mode: PollMode,
    ) -> io::Result<()> {
        match self {
            Self::Pty(pty) => pty.reregister(poller, interest, mode),
            #[cfg(windows)]
            Self::Wsl(pipe) => {
                pipe.register(poller, interest);
                Ok(())
            }
        }
    }

    pub(crate) fn deregister(&mut self, poller: &Arc<Poller>) -> io::Result<()> {
        match self {
            Self::Pty(pty) => pty.deregister(poller),
            #[cfg(windows)]
            Self::Wsl(pipe) => {
                pipe.deregister();
                Ok(())
            }
        }
    }

    pub(crate) fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Pty(pty) => pty.reader().read(buf),
            #[cfg(windows)]
            Self::Wsl(pipe) => pipe.read(buf),
        }
    }

    pub(crate) fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Pty(pty) => pty.writer().write(buf),
            #[cfg(windows)]
            Self::Wsl(pipe) => pipe.write(buf),
        }
    }

    /// Doesn't block.
    pub(crate) fn next_event(&mut self) -> Option<BackendEvent> {
        match self {
            Self::Pty(pty) => match pty.next_child_event()? {
                ChildEvent::Exited(status) => Some(BackendEvent::Exited(status)),
            },
            #[cfg(windows)]
            Self::Wsl(pipe) => pipe.next_event(),
        }
    }

    pub(crate) fn resize(&mut self, size: WindowSize) {
        match self {
            Self::Pty(pty) => pty.on_resize(size),
            #[cfg(windows)]
            Self::Wsl(pipe) => pipe.resize(size),
        }
    }

    /// The process nuntio started: the shell, `login`, or `wsl.exe`.
    pub(crate) fn pid(&self) -> Option<u32> {
        match self {
            #[cfg(unix)]
            Self::Pty(pty) => Some(pty.child().id()),
            #[cfg(windows)]
            Self::Pty(pty) => pty.child_watcher().pid().map(|pid| pid.get()),
            #[cfg(windows)]
            Self::Wsl(pipe) => Some(pipe.pid()),
        }
    }
}
