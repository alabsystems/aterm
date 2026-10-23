// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-machines-askpass`: the `SSH_ASKPASS` helper used during `aterm machines enroll`.

use crate::port::Secret;
use std::ffi::OsString;
use std::io::{IsTerminal as _, Read, Write};
use std::process::ExitCode;

/// The argv0 alias that routes an aterm executable here.
pub const ALIAS: &str = "aterm-machines-askpass";

/// `aterm-machines-askpass <prompt>`, as OpenSSH runs it.
#[must_use]
pub fn main(args: &[OsString]) -> ExitCode {
    crate::port::no_core_dumps();
    let prompt = args
        .first()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_default();
    let confirm = std::env::var_os("SSH_ASKPASS_PROMPT").is_some();
    let person = std::io::stdin().is_terminal();
    let code = answer(
        &prompt,
        confirm,
        person,
        &mut Unbuffered(0),
        &mut Unbuffered(1),
        &mut std::io::stderr().lock(),
    );
    ExitCode::from(code)
}

/// One of this process's descriptors, read and written with no buffer in between.
struct Unbuffered(i32);

impl Read for Unbuffered {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // SAFETY: `buf` is a live, writable slice of `buf.len()` bytes for the call.
        let n = unsafe { libc::read(self.0, buf.as_mut_ptr().cast(), buf.len()) };
        usize::try_from(n).map_err(|_| std::io::Error::last_os_error())
    }
}

impl Write for Unbuffered {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // SAFETY: `buf` is a live, readable slice of `buf.len()` bytes for the call.
        let n = unsafe { libc::write(self.0, buf.as_ptr().cast(), buf.len()) };
        usize::try_from(n).map_err(|_| std::io::Error::last_os_error())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Answers `prompt` with the first line of `input` on `out`, or writes a refusal to `err`.
pub fn answer(
    prompt: &str,
    confirm: bool,
    person: bool,
    input: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let refuse = |err: &mut dyn Write, why: &str| {
        let _ = writeln!(err, "aterm machines enroll: {why}");
        1
    };
    if person {
        return refuse(
            err,
            "aterm-machines-askpass answers ssh during `aterm machines enroll`, not a person",
        );
    }
    if confirm || !is_password_prompt(prompt) {
        return refuse(
            err,
            &format!(
                "not answering ssh's {:?}: enroll answers a password prompt only",
                crate::controller::text::shown(prompt)
            ),
        );
    }
    let line = match Secret::read_line(input) {
        Ok(Some(line)) if !line.is_empty() => line,
        Ok(_) => return refuse(err, "the password was already used for this login"),
        Err(e) => return refuse(err, &format!("reading the password: {e}")),
    };
    let wrote = out
        .write_all(line.bytes())
        .and_then(|()| out.write_all(b"\n"))
        .and_then(|()| out.flush());
    if wrote.is_err() {
        return refuse(err, "ssh did not take the answer");
    }
    0
}

/// Returns whether OpenSSH's `prompt` requests an account password rather than a passphrase or code.
#[must_use]
pub fn is_password_prompt(prompt: &str) -> bool {
    let lower = prompt.to_ascii_lowercase();
    lower.contains("password") && !lower.contains("passphrase")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "admin@192.168.64.5's password: ";
    const KBD_INT: &str = "(admin@192.168.64.5) Password:";

    fn run(prompt: &str, confirm: bool, person: bool, input: &[u8]) -> (u8, Vec<u8>, String) {
        let mut input = input;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = answer(prompt, confirm, person, &mut input, &mut out, &mut err);
        (code, out, String::from_utf8(err).unwrap())
    }

    #[test]
    fn a_password_prompt_is_answered_with_the_first_line_of_input() {
        for prompt in [PASSWORD, KBD_INT] {
            let (code, out, err) = run(prompt, false, false, b"hunter2\n");
            assert_eq!(
                (code, out.as_slice(), err.as_str()),
                (0, &b"hunter2\n"[..], "")
            );
        }
    }

    #[test]
    fn a_confirmation_a_passphrase_a_code_or_a_person_is_never_answered() {
        for (prompt, confirm, person) in [
            (
                "Are you sure you want to continue connecting (yes/no)?",
                true,
                false,
            ),
            (PASSWORD, true, false),
            (
                "Enter passphrase for key '/Users//a/.ssh/id_ed25519': ",
                false,
                false,
            ),
            ("Verification code: ", false, false),
            (PASSWORD, false, true),
        ] {
            let (code, out, err) = run(prompt, confirm, person, b"hunter2\n");
            assert_eq!(code, 1, "{prompt}");
            assert!(out.is_empty(), "{prompt}");
            assert!(!err.contains("hunter2"), "{err}");
        }
    }

    #[test]
    fn a_second_prompt_of_one_login_finds_the_password_used() {
        let (code, out, err) = run(PASSWORD, false, false, b"");
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert!(err.contains("already used"), "{err}");
    }
}
