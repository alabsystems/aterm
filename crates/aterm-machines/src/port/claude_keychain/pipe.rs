// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One deadline and wiping capture for the noninteractive Keychain child.

use super::{CHILD, MAX_BYTES, ReadError};
use aterm_secrets::SecretBytes;
use aterm_uds::spawnfd::{Child, Stdio};
use std::io::{ErrorKind, Read as _};
use std::os::fd::AsRawFd as _;
use std::path::Path;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

struct OwnedChild(Child, bool);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        // The child never spawns descendants. SIGKILL is sent before handing a
        // kernel-delayed reap to Child's owned reaper; no timed-out read continues.
        if self.1 {
            aterm_uds::deadline::kill_within(&mut self.0, Duration::ZERO);
        }
    }
}

pub(super) fn read(
    program: &Path,
    stage: &Path,
    deadline: Instant,
) -> Result<Option<SecretBytes>, ReadError> {
    read_mode(program, stage, deadline, CHILD)
}

pub(super) fn read_mode(
    program: &Path,
    stage: &Path,
    deadline: Instant,
    mode: &str,
) -> Result<Option<SecretBytes>, ReadError> {
    let mut command = aterm_uds::spawnfd::command(program);
    command.arg(mode).arg(stage).env_clear();
    // Claude's MI() reads USER first; otherwise it asks the operating system.
    // Preserve this one non-secret input without inheriting unrelated credentials.
    if let Some(user) = std::env::var_os("USER") {
        command.env("USER", user);
    }
    capture(&mut command, || deadline, || {})
}

fn capture(
    command: &mut aterm_uds::spawnfd::Command,
    until: impl Fn() -> Instant,
    mut after_read: impl FnMut(),
) -> Result<Option<SecretBytes>, ReadError> {
    if Instant::now() >= until() {
        return Err(ReadError::TimedOut);
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ReadError::Unavailable)?;
    let mut child = OwnedChild(child, true);
    let mut stdout = child.0.stdout.take().ok_or(ReadError::Unavailable)?;
    let fd = stdout.as_raw_fd();
    // SAFETY: stdout owns this live pipe descriptor; fcntl only changes its flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    // SAFETY: fd is still stdout's live descriptor, and flags came from F_GETFL.
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(ReadError::Unavailable);
    }
    // Allocate the bound once: growing a Vec could free an unwiped old allocation.
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_BYTES));
    let mut chunk = Zeroizing::new([0_u8; 8192]);
    let mut eof = false;
    loop {
        let deadline = until();
        if Instant::now() >= deadline {
            return Err(ReadError::TimedOut);
        }
        if !eof {
            match stdout.read(&mut *chunk) {
                Ok(0) => eof = true,
                Ok(n) => {
                    if bytes.len().saturating_add(n) > MAX_BYTES {
                        return Err(ReadError::TooLarge);
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    after_read();
                    continue;
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {}
                Err(_) => return Err(ReadError::Unavailable),
            }
        }
        let status = child.0.try_wait().map_err(|_| ReadError::Unavailable)?;
        if status.is_some() {
            child.1 = false;
        }
        if let Some(status) = status
            && eof
        {
            return match status.code() {
                Some(0) => Ok(Some(SecretBytes::new(std::mem::take(&mut *bytes)))),
                Some(10) if bytes.is_empty() => Ok(None),
                Some(11) => Err(ReadError::Refused),
                Some(12) => Err(ReadError::Interaction),
                Some(13) => Err(ReadError::Ambiguous),
                Some(16) => Err(ReadError::TooLarge),
                _ => Err(ReadError::Unavailable),
            };
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let timeout = i32::try_from(left.as_millis().min(50)).unwrap_or(50);
        if eof {
            std::thread::sleep(left.min(Duration::from_millis(1)));
        } else {
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll points at one initialized entry containing our live pipe.
            if unsafe { libc::poll(&raw mut poll, 1, timeout) } < 0
                && std::io::Error::last_os_error().kind() != ErrorKind::Interrupted
            {
                return Err(ReadError::Unavailable);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn command(script: &str) -> aterm_uds::spawnfd::Command {
        let shell = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
        let mut command = aterm_uds::spawnfd::command(&shell);
        command.args(["-c", script]);
        command
    }

    #[test]
    fn capture_distinguishes_present_absent_denied_and_partial_errors() {
        let deadline = Instant::now() + Duration::from_secs(60);
        let present = capture(&mut command("printf fixture"), || deadline, || {})
            .unwrap()
            .unwrap();
        assert_eq!(present.as_slice(), b"fixture");
        assert!(
            capture(&mut command("exit 10"), || deadline, || {})
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            capture(&mut command("printf partial; exit 10"), || deadline, || {}),
            Err(ReadError::Unavailable)
        ));
        assert!(matches!(
            capture(&mut command("printf private; exit 12"), || deadline, || {}),
            Err(ReadError::Interaction)
        ));
    }

    #[test]
    fn capture_caps_output_and_kills_a_started_child_at_the_deadline() {
        let deadline = Instant::now() + Duration::from_secs(60);
        let script = "i=0; while [ \"$i\" -lt 1025 ]; do printf '%064d' 0; i=$((i+1)); done";
        assert!(matches!(
            capture(&mut command(script), || deadline, || {}),
            Err(ReadError::TooLarge)
        ));

        let temp = aterm_tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("child.pid");
        let mut child = command("printf '%s' \"$$\" > \"$1\"; printf started; while :; do :; done");
        child.arg("fixture").arg(&pid_file);
        let until = Cell::new(Instant::now() + Duration::from_secs(60));
        let result = capture(&mut child, || until.get(), || until.set(Instant::now()));
        assert!(matches!(result, Err(ReadError::TimedOut)));
        let pid = std::fs::read_to_string(pid_file)
            .unwrap()
            .parse::<i32>()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if matches!(
                aterm_uds::process::process_presence(pid),
                aterm_uds::process::ProcessPresence::Exited
                    | aterm_uds::process::ProcessPresence::Absent
            ) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned timed-out child is still alive"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
