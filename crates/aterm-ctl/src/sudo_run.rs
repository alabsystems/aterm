// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl sudo -- <command>`: asks the aterm hosting this shell to start `/usr/bin/sudo` itself
//! on these exact words and this process's stdio (`docs/DESIGN-sudo-askpass-2026-10-15.md` §3.10).

use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const REPLY_LINE_LIMIT: usize = 4096;

/// The words after `sudo` when this request runs a command; `None` for any other verb and for
/// `sudo --help`, which the help path answers.
pub(crate) fn command_words(parts: &[String]) -> Option<&[String]> {
    let (verb, rest) = parts.split_first()?;
    if verb != "sudo" || matches!(rest.first().map(String::as_str), Some("-h" | "--help")) {
        return None;
    }
    Some(rest)
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// The absolute path `word` runs: joined to `cwd` when it names a path, else the first executable
/// file on `path`, as the shell would find it. aterm's sudo has neither this shell's `PATH` nor a
/// reason to guess, and the sheet then shows the program that runs.
fn resolve_program(word: &str, cwd: &Path, path: Option<&std::ffi::OsStr>) -> io::Result<String> {
    let found: Option<PathBuf> = if word.contains('/') {
        Some(cwd.join(word))
    } else {
        path.and_then(|path| {
            std::env::split_paths(path)
                .map(|dir| dir.join(word))
                .find(|candidate| is_executable_file(candidate))
        })
    };
    let found = found.ok_or_else(|| invalid(format!("sudo: {word}: command not found")))?;
    found
        .into_os_string()
        .into_string()
        .map_err(|_| invalid(format!("sudo: {word}: its path is not UTF-8")))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// The exit code a final server line means: the command's, `128 + n` for a signal, or the line
/// itself as the error.
fn final_line_code(line: &str) -> Result<u8, String> {
    let number = |text: &str| text.parse::<u8>().ok();
    if let Some(code) = line.strip_prefix("OK sudo exit=").and_then(number) {
        return Ok(code);
    }
    if let Some(signal) = line.strip_prefix("OK sudo signal=").and_then(number) {
        return Ok(128u8.saturating_add(signal));
    }
    Err(line.to_string())
}

fn read_line(stream: &crate::CtlStream) -> io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    let mut reader = aterm_uds::hangup::Awake(stream);
    loop {
        if reader.read(&mut byte)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "sudo: aterm closed the connection; the command may still be running",
            ));
        }
        if byte[0] == b'\n' {
            return Ok(String::from_utf8_lossy(&line).into_owned());
        }
        line.push(byte[0]);
        if line.len() > REPLY_LINE_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sudo: aterm sent an overlong reply",
            ));
        }
    }
}

#[cfg(unix)]
fn send_stdio(stream: &crate::CtlStream) -> io::Result<()> {
    use std::os::fd::BorrowedFd;
    // SAFETY: fds 0, 1 and 2 stay open for this process's life, and each borrow ends with the send.
    let stdio = unsafe {
        [
            BorrowedFd::borrow_raw(0),
            BorrowedFd::borrow_raw(1),
            BorrowedFd::borrow_raw(2),
        ]
    };
    aterm_uds::fdpass::send_with_fds(stream, b"s", &stdio).map(|_| ())
}

#[cfg(not(unix))]
fn send_stdio(_stream: &crate::CtlStream) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Run `words` through the aterm at the resolved socket; this process exits as the command did.
pub(crate) fn run(
    words: &[String],
    sock: Option<String>,
    pid: Option<u32>,
) -> io::Result<ExitCode> {
    let words = match words.split_first() {
        Some((first, rest)) if first == "--" => rest,
        _ => words,
    };
    let Some((program, arguments)) = words.split_first() else {
        return Err(invalid(
            "usage: aterm ctl sudo -- <command> [args...]".into(),
        ));
    };
    let cwd = std::env::current_dir()?;
    let program = resolve_program(program, &cwd, std::env::var_os("PATH").as_deref())?;
    let mut exact = vec![program];
    exact.extend_from_slice(arguments);
    let cwd_text = cwd
        .into_os_string()
        .into_string()
        .map_err(|_| invalid("sudo: the working directory's path is not UTF-8".into()))?;
    let request = format!(
        "sudo {}\n",
        aterm_uds::sudo::encode_command(&cwd_text, &exact)
    );
    if request.len() > aterm_types::control_verbs::MAX_REQUEST_LINE {
        return Err(invalid(format!(
            "sudo: the command is too long for aterm's request line ({} bytes at most once encoded); \
             put it in a script and run that",
            aterm_types::control_verbs::MAX_REQUEST_LINE
        )));
    }

    let path = crate::resolve_path(sock, pid, std::env::var(crate::SELF_SID_ENV).ok())?;
    let stream = match crate::open_accepted(
        &path,
        crate::TargetOrigin::Pinned,
        Some(crate::REQUIRED_SERVER_HANG),
    )? {
        crate::Acceptance::Served(stream) => stream,
        crate::Acceptance::Refused(_) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "sudo: aterm refused the connection",
            ));
        }
        crate::Acceptance::NotAccepted(_) => return Err(io::ErrorKind::TimedOut.into()),
        crate::Acceptance::Dropped => return Err(io::ErrorKind::UnexpectedEof.into()),
    };
    // The socket's options are all set before what aterm answers: aterm closes once it has
    // refused or the command ended, and macOS then refuses an option (EINVAL) though the answer
    // is still there to read. `open_accepted` left the read bounded by the server hang detector.
    (&stream).write_all(request.as_bytes())?;
    let admitted = read_line(&stream)?;
    if admitted != "OK sudo stdio" {
        return Err(io::Error::other(admitted));
    }
    // No deadline: the command runs as long as it runs, and this process is its caller.
    stream.set_read_timeout(None)?;
    send_stdio(&stream)?;
    let last = read_line(&stream)?;
    final_line_code(&last)
        .map(ExitCode::from)
        .map_err(io::Error::other)
}

#[cfg(test)]
#[path = "sudo_run_tests.rs"]
mod tests;
