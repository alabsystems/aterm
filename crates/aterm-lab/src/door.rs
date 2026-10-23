// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE door to a lab's aterm: every control-socket call the lab makes, as a client process or in process, passes these three rules.

use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Command, Stdio};

use crate::Fail;
use crate::run::Run;
use crate::stop::{self, Stopped};

/// What a client process said.
#[derive(Debug, Default)]
pub struct Reply {
    /// Its exit status (`124`: an `await` that timed out).
    pub code: i32,
    /// Its standard output.
    pub stdout: String,
    /// Its standard error.
    pub stderr: String,
}

/// A lab's door: the aterm under test, aimed at that lab only.
pub struct Door<'a> {
    run: &'a Run,
    aterm: PathBuf,
}

impl<'a> Door<'a> {
    /// The door of `run`: the aterm `up` recorded for it (its jail pins that file), never a new pick.
    /// # Errors
    /// The lab records no aterm.
    pub fn new(run: &'a Run) -> Result<Door<'a>, String> {
        Ok(Door {
            run,
            aterm: run.need_aterm()?,
        })
    }

    /// The lab this door opens.
    #[must_use]
    pub const fn run(&self) -> &Run {
        self.run
    }

    /// The rules of the socket (module docs).
    /// # Errors
    /// Which rule the socket broke.
    pub fn check_socket(&self) -> Result<(), String> {
        let (sock, dir) = (&self.run.sock, &self.run.sockdir);
        let owned = std::fs::metadata(dir).is_ok_and(|m| m.uid() == crate::sys::getuid());
        if dir.is_empty() || *sock != format!("{dir}/a.sock") || !owned {
            return Err(format!(
                "refusing to talk to '{sock}': not this lab's socket"
            ));
        }
        let link = |p: &str| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink());
        if link(sock) || link(&format!("{dir}/a.sock.token")) {
            return Err(format!(
                "refusing to talk to '{sock}': it or its token is a link"
            ));
        }
        if !lab_socket_shape(sock) {
            return Err(format!("refusing to talk to '{sock}': not a lab socket"));
        }
        let is_sock = std::fs::metadata(sock).is_ok_and(|m| {
            use std::os::unix::fs::FileTypeExt as _;
            m.file_type().is_socket()
        });
        if !is_sock {
            return Err(format!(
                "lab '{}' has no control socket at {sock} (is it up? {} ps)",
                self.run.tag,
                crate::LAB
            ));
        }
        Ok(())
    }

    /// The aterm under test with `args`, its environment scrubbed and aimed at
    /// the lab's scratch, under its recorded jail; `path` is its `PATH`.
    fn command(&self, args: &[&str], path: &str) -> Command {
        let w = &self.run.w;
        let mut cmd = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec");
        cmd.arg("-f")
            .arg(self.run.run.join("jail.sb"))
            .arg(&self.aterm)
            .env_clear()
            .env("PATH", path)
            .env("xcrun_nocache", "1")
            .env("HOME", w.join("home"))
            .env("XDG_CONFIG_HOME", w.join("cfg"))
            .env("XDG_RUNTIME_DIR", &self.run.sockdir)
            .env("ATERM_STATE_HOME", w.join("state"))
            .args(args);
        cmd
    }

    fn ctl_args<'s>(&'s self, args: &[&'s str]) -> Result<Vec<&'s str>, String> {
        if let Some(first) = args.first()
            && first.starts_with('-')
        {
            return Err(format!(
                "ctl: '{first}' before the verb: flags are not passed through (the socket is this lab's)"
            ));
        }
        self.check_socket()?;
        let mut v = vec!["ctl", "--sock", self.run.sock.as_str(), "--"];
        v.extend_from_slice(args);
        Ok(v)
    }

    /// `aterm ctl <args>` against this lab, captured.
    /// # Errors
    /// A rule refused it, or the client could not start.
    pub fn ctl(&self, args: &[&str]) -> Result<Reply, String> {
        let v = self.ctl_args(args)?;
        let out = self
            .command(&v, "/usr/bin:/bin")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not run {}: {e}", self.aterm.display()))?;
        Ok(Reply {
            code: out.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// `aterm ctl <args>` against this lab with `input` on its standard input (a worker's task for `spawn agent=claude … -`), captured.
    /// # Errors
    /// A rule refused it, or the client could not start.
    pub fn ctl_input(&self, args: &[&str], input: &[u8]) -> Result<Reply, String> {
        use std::io::Write as _;
        let v = self.ctl_args(args)?;
        let mut child = self
            .command(&v, "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run {}: {e}", self.aterm.display()))?;
        let wrote = child
            .stdin
            .take()
            .map_or(Ok(()), |mut pipe| pipe.write_all(input));
        let out = child
            .wait_with_output()
            .map_err(|e| format!("could not run {}: {e}", self.aterm.display()))?;
        wrote.map_err(|e| format!("could not write the client's input: {e}"))?;
        Ok(Reply {
            code: out.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// The standard output of `aterm ctl <args>`, empty on any failure.
    #[must_use]
    pub fn out(&self, args: &[&str]) -> String {
        self.ctl(args).map(|r| r.stdout).unwrap_or_default()
    }

    /// `aterm ctl <args>` against this lab, on this process's own terminal; its status.
    /// # Errors
    /// A rule refused it, or the client could not start.
    pub fn ctl_inherit(&self, args: &[&str]) -> Result<i32, String> {
        let v = self.ctl_args(args)?;
        let st = self
            .command(&v, "/usr/bin:/bin")
            .status()
            .map_err(|e| format!("could not run {}: {e}", self.aterm.display()))?;
        Ok(st.code().unwrap_or(1))
    }

    /// `aterm drive --socket <sock> <args>` (the hosted loop's own reader).
    /// # Errors
    /// A rule refused it, or the client could not start.
    pub fn drive(&self, args: &[&str]) -> Result<Reply, String> {
        self.check_socket()?;
        let mut v = vec!["drive", "--socket", self.run.sock.as_str()];
        v.extend_from_slice(args);
        let out = self
            .command(&v, "/usr/bin:/bin")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not run {}: {e}", self.aterm.display()))?;
        Ok(Reply {
            code: out.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// `aterm harness ledger --sock <sock> @<sid>`, both streams.
    /// # Errors
    /// A rule refused it, or the client could not start.
    pub fn harness_ledger(&self, sid: &str) -> Result<String, String> {
        self.check_socket()?;
        let at = format!("@{sid}");
        let out = self
            .command(
                &["harness", "ledger", "--sock", &self.run.sock, &at],
                "/usr/bin:/bin",
            )
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not run {}: {e}", self.aterm.display()))?;
        Ok(format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ))
    }

    /// One authenticated control connection to the lab, in this process.
    /// # Errors
    /// A rule refused it, or the connection or its token failed.
    pub fn connect(&self) -> Result<aterm_agent::RelayClient<aterm_uds::CtlStream>, String> {
        self.check_socket()?;
        aterm_agent::RelayClient::connect_local_beside(&self.run.sock)
            .map_err(|e| format!("cannot reach {}: {e}", self.run.sock))
    }

    /// `await match <re>` on the lab's screen for up to `secs`: whether a row matched.
    /// # Errors
    /// Interrupted, a rule refused it, the connection failed, or the server refused the pattern.
    pub fn await_match(&self, pattern: &str, secs: u64) -> Result<bool, Fail> {
        self.await_level("match", pattern, Duration::from_secs(secs))
    }

    /// `await gone <re>`: whether, within `secs`, no row matches any more.
    /// # Errors
    /// As [`Door::await_match`].
    pub fn await_gone(&self, pattern: &str, secs: u64) -> Result<bool, Fail> {
        self.await_level("gone", pattern, Duration::from_secs(secs))
    }

    /// `await idle <quiet_ms>`: whether, within `secs`, the screen went `quiet_ms` without output (a settle, never a fixed pause).
    /// # Errors
    /// As [`Door::await_match`].
    pub fn await_idle(&self, quiet_ms: u64, secs: u64) -> Result<bool, Fail> {
        self.await_level(&format!("idle {quiet_ms}"), "", Duration::from_secs(secs))
    }

    /// [`Door::await_match`] where only an interrupt is an error: a refused or broken wait reads as "not seen", which every caller then reports.
    /// # Errors
    /// Interrupted.
    pub fn seen(&self, pattern: &str, secs: u64) -> Result<bool, Stopped> {
        match self.await_match(pattern, secs) {
            Ok(seen) => Ok(seen),
            Err(Fail {
                stopped: Some(sig), ..
            }) => Err(Stopped(sig)),
            Err(_) => Ok(false),
        }
    }

    /// One `await <kind> <pattern>` held for up to `total`, as short waits
    /// with an interrupt check between them: a ^C ends it within a slice.
    fn await_level(&self, kind: &str, pattern: &str, total: Duration) -> Result<bool, Fail> {
        let quiet = kind
            .strip_prefix("idle ")
            .and_then(|ms| ms.parse().ok())
            .map_or(Duration::ZERO, Duration::from_millis);
        let slice = Duration::from_secs(1) + quiet;
        let mut client = self.connect()?;
        let what = if pattern.is_empty() {
            kind.to_string()
        } else {
            format!("{kind} {}", wire_pattern(pattern))
        };
        let end = Instant::now() + total;
        loop {
            stop::check()?;
            let left = end.saturating_duration_since(Instant::now());
            let ms = left.min(slice).as_millis();
            let line = client
                .request_line(&format!("await {what} timeout={ms}"))
                .map_err(|e| Fail::die(format!("await {what}: {e}")))?;
            if line.starts_with("OK timeout") {
                if left <= slice {
                    return Ok(false);
                }
            } else if line == "OK" || line.starts_with("OK ") {
                return Ok(true);
            } else {
                return Err(Fail::die(format!("await {what}: {line}")));
            }
        }
    }
}

/// `/tmp/at.<one name>/a.sock`.
#[must_use]
pub fn lab_socket_shape(sock: &str) -> bool {
    sock.strip_prefix("/tmp/at.")
        .and_then(|rest| rest.strip_suffix("/a.sock"))
        .is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

/// A pattern as one whitespace-free wire token: white space and `=` escaped as `\xNN`.
#[must_use]
pub fn wire_pattern(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    for c in pattern.chars() {
        if c.is_ascii_whitespace() || c == '=' {
            out.push_str(&format!("\\x{:02x}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_lab_shaped_socket_passes() {
        assert!(lab_socket_shape("/tmp/at.PXaw/a.sock"));
        assert!(!lab_socket_shape("/tmp/at./a.sock"));
        assert!(!lab_socket_shape("/tmp/at.x/y/a.sock"));
        assert!(!lab_socket_shape("/private/tmp/at.x/a.sock"));
        assert!(!lab_socket_shape(
            "/Users//o/Library/Application Support/aterm/aterm.sock"
        ));
    }

    #[test]
    fn a_pattern_crosses_the_wire_as_one_token() {
        assert_eq!(
            wire_pattern("❯|Welcome|trust|Claude Code"),
            "❯|Welcome|trust|Claude\\x20Code"
        );
        assert_eq!(wire_pattern("level=attention"), "level\\x3dattention");
        assert!(!wire_pattern("a b\tc").contains(char::is_whitespace));
    }
}
