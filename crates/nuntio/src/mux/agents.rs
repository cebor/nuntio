//! Following the agents of a running herdr session through its socket:
//! the list once, then again whenever herdr reports a change.

use std::io::{self, BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use winit::event_loop::EventLoopProxy;

use super::Target;
use super::herdr::{Agent, parse_agents};
use crate::event::UserEvent;

/// A herdr session on a target; `session: None` is the default session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub target: Target,
    pub session: Option<String>,
}

/// The agents of a session, sent after every change; empty once the
/// session is gone.
#[derive(Debug)]
pub struct AgentsUpdate {
    pub key: SessionKey,
    pub agents: Vec<Agent>,
}

/// How to reach a session's socket.
#[derive(Debug, Clone)]
pub enum Endpoint {
    #[cfg(unix)]
    Unix(String),
    #[cfg(windows)]
    Pipe(String),
    Wsl {
        distro: String,
        user: Option<String>,
        socket: String,
    },
}

impl Endpoint {
    /// The endpoint of the session whose socket is `socket` on `target`.
    pub fn of(target: &Target, socket: &str) -> Self {
        match target {
            Target::Wsl { distro, user } => Endpoint::Wsl {
                distro: distro.clone(),
                user: user.clone(),
                socket: socket.to_owned(),
            },
            #[cfg(windows)]
            Target::Local => Endpoint::Pipe(socket.to_owned()),
            #[cfg(unix)]
            Target::Local => Endpoint::Unix(socket.to_owned()),
        }
    }
}

/// Ends a connection, also from another thread while it blocks in a read.
type Closer = Box<dyn Fn() + Send + Sync>;

struct Connection {
    reader: Box<dyn BufRead + Send>,
    writer: Box<dyn Write + Send>,
    close: Closer,
}

impl Drop for Connection {
    fn drop(&mut self) {
        (self.close)();
    }
}

fn connect(endpoint: &Endpoint) -> io::Result<Connection> {
    match endpoint {
        #[cfg(unix)]
        Endpoint::Unix(path) => {
            let stream = std::os::unix::net::UnixStream::connect(path)?;
            let reader = stream.try_clone()?;
            let closer = stream.try_clone()?;
            Ok(Connection {
                reader: Box::new(BufReader::new(reader)),
                writer: Box::new(stream),
                close: Box::new(move || {
                    let _ = closer.shutdown(std::net::Shutdown::Both);
                }),
            })
        }
        #[cfg(windows)]
        Endpoint::Pipe(path) => {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)?;
            let reader = file.try_clone()?;
            Ok(Connection {
                reader: Box::new(BufReader::new(reader)),
                writer: Box::new(file),
                // The reader ends when the pipe's server side goes away;
                // a stopped monitor notices at its next line.
                close: Box::new(|| {}),
            })
        }
        Endpoint::Wsl {
            distro,
            user,
            socket,
        } => wsl_bridge(distro, user.as_deref(), socket),
    }
}

/// A connection through `nuntio-wsl socket`, which copies between its
/// standard streams and the socket inside the distribution.
#[cfg(windows)]
fn wsl_bridge(distro: &str, user: Option<&str>, socket: &str) -> io::Result<Connection> {
    use std::process::Stdio;

    let helper = crate::wsl::helper().ok_or_else(|| io::Error::other("no nuntio-wsl helper"))?;
    let mut command = std::process::Command::new("wsl.exe");
    command.args(["-d", distro]);
    if let Some(user) = user {
        command.args(["-u", user]);
    }
    command
        .args([
            "--exec",
            "/bin/sh",
            "-c",
            r#"exec "$(wslpath -u "$0")" "$@""#,
        ])
        .arg(helper)
        .args(["socket", socket])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::command::no_window(&mut command);
    let mut child = command.spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("no stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;
    let child = Mutex::new(child);
    Ok(Connection {
        reader: Box::new(BufReader::new(stdout)),
        writer: Box::new(stdin),
        close: Box::new(move || {
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }),
    })
}

#[cfg(not(windows))]
fn wsl_bridge(_distro: &str, _user: Option<&str>, _socket: &str) -> io::Result<Connection> {
    Err(io::Error::other("WSL exists only on Windows"))
}

/// The thread that follows one session; stops when dropped.
pub struct Monitor {
    stop: Arc<AtomicBool>,
    /// Closes the subscription the thread is waiting on.
    current: Arc<Mutex<Option<Closer>>>,
    thread: JoinHandle<()>,
}

impl Monitor {
    pub fn start(key: SessionKey, endpoint: Endpoint, proxy: EventLoopProxy<UserEvent>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let current = Arc::new(Mutex::new(None));
        let thread = {
            let (stop, current) = (stop.clone(), current.clone());
            std::thread::Builder::new()
                .name("herdr agents".into())
                .spawn(move || {
                    if let Err(err) = follow(&key, &endpoint, &proxy, &stop, &current) {
                        tracing::debug!(?key, "herdr agent monitor ended: {err}");
                    }
                    if !stop.load(Ordering::Relaxed) {
                        let _ = proxy.send_event(UserEvent::Agents(AgentsUpdate {
                            key,
                            agents: Vec::new(),
                        }));
                    }
                })
                .expect("spawning a thread")
        };
        Self {
            stop,
            current,
            thread,
        }
    }

    /// Whether the thread still follows its session.
    pub fn alive(&self) -> bool {
        !self.thread.is_finished()
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut current) = self.current.lock()
            && let Some(close) = current.take()
        {
            close();
        }
    }
}

/// One request on its own connection, and the answer's line.
fn request(endpoint: &Endpoint, line: &str) -> io::Result<String> {
    let mut conn = connect(endpoint)?;
    conn.writer.write_all(line.as_bytes())?;
    conn.writer.flush()?;
    let mut answer = String::new();
    conn.reader.read_line(&mut answer)?;
    if answer.is_empty() {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    Ok(answer)
}

const LIST: &str = "{\"id\":\"list\",\"method\":\"agent.list\",\"params\":{}}\n";

fn list(endpoint: &Endpoint) -> io::Result<Vec<Agent>> {
    let answer = request(endpoint, LIST)?;
    if answer.contains("\"error\"") && !answer.contains("\"agents\"") {
        return Err(io::Error::other(answer.trim().to_owned()));
    }
    Ok(parse_agents(&answer))
}

/// The subscription request for changes of the panes and of `agents`.
pub fn subscription(agents: &[Agent]) -> String {
    let mut subscriptions: Vec<serde_json::Value> = [
        "pane.created",
        "pane.closed",
        "pane.exited",
        "pane.agent_detected",
    ]
    .iter()
    .map(|t| serde_json::json!({ "type": t }))
    .collect();
    subscriptions.extend(
        agents.iter().map(
            |a| serde_json::json!({ "type": "pane.agent_status_changed", "pane_id": a.pane_id }),
        ),
    );
    let request = serde_json::json!({
        "id": "sub",
        "method": "events.subscribe",
        "params": { "subscriptions": subscriptions },
    });
    format!("{request}\n")
}

/// What a line pushed on the subscription asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Pushed {
    /// The subscription's own answer.
    Started,
    /// An agent's status changed: list again.
    Status,
    /// Panes came or went: list again and subscribe to the new set.
    Panes,
    /// herdr dropped events or rejected the subscription.
    Error,
}

pub fn classify(line: &str) -> Pushed {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Pushed::Error;
    };
    if value.get("error").is_some() {
        return Pushed::Error;
    }
    if value.pointer("/result/type").and_then(|t| t.as_str()) == Some("subscription_started") {
        return Pushed::Started;
    }
    if line.contains("agent_status_changed") {
        Pushed::Status
    } else {
        Pushed::Panes
    }
}

fn follow(
    key: &SessionKey,
    endpoint: &Endpoint,
    proxy: &EventLoopProxy<UserEvent>,
    stop: &AtomicBool,
    current: &Mutex<Option<Closer>>,
) -> io::Result<()> {
    let send = |agents: Vec<Agent>| {
        let _ = proxy.send_event(UserEvent::Agents(AgentsUpdate {
            key: key.clone(),
            agents,
        }));
    };
    let mut agents = list(endpoint)?;
    send(agents.clone());
    while !stop.load(Ordering::Relaxed) {
        let mut conn = connect(endpoint)?;
        conn.writer.write_all(subscription(&agents).as_bytes())?;
        conn.writer.flush()?;
        // Hand the closer over, so that dropping the monitor ends the read.
        let close = std::mem::replace(&mut conn.close, Box::new(|| {}));
        *current.lock().map_err(|_| io::Error::other("poisoned"))? = Some(close);
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let mut line = String::new();
        loop {
            line.clear();
            if conn.reader.read_line(&mut line)? == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            match classify(&line) {
                Pushed::Started => continue,
                Pushed::Status => {
                    agents = list(endpoint)?;
                    send(agents.clone());
                }
                Pushed::Panes => {
                    agents = list(endpoint)?;
                    send(agents.clone());
                    break;
                }
                Pushed::Error => {
                    tracing::debug!(?key, line = line.trim(), "herdr subscription error");
                    std::thread::sleep(Duration::from_secs(1));
                    agents = list(endpoint)?;
                    send(agents.clone());
                    break;
                }
            }
        }
        if let Ok(mut current) = current.lock()
            && let Some(close) = current.take()
        {
            close();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushed_lines_are_classified() {
        assert_eq!(
            classify(r#"{"id":"sub","result":{"type":"subscription_started"}}"#),
            Pushed::Started
        );
        assert_eq!(
            classify(r#"{"event":"pane.agent_status_changed","data":{"pane_id":"w1:p1"}}"#),
            Pushed::Status
        );
        assert_eq!(
            classify(r#"{"event":"pane.created","data":{"pane_id":"w1:p2"}}"#),
            Pushed::Panes
        );
        assert_eq!(
            classify(r#"{"id":"sub","error":{"code":"events_lost"}}"#),
            Pushed::Error
        );
        assert_eq!(classify("garbage"), Pushed::Error);
    }
}
