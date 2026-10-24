// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The agent's command client (`docs/DESIGN-vz-guest-2026-10-23.md` §6): one request on one
//! command connection, whose descriptor the helper passed ([`crate::client::Session::connect`]).
//! The lab speaks to the agent directly; the helper never touches command output.
//!
//! Each read waits at most the caller's span, the lane row's own no-progress detector: a
//! command that prints nothing for that long ends the wait, and the caller kills its group
//! (`Kill`) or closes the connection, which SIGKILLs it (§6).

use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use aterm_vz_agent::wire::{self, Exec, Exit, Frame, Stdin};

/// What a command did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// How it ended.
    pub exit: Exit,
    /// Its stdout (and its stderr, when merged).
    pub out: Vec<u8>,
    /// Its stderr.
    pub err: Vec<u8>,
}

/// The lane user's `Exec` of `argv` in `cwd` with exactly `env`, stdin `/dev/null`, its
/// clock the host's now.
#[must_use]
pub fn exec_of(argv: &[&str], cwd: &str, env: &[(&str, &str)]) -> Exec {
    Exec {
        argv: argv.iter().map(|a| a.as_bytes().to_vec()).collect(),
        cwd: cwd.as_bytes().to_vec(),
        env: env
            .iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
            .collect(),
        stdin: Stdin::Null,
        merge: false,
        clock_unix_ns: aterm_time::since_epoch()
            .map_or(0, |t| u64::try_from(t.as_nanos()).unwrap_or(u64::MAX)),
    }
}

/// Run `exec` on the connection `fd` and collect its output, each read waiting at most
/// `span`. The connection is closed on return.
///
/// # Errors
/// The connection failed, the agent sent a frame no command sends, or a read waited past
/// `span` (the words say which).
pub fn run(fd: OwnedFd, exec: &Exec, span: Duration) -> Result<Output, String> {
    let mut stream = UnixStream::from(fd);
    stream
        .set_read_timeout(Some(span))
        .map_err(|e| format!("the command connection: {e}"))?;
    wire::write_frame(&mut stream, &Frame::Exec(exec.clone())).map_err(|e| format!("Exec: {e}"))?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    loop {
        match wire::read_frame(&mut stream) {
            Ok(Some(Frame::Out(b))) => out.extend_from_slice(&b),
            Ok(Some(Frame::Err(b))) => err.extend_from_slice(&b),
            Ok(Some(Frame::Exit(exit))) => return Ok(Output { exit, out, err }),
            Ok(Some(other)) => {
                return Err(format!(
                    "the agent sent a frame of kind {:#04x} on a command connection",
                    other.kind()
                ));
            }
            Ok(None) => {
                return Err("the agent closed the command connection before its Exit".into());
            }
            Err(wire::WireError::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(format!(
                    "the command sent nothing for {} s",
                    span.as_secs_f64()
                ));
            }
            Err(e) => return Err(format!("the command connection: {e}")),
        }
    }
}

/// Ask the agent on `fd` to power the guest off (`systemctl poweroff`). The helper sees the
/// VM stop.
///
/// # Errors
/// The connection failed.
pub fn poweroff(fd: OwnedFd) -> Result<(), String> {
    let mut stream = UnixStream::from(fd);
    wire::write_frame(&mut stream, &Frame::Poweroff).map_err(|e| format!("Poweroff: {e}"))?;
    stream.flush().map_err(|e| format!("Poweroff: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_runs_on_its_connection_and_ends_with_its_exit() {
        let (ours, mut guest) = UnixStream::pair().unwrap();
        let agent = std::thread::spawn(move || {
            let Some(Frame::Exec(exec)) = wire::read_frame(&mut guest).unwrap() else {
                panic!("no Exec");
            };
            assert_eq!(exec.argv, [b"uname".to_vec(), b"-r".to_vec()]);
            assert_eq!(exec.env, [(b"PATH".to_vec(), b"/usr/bin".to_vec())]);
            wire::write_frame(&mut guest, &Frame::Out(b"6.8.0".to_vec())).unwrap();
            wire::write_frame(&mut guest, &Frame::Err(b"warn".to_vec())).unwrap();
            wire::write_frame(&mut guest, &Frame::Out(b"-142-generic\n".to_vec())).unwrap();
            wire::write_frame(&mut guest, &Frame::Exit(Exit::Exited(0))).unwrap();
        });
        let exec = exec_of(&["uname", "-r"], "/", &[("PATH", "/usr/bin")]);
        let got = run(OwnedFd::from(ours), &exec, Duration::from_secs(60)).unwrap();
        agent.join().unwrap();
        assert_eq!(
            got,
            Output {
                exit: Exit::Exited(0),
                out: b"6.8.0-142-generic\n".to_vec(),
                err: b"warn".to_vec(),
            }
        );
    }

    #[test]
    fn a_connection_that_ends_before_its_exit_or_stays_silent_is_named() {
        let (ours, guest) = UnixStream::pair().unwrap();
        drop(guest);
        let exec = exec_of(&["true"], "/", &[]);
        let e = run(OwnedFd::from(ours), &exec, Duration::from_secs(60)).unwrap_err();
        // macOS refuses even the read timeout on a socket whose peer is gone (EINVAL).
        assert!(
            e.starts_with("the command connection")
                || e.contains("Exec")
                || e.contains("before its Exit"),
            "{e}"
        );
        let (ours, _guest) = UnixStream::pair().unwrap();
        let e = run(OwnedFd::from(ours), &exec, Duration::from_millis(20)).unwrap_err();
        assert!(e.contains("sent nothing for 0.02 s"), "{e}");
    }
}
