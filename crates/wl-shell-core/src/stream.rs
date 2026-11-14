//! Line streams: a command run with `sh -c` for as long as the program
//! runs, printing events one per line, e.g. the `mru-event-stream` (the ID
//! of each window that gains focus) or wl-dock's `output-event-stream`.
//! Each line goes to a callback.
//!
//! The command runs in its own process group, so stopping it also stops
//! the rest of a pipeline. Its stdout is read without blocking. If it
//! exits, it is restarted with backoff (1 s, doubling to 30 s, reset once
//! it ran for a minute). Its stderr goes to the program's, prefixed with
//! the stream's name.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use calloop::{
    Interest, LoopHandle, Mode, PostAction, RegistrationToken,
    generic::Generic,
    timer::{TimeoutAction, Timer},
};

use crate::shell::{Program, Shell};

/// Longer lines are skipped.
const MAX_LINE: usize = 1 << 16;
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// A stream that ran this long starts over at `FIRST_BACKOFF`.
const STABLE: Duration = Duration::from_secs(60);

type Handle<P> = LoopHandle<'static, Shell<P>>;

/// How a stream reaches its owner. Plain functions, so the stream can
/// call them from event loop callbacks, which get the whole shell.
pub struct Hooks<P> {
    /// Where the stream lives in the shell.
    pub locate: fn(&mut Shell<P>) -> &mut LineStream<P>,
    /// A line of output, without its newline.
    pub line: fn(&mut Shell<P>, &str),
    /// The stream exited, or started again after exiting: see
    /// `LineStream::running`.
    pub changed: fn(&mut Shell<P>),
}

impl<P> Clone for Hooks<P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P> Copy for Hooks<P> {}

pub struct LineStream<P> {
    /// What messages about the stream start with (the config key).
    name: &'static str,
    hooks: Hooks<P>,
    /// The command from the config; None: no stream.
    command: Option<String>,
    running: Option<Running>,
    /// The pending restart after the stream exited.
    restart: Option<RegistrationToken>,
    /// The last wait before a restart; zero until the stream first exits.
    backoff: Duration,
}

impl<P: Program> LineStream<P> {
    /// A stream named `name` (for messages), not running yet.
    pub fn new(name: &'static str, hooks: Hooks<P>) -> Self {
        Self {
            name,
            hooks,
            command: None,
            running: None,
            restart: None,
            backoff: Duration::ZERO,
        }
    }

    /// Whether the command runs: configured, and not waiting to restart.
    pub fn running(&self) -> bool {
        self.running.is_some()
    }

    /// Whether a command is configured.
    pub fn configured(&self) -> bool {
        self.command.is_some()
    }

    /// Run `command` (from the config): start it, restart it if it
    /// changed, or stop it if None. Nothing happens if it's the same.
    pub fn configure(&mut self, command: Option<&str>, handle: &Handle<P>) {
        let command = command.map(str::trim).filter(|c| !c.is_empty());
        if command == self.command.as_deref() {
            return;
        }
        if self.command.is_some() {
            self.warn(format_args!("the config changed; restarting"));
        }
        self.stop(handle);
        self.command = command.map(Into::into);
        self.backoff = Duration::ZERO;
        self.start(handle);
    }

    fn warn(&self, message: std::fmt::Arguments) {
        wl_shell_log::warn!("{}: {message}", self.name);
    }

    fn stop(&mut self, handle: &Handle<P>) {
        if let Some(token) = self.restart.take() {
            handle.remove(token);
        }
        if let Some(mut running) = self.running.take()
            && let Some(token) = running.stdout.take()
        {
            handle.remove(token);
        }
        // Dropping it killed it.
    }

    fn start(&mut self, handle: &Handle<P>) {
        let Some(command) = &self.command else {
            return;
        };
        match Running::spawn(command, self.name, self.hooks, handle) {
            Ok(running) => self.running = Some(running),
            Err(e) => {
                self.warn(format_args!("failed to start: {e}"));
                self.schedule_restart(Duration::ZERO, handle);
            }
        }
    }

    /// The stream's output ended (its source removes itself).
    fn exited(&mut self, handle: &Handle<P>) {
        let Some(mut running) = self.running.take() else {
            return;
        };
        let ran = running.started.elapsed();
        match running.stop().and_then(|s| s.code()) {
            Some(code) => self.warn(format_args!("exited with status {code}")),
            None => self.warn(format_args!("closed its output")),
        }
        self.schedule_restart(ran, handle);
    }

    fn schedule_restart(&mut self, ran: Duration, handle: &Handle<P>) {
        self.backoff = backoff(self.backoff, ran);
        self.warn(format_args!("restarting in {} s", self.backoff.as_secs()));
        let timer = Timer::from_duration(self.backoff);
        let hooks = self.hooks;
        let token = handle.insert_source(timer, move |_, _, shell| {
            let handle = shell.core.loop_handle.clone();
            let stream = (hooks.locate)(shell);
            stream.restart = None;
            stream.start(&handle);
            if stream.running() {
                (hooks.changed)(shell);
            }
            TimeoutAction::Drop
        });
        match token {
            Ok(token) => self.restart = Some(token),
            Err(e) => self.warn(format_args!("cannot restart: {e}")),
        }
    }
}

/// The wait before restarting a stream that ran for `ran`, after waiting
/// `prev` the last time (zero: never).
fn backoff(prev: Duration, ran: Duration) -> Duration {
    if prev.is_zero() || ran >= STABLE {
        FIRST_BACKOFF
    } else {
        (prev * 2).min(MAX_BACKOFF)
    }
}

/// The running command. Dropping it kills its process group.
struct Running {
    child: Child,
    started: Instant,
    /// The stdout source, once registered.
    stdout: Option<RegistrationToken>,
    /// Set once reaped.
    status: Option<ExitStatus>,
}

impl Running {
    fn spawn<P: Program>(
        command: &str,
        name: &'static str,
        hooks: Hooks<P>,
        handle: &Handle<P>,
    ) -> std::io::Result<Self> {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        // A backstop for a program killed by a signal; normally `stop`
        // kills the whole group.
        // SAFETY: prctl is async-signal-safe and touches no memory.
        unsafe {
            cmd.pre_exec(|| {
                rustix::process::set_parent_process_death_signal(Some(
                    rustix::process::Signal::TERM,
                ))
                .map_err(Into::into)
            });
        }
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().map(|s| File::from(OwnedFd::from(s)));
        let stderr = child.stderr.take().map(|s| File::from(OwnedFd::from(s)));
        // On error, dropping `running` kills the child.
        let mut running = Running {
            child,
            started: Instant::now(),
            stdout: None,
            status: None,
        };
        let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
            return Err(std::io::Error::other("no pipes"));
        };
        running.stdout = Some(watch_stdout(stdout, hooks, handle)?);
        watch_prefixed(stderr, name, handle);
        Ok(running)
    }

    /// Kill the process group and reap the command; its exit status.
    fn stop(&mut self) -> Option<ExitStatus> {
        if self.status.is_none() {
            // Before reaping, the group's ID can't have been reused.
            if let Some(pid) = rustix::process::Pid::from_raw(self.child.id() as i32) {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
            self.status = self.child.wait().ok();
        }
        self.status
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Pass each line of stdout to the stream's owner. At the end of the
/// output, the stream counts as exited.
fn watch_stdout<P: Program>(
    stdout: File,
    hooks: Hooks<P>,
    handle: &Handle<P>,
) -> std::io::Result<RegistrationToken> {
    let mut lines = Lines::default();
    let source = Generic::new(stdout, Interest::READ, Mode::Level);
    handle
        .insert_source(source, move |_, stdout, shell| {
            // Level-triggered: a single read per wakeup never blocks.
            let mut chunk = [0; 4096];
            match (&**stdout).read(&mut chunk) {
                Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {}
                Ok(0) | Err(_) => {
                    let handle = shell.core.loop_handle.clone();
                    (hooks.locate)(shell).exited(&handle);
                    (hooks.changed)(shell);
                    return Ok(PostAction::Remove);
                }
                Ok(n) => lines.push(&chunk[..n], |line| (hooks.line)(shell, line)),
            }
            Ok(PostAction::Continue)
        })
        .map_err(|e| std::io::Error::other(e.to_string()))
}

/// Pass output on as warnings, prefixed with `name`. Ends by itself once
/// every process holding the pipe is gone.
pub(crate) fn watch_prefixed<D: 'static>(
    pipe: File,
    name: impl std::fmt::Display + Clone + 'static,
    handle: &LoopHandle<'static, D>,
) {
    let mut lines = Lines::default();
    let prefix = name.clone();
    let source = Generic::new(pipe, Interest::READ, Mode::Level);
    let result = handle.insert_source(source, move |_, pipe, _| {
        let mut chunk = [0; 4096];
        match (&**pipe).read(&mut chunk) {
            Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {}
            Ok(0) | Err(_) => return Ok(PostAction::Remove),
            Ok(n) => lines.push(&chunk[..n], |line| {
                let line = line.trim_end();
                if !line.is_empty() {
                    wl_shell_log::warn!("{prefix}: {line}");
                }
            }),
        }
        Ok(PostAction::Continue)
    });
    if let Err(e) = result {
        wl_shell_log::warn!("cannot read the output of {name}: {e}");
    }
}

/// Splits output into lines, keeping a partial line for the next chunk.
/// Lines longer than `MAX_LINE` are dropped whole.
#[derive(Default)]
struct Lines {
    buf: Vec<u8>,
    /// Inside an overlong line: skip to its end.
    skipping: bool,
}

impl Lines {
    fn push(&mut self, mut data: &[u8], mut each: impl FnMut(&str)) {
        while let Some(end) = data.iter().position(|&b| b == b'\n') {
            let skip = std::mem::take(&mut self.skipping) || self.buf.len() + end > MAX_LINE;
            if !skip {
                self.buf.extend_from_slice(&data[..end]);
                each(&String::from_utf8_lossy(&self.buf));
            }
            self.buf.clear();
            data = &data[end + 1..];
        }
        if !self.skipping {
            self.buf.extend_from_slice(data);
            if self.buf.len() > MAX_LINE {
                self.buf.clear();
                self.skipping = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(chunks: &[&[u8]]) -> Vec<String> {
        let mut lines = Lines::default();
        let mut out = Vec::new();
        for chunk in chunks {
            lines.push(chunk, |l| out.push(l.to_string()));
        }
        out
    }

    #[test]
    fn partial_lines_are_kept() {
        assert_eq!(split(&[b"1\n2", b"3\n\n4"]), ["1", "23", ""]);
        assert_eq!(split(&[b"a", b"b", b"c\n"]), ["abc"]);
    }

    #[test]
    fn overlong_lines_are_dropped() {
        let long = vec![b'x'; MAX_LINE + 1];
        assert_eq!(split(&[b"1\n", &long, b"yy\n2\n"]), ["1", "2"]);
        let mut whole = long.clone();
        whole.extend_from_slice(b"\n3\n");
        assert_eq!(split(&[&whole]), ["3"]);
    }

    #[test]
    fn backoff_doubles_then_resets() {
        let s = Duration::from_secs;
        let quick = s(2);
        let mut wait = Duration::ZERO;
        let mut waits = Vec::new();
        for _ in 0..7 {
            wait = backoff(wait, quick);
            waits.push(wait.as_secs());
        }
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30]);
        assert_eq!(backoff(wait, STABLE), s(1));
    }
}
