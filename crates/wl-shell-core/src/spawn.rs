//! Running commands detached: activating windows, launching apps, menu
//! actions. The program never waits for them.
//!
//! A command is forked twice: the first child starts a new session
//! (`setsid`), forks the command and exits at once, so the command is
//! re-parented to init (or the subreaper) and never becomes our zombie,
//! and stopping or restarting the program doesn't take it along.
//!
//! Short commands (`Output::Prefixed`) have their stdout and stderr read
//! and passed on to ours as warnings, prefixed. Launched apps
//! (`Output::Inherit`) write to our stderr directly instead, since they
//! may outlive us: a pipe we read would break when we exit, and an app
//! writing to it would get SIGPIPE.

use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use calloop::LoopHandle;

use crate::stream::watch_prefixed;

/// Where a command's output goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// To our stderr, line by line, prefixed with this (e.g. the config
    /// key the command came from).
    Prefixed(String),
    /// Straight to our stdout and stderr.
    Inherit,
}

/// Run `command` with `sh -c`, detached.
pub fn shell<D: 'static>(
    handle: &LoopHandle<'static, D>,
    command: &str,
    output: Output,
) -> io::Result<()> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);
    run(handle, cmd, output)
}

/// Run `argv` (no shell), detached.
pub fn argv<D: 'static>(
    handle: &LoopHandle<'static, D>,
    argv: &[String],
    output: Output,
) -> io::Result<()> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::other("empty command"))?;
    let mut cmd = Command::new(program);
    cmd.args(args);
    run(handle, cmd, output)
}

fn run<D: 'static>(
    handle: &LoopHandle<'static, D>,
    mut cmd: Command,
    output: Output,
) -> io::Result<()> {
    cmd.stdin(Stdio::null());
    if let Output::Prefixed(_) = output {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    // SAFETY: fork, setsid and _exit are async-signal-safe, and nothing
    // here allocates or takes locks.
    unsafe {
        cmd.pre_exec(|| {
            match libc::fork() {
                -1 => return Err(io::Error::last_os_error()),
                0 => {}
                // The first child: done. Its exit is what `wait` sees.
                _ => libc::_exit(0),
            }
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // Returns once the command was executed (or failed to be): until then
    // it holds the pipe std reports exec errors through.
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().map(|s| File::from(OwnedFd::from(s)));
    let stderr = child.stderr.take().map(|s| File::from(OwnedFd::from(s)));
    // The first child, which exited right after forking.
    child.wait()?;
    if let Output::Prefixed(name) = output {
        for pipe in [stdout, stderr].into_iter().flatten() {
            watch_prefixed(pipe, name.clone(), handle);
        }
    }
    Ok(())
}

/// `s` quoted for `sh`: as is if it only has safe characters, else in
/// single quotes.
pub fn quote(s: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./:,+=@%^".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `argv` as one `sh` command line.
pub fn join(argv: &[String]) -> String {
    (argv.iter())
        .map(|a| quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `template` with each `{key}` of `values` replaced by its value, as is
/// (quote values first where needed). Other braces stay.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    'outer: while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        for (key, value) in values {
            if let Some(after) = rest
                .strip_prefix('{')
                .and_then(|r| r.strip_prefix(*key))
                .and_then(|r| r.strip_prefix('}'))
            {
                out.push_str(value);
                rest = after;
                continue 'outer;
            }
        }
        out.push('{');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

/// Whether `template` uses `{key}`.
pub fn uses(template: &str, key: &str) -> bool {
    template.contains(&format!("{{{key}}}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn quoting() {
        assert_eq!(quote("abc-1.2"), "abc-1.2");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote("$HOME"), "'$HOME'");
        assert_eq!(
            join(&["echo".into(), "a b".into(), "c".into()]),
            "echo 'a b' c"
        );
    }

    #[test]
    fn filling() {
        let values = [("id", "'4 2'"), ("app-id", "foot")];
        assert_eq!(
            fill("focus --id {id} {app-id}", &values),
            "focus --id '4 2' foot"
        );
        assert_eq!(fill("{x} {id}{id}", &values), "{x} '4 2''4 2'");
        assert_eq!(fill("awk '{print}' {", &values), "awk '{print}' {");
        assert_eq!(fill("{{id}}", &values), "{'4 2'}");
        assert!(uses("a {id}", "id") && !uses("a {ids}", "id"));
    }

    #[test]
    fn runs_detached() {
        let dir = std::env::temp_dir().join(format!("wl-shell-spawn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("out");
        let event_loop: calloop::EventLoop<()> = calloop::EventLoop::try_new().unwrap();
        let command = format!("echo $$ > {}", quote(file.to_str().unwrap()));
        shell(&event_loop.handle(), &command, Output::Inherit).unwrap();
        let argv = ["/nonexistent/program".to_string()];
        assert!(super::argv(&event_loop.handle(), &argv, Output::Inherit).is_err());
        let mut pid = String::new();
        for _ in 0..100 {
            pid = std::fs::read_to_string(&file).unwrap_or_default();
            if pid.ends_with('\n') {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid: i32 = pid.trim().parse().unwrap();
        // In a session of its own, so not our child or in our group.
        assert_ne!(pid, std::process::id() as i32);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
