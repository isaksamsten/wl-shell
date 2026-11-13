//! The desktop's color scheme for a daemon: read once, then kept current by
//! a monitor process (`gdbus monitor` on the settings portal, else
//! `gsettings monitor`) whose output the event loop reads, so shows never
//! have to ask.

use std::cell::Cell;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;

use calloop::{Interest, LoopHandle, Mode, PostAction, generic::Generic};

use crate::appearance::Scheme;

/// Longer lines are not the ones we look for.
const MAX_LINE: usize = 1 << 16;

#[derive(Default)]
pub struct DesktopScheme {
    state: State,
}

#[derive(Default)]
enum State {
    /// Not asked for yet, or the monitor exited.
    #[default]
    Idle,
    Watching(Watch),
    /// Neither the portal nor gsettings answered.
    Unavailable,
}

impl DesktopScheme {
    /// The desktop's scheme; light if it has none. The first call starts
    /// watching (as does the next one after the monitor exited).
    pub fn get<D: 'static>(
        &mut self,
        handle: &LoopHandle<'static, D>,
        warn: &mut dyn FnMut(String),
    ) -> Scheme {
        if let State::Watching(w) = &self.state
            && w.current.get().is_none()
        {
            // Dropping the watch reaps the monitor.
            self.state = State::Idle;
        }
        if let State::Idle = self.state {
            self.state = match Watch::start(handle) {
                Some(watch) => State::Watching(watch),
                None => {
                    warn("cannot read the desktop's color scheme; using light".into());
                    State::Unavailable
                }
            };
        }
        match &self.state {
            State::Watching(w) => w.current.get().unwrap_or(Scheme::Light),
            _ => Scheme::Light,
        }
    }

    /// Whether the monitor reported another scheme since the last call.
    pub fn take_changed(&self) -> bool {
        match &self.state {
            State::Watching(w) => w.changed.replace(false),
            _ => false,
        }
    }
}

/// A running monitor; `current` follows its output.
struct Watch {
    child: Child,
    /// None once the monitor's output ended.
    current: Rc<Cell<Option<Scheme>>>,
    /// Set when `current` changes to another scheme.
    changed: Rc<Cell<bool>>,
}

impl Watch {
    /// Watch the first source that answers.
    fn start<D: 'static>(handle: &LoopHandle<'static, D>) -> Option<Self> {
        [Source::Portal, Source::Gsettings]
            .into_iter()
            .find_map(|source| Self::start_source(source, handle))
    }

    fn start_source<D: 'static>(source: Source, handle: &LoopHandle<'static, D>) -> Option<Self> {
        // Monitor first, so a change right after the read is not missed.
        let mut child = source.spawn().ok()?;
        let watch = |child: &mut Child| {
            let dark = source.read()?;
            let stdout = File::from(OwnedFd::from(child.stdout.take()?));
            let current = Rc::new(Cell::new(Some(Scheme::new(dark))));
            let changed = Rc::new(Cell::new(false));
            let (cell, flag) = (current.clone(), changed.clone());
            let mut buf = Vec::new();
            let generic = Generic::new(stdout, Interest::READ, Mode::Level);
            handle
                .insert_source(generic, move |_, stdout, _| {
                    // Level-triggered: a single read per wakeup never blocks.
                    let mut chunk = [0; 4096];
                    let n = match (&**stdout).read(&mut chunk) {
                        Err(e) if e.kind() == ErrorKind::Interrupted => {
                            return Ok(PostAction::Continue);
                        }
                        Ok(0) | Err(_) => {
                            wl_shell_log::warn!("color scheme monitor exited");
                            cell.set(None);
                            return Ok(PostAction::Remove);
                        }
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&chunk[..n]);
                    while let Some(end) = buf.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = buf.drain(..=end).collect();
                        if let Some(dark) = source.parse(&String::from_utf8_lossy(&line)) {
                            let new = Some(Scheme::new(dark));
                            if cell.replace(new) != new {
                                flag.set(true);
                            }
                        }
                    }
                    if buf.len() > MAX_LINE {
                        buf.clear();
                    }
                    Ok(PostAction::Continue)
                })
                .ok()?;
            Some((current, changed))
        };
        match watch(&mut child) {
            Some((current, changed)) => Some(Self {
                child,
                current,
                changed,
            }),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                None
            }
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Clone, Copy)]
enum Source {
    Portal,
    Gsettings,
}

impl Source {
    fn spawn(self) -> std::io::Result<Child> {
        let mut cmd = match self {
            Source::Portal => {
                let mut cmd = Command::new("gdbus");
                cmd.args([
                    "monitor",
                    "--session",
                    "--dest=org.freedesktop.portal.Desktop",
                    "--object-path=/org/freedesktop/portal/desktop",
                ]);
                cmd
            }
            Source::Gsettings => {
                let mut cmd = Command::new("gsettings");
                cmd.args(["monitor", "org.gnome.desktop.interface", "color-scheme"]);
                cmd
            }
        };
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Do not outlive the daemon, however it goes.
        // SAFETY: prctl is async-signal-safe and touches no memory.
        unsafe {
            cmd.pre_exec(|| {
                rustix::process::set_parent_process_death_signal(Some(
                    rustix::process::Signal::TERM,
                ))
                .map_err(Into::into)
            });
        }
        cmd.spawn()
    }

    /// Whether the desktop prefers dark now; None if this source is not there.
    fn read(self) -> Option<bool> {
        match self {
            Source::Portal => portal_prefers_dark(),
            Source::Gsettings => gsettings_prefers_dark(),
        }
    }

    /// Whether a monitor `line` says the desktop now prefers dark; None if
    /// it is about something else.
    fn parse(self, line: &str) -> Option<bool> {
        match self {
            Source::Portal => line
                .contains("SettingChanged ('org.freedesktop.appearance', 'color-scheme',")
                .then(|| parse_portal(line))
                .flatten(),
            Source::Gsettings => line.strip_prefix("color-scheme:").map(parse_gsettings),
        }
    }
}

/// Run `cmd` with `args`; its stdout if it succeeds.
fn output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    String::from_utf8(out.stdout).ok()
}

/// `org.freedesktop.appearance color-scheme`: 0 no preference, 1 dark, 2 light.
pub fn portal_prefers_dark() -> Option<bool> {
    let out = output(
        "gdbus",
        &[
            "call",
            "--session",
            "--timeout=1",
            "--dest=org.freedesktop.portal.Desktop",
            "--object-path=/org/freedesktop/portal/desktop",
            "--method=org.freedesktop.portal.Settings.ReadOne",
            "org.freedesktop.appearance",
            "color-scheme",
        ],
    )?;
    parse_portal(&out)
}

/// `(<uint32 1>,)`, or `(<<uint32 1>>,)` from the older `Read`; also the
/// `SettingChanged` lines of `gdbus monitor`.
pub fn parse_portal(out: &str) -> Option<bool> {
    let rest = &out[out.find("uint32 ")? + "uint32 ".len()..];
    let n: u32 = rest[..rest.find(|c: char| !c.is_ascii_digit())?]
        .parse()
        .ok()?;
    Some(n == 1)
}

pub fn gsettings_prefers_dark() -> Option<bool> {
    let out = output(
        "gsettings",
        &["get", "org.gnome.desktop.interface", "color-scheme"],
    )?;
    Some(parse_gsettings(&out))
}

/// `'prefer-dark'`, from `gsettings get`.
pub fn parse_gsettings(value: &str) -> bool {
    value.trim().trim_matches('\'') == "prefer-dark"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal() {
        assert_eq!(parse_portal("(<uint32 1>,)\n"), Some(true));
        assert_eq!(parse_portal("(<<uint32 2>>,)\n"), Some(false));
        assert_eq!(parse_portal("(<uint32 0>,)\n"), Some(false));
        assert_eq!(parse_portal("()\n"), None);
    }

    #[test]
    fn monitor_lines() {
        let portal = |s: &str| Source::Portal.parse(s);
        let p = "/org/freedesktop/portal/desktop: org.freedesktop.portal.Settings.SettingChanged";
        assert_eq!(
            portal(&format!(
                "{p} ('org.freedesktop.appearance', 'color-scheme', <uint32 1>)\n"
            )),
            Some(true)
        );
        assert_eq!(
            portal(&format!(
                "{p} ('org.freedesktop.appearance', 'color-scheme', <uint32 2>)\n"
            )),
            Some(false)
        );
        assert_eq!(
            portal(&format!(
                "{p} ('org.gnome.desktop.interface', 'color-scheme', <'prefer-dark'>)\n"
            )),
            None
        );
        assert_eq!(
            portal("The name org.freedesktop.portal.Desktop is owned by :1.1958\n"),
            None
        );

        let gsettings = |s: &str| Source::Gsettings.parse(s);
        assert_eq!(gsettings("color-scheme: 'prefer-dark'\n"), Some(true));
        assert_eq!(gsettings("color-scheme: 'default'\n"), Some(false));
    }
}
