//! The helper itself: a Linux PTY between nuntio's pipes and the shell.
//!
//! Frames come in on stdin ([`crate::proto`]), the PTY's output goes out on stdout unchanged.

use std::ffi::{CStr, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use crate::proto::{Decoder, Frame, READY, Size, directory_marker, foreground_marker};

const CHUNK: usize = 64 * 1024;
/// The largest buffer `login_shell` offers `getpwuid_r` for the passwd entry.
const PASSWD_BUF_MAX: usize = 64 * 1024;
/// Output read after the shell exited; background jobs may keep writing forever.
const POST_EXIT_DRAIN: usize = 0x10_0000;
/// Input from nuntio waiting for the program to read it, before stdin is left unread.
const MAX_PENDING_INPUT: usize = 1 << 20;
/// How long after input or output the foreground process group is looked at periodically: a
/// command the shell starts after the user pressed Enter makes no output of its own.
const RECHECK_WINDOW: Duration = Duration::from_secs(1);
/// The period of those looks, in milliseconds.
const RECHECK_EVERY: libc::c_int = 50;

/// The write end of the SIGCHLD self-pipe, or -1 before it exists.
static WAKE_FD: AtomicI32 = AtomicI32::new(-1);

/// Runs the helper. `args` is empty for the login shell, or `-- <program> <args>…`.
/// Returns the exit code of the process.
pub fn run(args: Vec<OsString>) -> i32 {
    let stdin = 0;
    let mut decoder = Decoder::default();
    let mut pending = Vec::new();
    let size = match read_first_size(stdin, &mut decoder, &mut pending) {
        Ok(Some(size)) => size,
        Ok(None) => return 0,
        Err(message) => {
            eprintln!("nuntio-wsl: {message}");
            return 2;
        }
    };

    let wake = match install_sigchld() {
        Ok(wake) => wake,
        Err(err) => {
            eprintln!("nuntio-wsl: cannot watch the child: {err}");
            return 1;
        }
    };
    let (master, slave) = match open_pty(size) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("nuntio-wsl: cannot open a pty: {err}");
            return 1;
        }
    };
    let pid = match spawn(&args, slave) {
        Ok(pid) => pid,
        Err((program, err)) => {
            eprintln!(
                "nuntio-wsl: cannot run {}: {err}",
                program.to_string_lossy()
            );
            return 127;
        }
    };

    if let Err(err) = write_all(1, READY) {
        eprintln!("nuntio-wsl: cannot write to stdout: {err}");
        return hang_up(pid, 1);
    }
    event_loop(&master, pid, &wake, decoder, pending)
}

/// Connects stdin and stdout to the Unix socket at `path`, byte for byte, so nuntio on Windows
/// can talk to a server inside WSL. Returns 0 when the socket closes, 1 when it can't be
/// reached.
pub fn socket_bridge(path: OsString) -> i32 {
    use std::io::{Read, Write};
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;

    let mut reader = match UnixStream::connect(&path) {
        Ok(socket) => socket,
        Err(err) => {
            eprintln!(
                "nuntio-wsl: cannot connect to {}: {err}",
                Path::new(&path).display()
            );
            return 1;
        }
    };
    let mut writer = match reader.try_clone() {
        Ok(socket) => socket,
        Err(err) => {
            eprintln!("nuntio-wsl: cannot use the socket: {err}");
            return 1;
        }
    };
    // Ends with the process: the main thread returns when the socket closes.
    std::thread::spawn(move || {
        let _ = io::copy(&mut io::stdin().lock(), &mut writer);
        let _ = writer.shutdown(Shutdown::Write);
    });

    let mut stdout = io::stdout().lock();
    let mut buf = vec![0u8; CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return 0,
            Ok(n) => {
                if stdout
                    .write_all(&buf[..n])
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    return 0;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return 0,
        }
    }
}

/// Reads stdin until the first complete frame. `Ok(None)` at EOF. Data frames that follow the
/// size in the same read go to `pending`; later resizes replace the size.
fn read_first_size(
    fd: RawFd,
    decoder: &mut Decoder,
    pending: &mut Vec<u8>,
) -> Result<Option<Size>, String> {
    const SIZE_FIRST: &str = "expected the window size first";
    let mut buf = vec![0u8; CHUNK];
    let mut size = None;
    let mut misplaced = false;
    loop {
        let n = match read_fd(fd, &mut buf) {
            Ok(0) => return Ok(None),
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
            Err(err) => return Err(format!("cannot read stdin: {err}")),
        };
        decoder
            .feed(&buf[..n], |frame| match frame {
                Frame::Resize(s) => size = Some(s),
                Frame::Data(_) if size.is_none() => misplaced = true,
                Frame::Data(bytes) => pending.extend_from_slice(bytes),
            })
            .map_err(|err| format!("bad input: {err}"))?;
        if misplaced {
            return Err(SIZE_FIRST.into());
        }
        if size.is_some() {
            return Ok(size);
        }
    }
}

extern "C" fn on_sigchld(_: libc::c_int) {
    let fd = WAKE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: write and errno access are async-signal-safe; errno is restored so the
        // interrupted code doesn't see a stray error.
        unsafe {
            let errno = libc::__errno_location();
            let saved = *errno;
            libc::write(fd, [1u8].as_ptr().cast(), 1);
            *errno = saved;
        }
    }
}

/// Makes SIGCHLD write a byte to a pipe and returns its read end.
fn install_sigchld() -> io::Result<OwnedFd> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` has room for two descriptors.
    cvt(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) })?;
    // SAFETY: both descriptors are new and owned by us.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    // The handler uses the write end for the rest of the process's life.
    WAKE_FD.store(write.into_raw_fd(), Ordering::Relaxed);

    // SAFETY: a zeroed sigaction is valid; the handler only calls async-signal-safe functions.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_sigchld as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART | libc::SA_NOCLDSTOP;
        libc::sigemptyset(&mut action.sa_mask);
        cvt(libc::sigaction(
            libc::SIGCHLD,
            &action,
            std::ptr::null_mut(),
        ))?;
    }
    Ok(read)
}

fn winsize(size: Size) -> libc::winsize {
    libc::winsize {
        ws_row: size.lines,
        ws_col: size.columns,
        // Like alacritty's `to_winsize`.
        ws_xpixel: size.columns.saturating_mul(size.cell_width),
        ws_ypixel: size.lines.saturating_mul(size.cell_height),
    }
}

/// Opens a PTY. Returns the master (non-blocking) and the slave.
fn open_pty(size: Size) -> io::Result<(OwnedFd, OwnedFd)> {
    let ws = winsize(size);
    let (mut master, mut slave) = (0, 0);
    // SAFETY: the out-pointers are valid; no name and no termios are requested.
    cvt(unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &ws,
        )
    })?;
    // SAFETY: both descriptors are new and owned by us.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    for fd in [&master, &slave] {
        // SAFETY: plain fcntl calls on a descriptor we own.
        cvt(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) })?;
    }
    // SAFETY: as above.
    unsafe {
        let flags = cvt(libc::fcntl(master.as_raw_fd(), libc::F_GETFL))?;
        cvt(libc::fcntl(
            master.as_raw_fd(),
            libc::F_SETFL,
            flags | libc::O_NONBLOCK,
        ))?;
    }
    // Backspace in canonical mode erases whole UTF-8 characters, as alacritty sets it up.
    let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `termios` is a valid out-pointer and initialized by a successful call.
    unsafe {
        cvt(libc::tcgetattr(master.as_raw_fd(), termios.as_mut_ptr()))?;
        let mut termios = termios.assume_init();
        termios.c_iflag |= libc::IUTF8;
        cvt(libc::tcsetattr(master.as_raw_fd(), libc::TCSANOW, &termios))?;
    }
    Ok((master, slave))
}

/// The login shell of the current user, `/bin/sh` if the passwd entry has none.
fn login_shell() -> OsString {
    let fallback = || OsString::from("/bin/sh");
    let mut buf = vec![0 as libc::c_char; 1024];
    loop {
        // SAFETY: a zeroed passwd is valid to pass as the out-parameter.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found = std::ptr::null_mut();
        // SAFETY: `buf` and `entry` outlive the call, the length matches `buf`.
        let rc = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                &mut entry,
                buf.as_mut_ptr(),
                buf.len(),
                &mut found,
            )
        };
        if rc == libc::ERANGE && buf.len() < PASSWD_BUF_MAX {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || found.is_null() || entry.pw_shell.is_null() {
            return fallback();
        }
        // SAFETY: pw_shell points to a NUL-terminated string in `buf`.
        let shell = unsafe { CStr::from_ptr(entry.pw_shell) }.to_bytes();
        return if shell.is_empty() {
            fallback()
        } else {
            OsString::from_vec(shell.to_vec())
        };
    }
}

/// Starts the program on the slave as a session leader and returns its pid. On failure, the
/// program name and the error.
fn spawn(args: &[OsString], slave: OwnedFd) -> Result<libc::pid_t, (OsString, io::Error)> {
    let mut command;
    let program;
    if args.first().is_some_and(|a| a == "--") && args.len() > 1 {
        program = args[1].clone();
        command = Command::new(&program);
        command.args(&args[2..]);
    } else {
        program = login_shell();
        command = Command::new(&program);
        let name = Path::new(&program)
            .file_name()
            .unwrap_or_else(|| OsStr::new("sh"));
        let mut arg0 = OsString::from("-");
        arg0.push(name);
        command.arg0(arg0);
    }

    let attach = |command: &mut Command| -> io::Result<()> {
        command.stdin(Stdio::from(slave.try_clone()?));
        command.stdout(Stdio::from(slave.try_clone()?));
        command.stderr(Stdio::from(slave.try_clone()?));
        Ok(())
    };
    attach(&mut command).map_err(|err| (program.clone(), err))?;
    // SAFETY: only async-signal-safe calls between fork and exec. std has already
    // restored the default SIGPIPE in the child.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            // The slave is stdin by now; failing to take it as controlling terminal is not fatal.
            libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            Ok(())
        });
    }
    let child = command.spawn().map_err(|err| (program.clone(), err))?;
    // Our copies of the slave, so the master sees a hangup when the shell is gone.
    drop(command);
    drop(slave);
    Ok(child.id() as libc::pid_t)
}

/// The foreground process group last reported to nuntio.
struct Reported {
    pgid: libc::pid_t,
    name: Vec<u8>,
    /// When `pgid` was first reported. Shortly after, its name may still be the shell's: the
    /// shell's child takes the terminal before it execs the command.
    since: Instant,
}

/// Tells nuntio the foreground process group of the PTY if it differs from the last report,
/// or if a group reported less than `RECHECK_WINDOW` ago has a new name (it exec'd).
/// Nothing is written when the terminal can't say. A group without a readable `comm` entry is
/// reported with an empty name.
fn report_foreground(
    master: RawFd,
    shell: libc::pid_t,
    reported: &mut Option<Reported>,
) -> io::Result<()> {
    // SAFETY: `master` is an open descriptor.
    let pgid = unsafe { libc::tcgetpgrp(master) };
    if pgid <= 0 {
        return Ok(());
    }
    let now = Instant::now();
    let same = reported.as_ref().filter(|r| r.pgid == pgid);
    if same.is_some_and(|r| now.duration_since(r.since) >= RECHECK_WINDOW) {
        return Ok(());
    }
    let comm = std::fs::read(format!("/proc/{pgid}/comm")).unwrap_or_default();
    if same.is_some_and(|r| r.name == comm) {
        return Ok(());
    }
    let since = same.map_or(now, |r| r.since);
    let marker = foreground_marker(String::from_utf8_lossy(&comm).trim(), pgid == shell);
    *reported = Some(Reported {
        pgid,
        name: comm,
        since,
    });
    write_all(1, &marker)
}

/// Tells nuntio the working directory of the PTY's foreground process group (of the shell if
/// the group's leader is gone) if it differs from the last report. Nothing is written when
/// the terminal can't say. A removed directory is reported as unknown.
fn report_directory(
    master: RawFd,
    shell: libc::pid_t,
    reported: &mut Option<Vec<u8>>,
) -> io::Result<()> {
    // SAFETY: `master` is an open descriptor.
    let pgid = unsafe { libc::tcgetpgrp(master) };
    if pgid <= 0 {
        return Ok(());
    }
    let cwd = |pid: libc::pid_t| std::fs::read_link(format!("/proc/{pid}/cwd"));
    // An unreadable directory gives no bytes: reported as unknown.
    let dir = cwd(pgid)
        .or_else(|_| cwd(shell))
        .map(|p| p.into_os_string().into_vec())
        .unwrap_or_default();
    // The kernel marks a removed directory `/x (deleted)`: nothing can start there.
    let dir = if dir.ends_with(b" (deleted)") {
        Vec::new()
    } else {
        dir
    };
    if reported.as_ref() == Some(&dir) {
        return Ok(());
    }
    let marker = directory_marker(&dir);
    *reported = Some(dir);
    write_all(1, &marker)
}

/// Forwards between stdin/stdout and the master until the session ends. Returns the exit code.
fn event_loop(
    master: &OwnedFd,
    pid: libc::pid_t,
    wake: &OwnedFd,
    mut decoder: Decoder,
    mut pending: Vec<u8>,
) -> i32 {
    let master_fd = master.as_raw_fd();
    let mut written = 0;
    let mut reaped = None;
    let mut buf = vec![0u8; CHUNK];
    // The foreground process group last reported to nuntio, `None` before the first report.
    let mut reported = None;
    // The working directory last reported to nuntio.
    let mut reported_dir = None;
    if report_foreground(master_fd, pid, &mut reported).is_err()
        || report_directory(master_fd, pid, &mut reported_dir).is_err()
    {
        return hang_up(pid, 0);
    }
    let in_events = libc::POLLIN | libc::POLLHUP | libc::POLLERR;
    // Until when the loop looks at the foreground process group without being woken: the shell
    // may not own the terminal yet.
    let mut recheck = Some(Instant::now() + RECHECK_WINDOW);

    'session: loop {
        let want_write = written < pending.len();
        // The program doesn't read its input: stop reading ours, so the pipe
        // fills up and nuntio sees the back-pressure. Resizes behind the
        // paused input wait with it.
        let paused = pending.len() - written >= MAX_PENDING_INPUT;
        let mut fds = [
            libc::pollfd {
                fd: 0,
                events: if paused {
                    libc::POLLRDHUP
                } else {
                    libc::POLLIN
                },
                revents: 0,
            },
            libc::pollfd {
                fd: master_fd,
                events: libc::POLLIN | if want_write { libc::POLLOUT } else { 0 },
                revents: 0,
            },
            libc::pollfd {
                fd: wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // Without output a command the shell just started is only seen by looking again.
        let timeout = match recheck {
            Some(until) if Instant::now() < until => RECHECK_EVERY,
            _ => {
                recheck = None;
                -1
            }
        };
        // SAFETY: `fds` is a valid array of the given length.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            eprintln!("nuntio-wsl: poll failed: {}", io::Error::last_os_error());
            return hang_up(pid, 1);
        }
        if ready == 0
            && (report_foreground(master_fd, pid, &mut reported).is_err()
                || report_directory(master_fd, pid, &mut reported_dir).is_err())
        {
            return hang_up(pid, 0);
        }
        let [stdin_poll, master_poll, wake_poll] = fds;

        if paused {
            // nuntio closed the pipe or killed wsl.exe; the unread input is moot.
            if stdin_poll.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLRDHUP) != 0 {
                return hang_up(pid, 0);
            }
        } else if stdin_poll.revents & in_events != 0 {
            match read_fd(0, &mut buf) {
                // nuntio is gone or closed the pane.
                Ok(0) => return hang_up(pid, 0),
                Ok(n) => {
                    recheck = Some(Instant::now() + RECHECK_WINDOW);
                    let result = decoder.feed(&buf[..n], |frame| match frame {
                        Frame::Data(bytes) => pending.extend_from_slice(bytes),
                        Frame::Resize(size) => set_winsize(master_fd, size),
                    });
                    if let Err(err) = result {
                        eprintln!("nuntio-wsl: bad input: {err}");
                        return hang_up(pid, 2);
                    }
                }
                Err(err) if is_retry(&err) => {}
                Err(_) => return hang_up(pid, 0),
            }
        }

        let master_events = master_poll.revents;
        if master_events & libc::POLLOUT != 0 && written < pending.len() {
            match write_fd(master_fd, &pending[written..]) {
                Ok(n) => written += n,
                Err(err) if is_retry(&err) => {}
                Err(_) => break 'session,
            }
            if written == pending.len() {
                pending.clear();
                written = 0;
            }
        }
        if master_events & in_events != 0 {
            match read_fd(master_fd, &mut buf) {
                Ok(0) => break 'session,
                Ok(n) => {
                    recheck = Some(Instant::now() + RECHECK_WINDOW);
                    if report_foreground(master_fd, pid, &mut reported).is_err()
                        || report_directory(master_fd, pid, &mut reported_dir).is_err()
                        || write_all(1, &buf[..n]).is_err()
                    {
                        return hang_up(pid, 0);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    if master_events & (libc::POLLHUP | libc::POLLERR) != 0 {
                        break 'session;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                // EIO: the last slave descriptor is closed.
                Err(_) => break 'session,
            }
        }

        if wake_poll.revents & libc::POLLIN != 0 {
            // A job that ended with the shell's command may have changed the foreground.
            if report_foreground(master_fd, pid, &mut reported).is_err()
                || report_directory(master_fd, pid, &mut reported_dir).is_err()
            {
                return hang_up(pid, 0);
            }
            drain(wake.as_raw_fd());
            let mut status = 0;
            // SAFETY: `status` is a valid out-pointer.
            if unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } == pid {
                reaped = Some(status);
                // Output the shell wrote before it exited, even if background jobs still
                // hold the PTY open, but not forever.
                let mut left = POST_EXIT_DRAIN;
                while left > 0 {
                    match read_fd(master_fd, &mut buf[..CHUNK.min(left)]) {
                        Ok(n) if n > 0 && write_all(1, &buf[..n]).is_ok() => left -= n,
                        _ => break,
                    }
                }
                break 'session;
            }
        }
    }

    let status = reaped.or_else(|| wait_for_exit(pid, wake));
    match status.map(ExitStatus::from_raw) {
        Some(status) => status
            .code()
            .or_else(|| status.signal().map(|signal| 128 + signal))
            .unwrap_or(1),
        None => 1,
    }
}

/// Waits for the program after its terminal closed, still reading stdin: nuntio closing the
/// pane hangs up the session. `None` if nuntio went away first.
fn wait_for_exit(pid: libc::pid_t, wake: &OwnedFd) -> Option<libc::c_int> {
    loop {
        let mut status = 0;
        // SAFETY: `status` is a valid out-pointer.
        if unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } == pid {
            return Some(status);
        }
        let mut fds = [
            libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: `fds` is a valid array of the given length.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) } < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return None;
        }
        let [stdin_poll, wake_poll] = fds;
        if wake_poll.revents & libc::POLLIN != 0 {
            drain(wake.as_raw_fd());
        }
        if stdin_poll.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            // There is no terminal left to write the input to.
            match read_fd(0, &mut [0u8; 4096]) {
                Ok(0) => {
                    hang_up(pid, 0);
                    return None;
                }
                Err(err) if !is_retry(&err) => {
                    hang_up(pid, 0);
                    return None;
                }
                _ => {}
            }
        }
    }
}

/// Hangs up the shell's process group and returns `code`.
fn hang_up(pid: libc::pid_t, code: i32) -> i32 {
    // SAFETY: plain syscall; the child is a session leader, so its pid is its group id.
    unsafe { libc::kill(-pid, libc::SIGHUP) };
    code
}

fn set_winsize(master: RawFd, size: Size) {
    let ws = winsize(size);
    // SAFETY: `ws` is a valid winsize for TIOCSWINSZ.
    unsafe { libc::ioctl(master, libc::TIOCSWINSZ as _, &ws) };
}

/// Empties a non-blocking descriptor.
fn drain(fd: RawFd) {
    let mut buf = [0u8; 64];
    while matches!(read_fd(fd, &mut buf), Ok(n) if n > 0) {}
}

fn is_retry(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

fn cvt(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

/// `read(2)` that retries on EINTR.
fn read_fd(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        // SAFETY: the pointer and length describe `buf`.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// `write(2)` that retries on EINTR.
fn write_fd(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    loop {
        // SAFETY: the pointer and length describe `buf`.
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Writes everything, unbuffered. Waits if the descriptor is non-blocking and full.
fn write_all(fd: RawFd, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        match write_fd(fd, buf) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => buf = &buf[n..],
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: one valid pollfd.
                unsafe { libc::poll(&mut pfd, 1, -1) };
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}
