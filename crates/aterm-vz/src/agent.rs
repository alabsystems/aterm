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
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "vz_lifecycle",
        action = "ExecExit",
        project = "conformance_lifecycle::project"
    )
)]
pub fn run(fd: OwnedFd, exec: &Exec, span: Duration) -> Result<Output, String> {
    let mut stream = UnixStream::from(fd);
    // XNU refuses every option on a socket that can neither send nor receive (EINVAL): the
    // agent closed the connection before the command went.
    stream.set_read_timeout(Some(span)).map_err(|e| {
        if e.raw_os_error() == Some(libc::EINVAL) {
            "the agent closed the command connection before the command".to_owned()
        } else {
            format!("the command connection: {e}")
        }
    })?;
    wire::write_exec(&mut stream, exec).map_err(|e| format!("Exec: {e}"))?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    loop {
        match wire::read_frame(&mut stream) {
            Ok(Some(Frame::Out(b))) => out.extend_from_slice(&b),
            Ok(Some(Frame::Err(b))) => err.extend_from_slice(&b),
            Ok(Some(Frame::Exit(exit))) => {
                #[cfg(any(test, debug_assertions))]
                if crate::fault::armed(crate::fault::Fault::DropExit) {
                    continue;
                }
                return Ok(Output { exit, out, err });
            }
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
#[path = "agent_tests.rs"]
mod tests;
