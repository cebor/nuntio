//! Looks relative link candidates up on disk off the main thread, so that a
//! hung network mount can't freeze the window. The main thread waits a
//! moment for each answer, so that a quick Ctrl+click still finds the link.

use std::io;
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::time::Duration;

use crate::link::{BaseDir, Target};

/// How long the main thread waits for an answer before it goes on without.
const WAIT: Duration = Duration::from_millis(50);

/// A relative path to look up from `base`; `id` tells the answers apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    pub text: String,
    pub base: BaseDir,
}

/// Where request `id` leads, `None` if no file is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub id: u64,
    pub target: Option<Target>,
}

/// The `link-lookup` thread. One request runs at a time; of those that come
/// meanwhile only the newest is kept, so a stuck lookup holds up one thread.
pub struct Lookups {
    requests: mpsc::Sender<Request>,
    answers: mpsc::Receiver<Answer>,
    wait: Duration,
    /// A request is with the thread.
    running: bool,
    /// The newest request waiting for the running one.
    queued: Option<Request>,
}

impl Lookups {
    /// Start the thread; `wake` runs after each answer.
    pub fn start(wake: impl Fn() + Send + 'static) -> io::Result<Self> {
        Self::start_with(crate::link::relative_target, wake, WAIT)
    }

    fn start_with(
        look: impl Fn(&str, &BaseDir) -> Option<Target> + Send + 'static,
        wake: impl Fn() + Send + 'static,
        wait: Duration,
    ) -> io::Result<Self> {
        let (requests, inbox) = mpsc::channel::<Request>();
        let (outbox, answers) = mpsc::channel();
        std::thread::Builder::new()
            .name("link-lookup".into())
            .spawn(move || {
                for request in inbox {
                    let target = look(&request.text, &request.base);
                    if outbox
                        .send(Answer {
                            id: request.id,
                            target,
                        })
                        .is_err()
                    {
                        break;
                    }
                    wake();
                }
            })?;
        Ok(Self {
            requests,
            answers,
            wait,
            running: false,
            queued: None,
        })
    }

    /// Looks `request` up. If the thread is free, waits up to `wait` for the
    /// answer, else queues it in place of an older one. Returns the answers
    /// that came, older ones too; `None` if the thread is gone.
    pub fn ask(&mut self, request: Request) -> Option<Vec<Answer>> {
        let mut answers = self.drain()?;
        if self.running {
            self.queued = Some(request);
            return Some(answers);
        }
        // `request` is newer than a queued one.
        self.queued = None;
        self.requests.send(request).ok()?;
        self.running = true;
        match self.answers.recv_timeout(self.wait) {
            Ok(answer) => {
                answers.push(answer);
                self.running = false;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
        Some(answers)
    }

    /// The answers that came since; then starts the queued request, without
    /// waiting. `None` if the thread is gone.
    pub fn collect(&mut self) -> Option<Vec<Answer>> {
        let answers = self.drain()?;
        if !self.running
            && let Some(request) = self.queued.take()
        {
            self.requests.send(request).ok()?;
            self.running = true;
        }
        Some(answers)
    }

    /// The answers that came, without waiting; `None` if the thread is gone.
    fn drain(&mut self) -> Option<Vec<Answer>> {
        let mut answers = Vec::new();
        loop {
            match self.answers.try_recv() {
                Ok(answer) => {
                    answers.push(answer);
                    self.running = false;
                }
                Err(TryRecvError::Empty) => return Some(answers),
                Err(TryRecvError::Disconnected) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(id: u64, text: &str) -> Request {
        Request {
            id,
            text: text.into(),
            base: BaseDir::Local("/x".into()),
        }
    }

    fn ans(id: u64, text: &str) -> Answer {
        Answer {
            id,
            target: Some(Target::Path(text.into())),
        }
    }

    #[test]
    fn a_stuck_lookup_keeps_only_the_newest_request() {
        let (release, blocker) = mpsc::channel::<()>();
        let look = move |text: &str, _: &BaseDir| {
            if text == "stuck" {
                blocker.recv().unwrap();
            }
            Some(Target::Path(text.into()))
        };
        let (woke, wakes) = mpsc::channel();
        let wake = move || {
            let _ = woke.send(());
        };
        let mut lookups = Lookups::start_with(look, wake, Duration::from_secs(1)).unwrap();
        let wait_wake = || wakes.recv_timeout(Duration::from_secs(5)).unwrap();

        assert_eq!(lookups.ask(req(1, "a")), Some(vec![ans(1, "a")]));
        wait_wake();
        assert_eq!(lookups.ask(req(2, "stuck")), Some(vec![]));
        assert_eq!(lookups.ask(req(3, "b")), Some(vec![]));
        assert_eq!(lookups.ask(req(4, "c")), Some(vec![]));

        release.send(()).unwrap();
        wait_wake();
        assert_eq!(lookups.collect(), Some(vec![ans(2, "stuck")]));
        wait_wake();
        assert_eq!(lookups.collect(), Some(vec![ans(4, "c")]));
    }
}
