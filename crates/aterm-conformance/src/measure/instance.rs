// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private headless instance of the binary under measurement, and the
//! client that drives it.
//!
//! The instance runs in a scratch world under `/tmp` (a socket path must fit
//! `sun_path`): its own HOME, config and runtime dirs, `/bin/sh` with a fixed
//! two-column prompt, and every automatic lane off. It holds a lifeline
//! ([`aterm_uds::lifeline`]), so a measuring process killed any way takes its
//! instance with it. The client is the binary's own `ctl`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd;

/// A wait for something that must happen: a hang detector, never a latency
/// budget. A correct run leaves the moment the event lands.
pub const MUST_HAPPEN: Duration = Duration::from_secs(60);

/// The gap between two observations of a bounded poll. Not synchronisation:
/// the observation decides; this only keeps the poll off a core.
const POLL_GAP: Duration = Duration::from_millis(50);

/// The most a single `ctl --timeout` asks for, in seconds: past every
/// minute-long wait a request asks the server for, so a server-side `await`
/// that times out answers for itself before the client gives up.
const CTL_TIMEOUT_SECS: u64 = 120;

/// Why a take decided nothing: never a pass and never a paint finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CouldNotRun(pub String);

impl std::fmt::Display for CouldNotRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CouldNotRun {}

/// Shorthand for a [`CouldNotRun`] result.
pub fn could_not_run<T>(why: impl Into<String>) -> Result<T, CouldNotRun> {
    Err(CouldNotRun(why.into()))
}

/// A take's whole-run limit. Every wait and every request gives up at its
/// end, so a slow but live instance cannot stretch one take without bound.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    ends: Instant,
    secs: u64,
}

impl Budget {
    /// A budget of `secs` seconds, starting now.
    #[must_use]
    pub fn from_now(secs: u64) -> Self {
        Self {
            ends: Instant::now() + Duration::from_secs(secs),
            secs,
        }
    }

    /// The time left, or `None` once the budget is spent.
    #[must_use]
    pub fn left(&self) -> Option<Duration> {
        self.ends
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
    }

    /// The reason a take gives up when the budget ran out during `what`.
    #[must_use]
    pub fn spent(&self, what: &str) -> CouldNotRun {
        CouldNotRun(format!(
            "the take's {}s budget ran out during {what}",
            self.secs
        ))
    }

    /// Poll `probe` until it answers `Some`: for at most [`MUST_HAPPEN`], and
    /// never past the budget. An event that already happened is no wait, so
    /// the probe is always asked once.
    ///
    /// # Errors
    /// [`CouldNotRun`] naming `what` once the minute or the budget is gone.
    pub fn await_event<T>(
        &self,
        what: &str,
        mut probe: impl FnMut() -> Option<T>,
    ) -> Result<T, CouldNotRun> {
        let deadline = (Instant::now() + MUST_HAPPEN).min(self.ends);
        loop {
            if let Some(v) = probe() {
                return Ok(v);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(if self.left().is_none() {
                    self.spent(&format!("the wait for {what}"))
                } else {
                    CouldNotRun(format!("{what} did not happen within {MUST_HAPPEN:?}"))
                });
            }
            std::thread::sleep(POLL_GAP.min(deadline - now));
        }
    }
}

/// Run `program --version` once, unclocked: a fresh binary's first exec waits
/// on the system's code check, which no take's timeline should carry.
pub fn first_exec(program: &Path) {
    let _ = spawnfd::command(program)
        .arg("--version")
        .stdin(spawnfd::Stdio::null())
        .stdout(spawnfd::Stdio::null())
        .stderr(spawnfd::Stdio::null())
        .status();
}

/// A private `0700` directory under `/tmp`, removed on drop unless kept for a
/// diagnostic read.
pub struct Scratch {
    path: PathBuf,
    /// The removal guard; `None` once kept.
    dir: Option<aterm_tempfile::TempDir>,
}

impl Scratch {
    /// A fresh directory named `<prefix>-<pid>_<random>_<n>`, made
    /// exclusively.
    ///
    /// # Errors
    /// The directory could not be made.
    pub fn new(prefix: &str) -> std::io::Result<Self> {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("{prefix}-"))
            .tempdir_in("/tmp")?;
        Ok(Self {
            path: dir.path().to_path_buf(),
            dir: Some(dir),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Leave the directory behind when this is dropped.
    pub fn keep(&mut self) {
        std::mem::forget(self.dir.take());
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.dir.is_none() {
            eprintln!("measure: kept the scratch world at {}", self.path.display());
        }
    }
}

/// The world an instance runs in, written into a [`Scratch`] before launch.
pub struct World {
    pub root: PathBuf,
    pub home: PathBuf,
    pub sock: PathBuf,
    pub log: PathBuf,
}

/// The automatic lanes off: no update check, no package loop, the machine's
/// own preferences left alone.
pub const CONFIG_OFF: &str = "[update]\nenabled = false\nauto_apply = false\n\
                              [packages]\nenabled = false\n\
                              [machine]\nuniversal_control = \"leave\"\n";

impl World {
    /// Lay out `root`: `home/` with a two-column `/bin/sh` prompt (so a typed
    /// burst never wraps on a long host name), `cfg/aterm/aterm.toml` holding
    /// the audible bell off, `config_head` then [`CONFIG_OFF`], and `run/`.
    /// The bell line leads because `config_head` may open a table; it is off
    /// because the binary under measurement (`--app`) may be a build from
    /// before a headless aterm stopped beeping, and its BEL would play the
    /// owner's alert sound with nothing on screen.
    ///
    /// # Errors
    /// A file or directory could not be written.
    pub fn prepare(root: &Path, config_head: &str) -> std::io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let home = root.join("home");
        for dir in [home.clone(), root.join("cfg/aterm"), root.join("run")] {
            std::fs::create_dir_all(&dir)?;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        // The login shell reads ~/.profile after /etc/profile (which sets a
        // host-named prompt); an interactive sh reads $ENV.
        std::fs::write(home.join(".profile"), "PS1='$ '\n")?;
        std::fs::write(home.join(".shrc"), "PS1='$ '\n")?;
        std::fs::write(
            root.join("cfg/aterm/aterm.toml"),
            format!("bell_sound = false\n{config_head}\n{CONFIG_OFF}"),
        )?;
        Ok(Self {
            root: root.to_path_buf(),
            home,
            sock: root.join("s.sock"),
            log: root.join("gui.log"),
        })
    }

    /// The instance's whole environment: nothing inherited.
    fn env(&self) -> Vec<(&'static str, OsString)> {
        let mut env = vec![
            ("HOME", self.home.clone().into_os_string()),
            ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("SHELL", "/bin/sh".into()),
            ("ENV", self.home.join(".shrc").into_os_string()),
            ("TERM", "xterm-256color".into()),
            ("XDG_CONFIG_HOME", self.root.join("cfg").into_os_string()),
            ("XDG_RUNTIME_DIR", self.root.join("run").into_os_string()),
            // The sandbox shell writes no history after the world is removed.
            ("HISTFILE", "/dev/null".into()),
        ];
        // A development build's spawn trace, passed through when asked for.
        if let Some(v) = std::env::var_os("ATERM_TRACE_SPAWN") {
            env.push(("ATERM_TRACE_SPAWN", v));
        }
        env
    }
}

/// A running headless instance. Dropping it kills and reaps it.
pub struct Instance {
    child: spawnfd::Child,
    _lifeline: aterm_uds::lifeline::Lifeline,
    pub log: PathBuf,
    pub ctl: Ctl,
}

impl Instance {
    /// Launch `bin --headless` in `world` and wait until its control socket
    /// listens and its session is ready. Every request its client makes, and
    /// every wait on it, ends with `budget`.
    ///
    /// # Errors
    /// The launch failed, the instance exited before it listened (its log
    /// tail is in the reason), or it never listened.
    pub fn launch(bin: &Path, world: &World, budget: Budget) -> Result<Self, CouldNotRun> {
        let log = std::fs::File::create(&world.log)
            .map_err(|e| CouldNotRun(format!("cannot create {}: {e}", world.log.display())))?;
        let err = log
            .try_clone()
            .map_err(|e| CouldNotRun(format!("cannot share the instance log: {e}")))?;
        let (lifeline, far) = aterm_uds::lifeline::Lifeline::open(&world.root)
            .map_err(|e| CouldNotRun(format!("the lifeline could not be made: {e}")))?;
        let mut cmd = spawnfd::command(bin);
        cmd.env_clear()
            .envs(world.env())
            .args(["--headless", "--no-reroute"])
            .arg(aterm_uds::lifeline::FLAG)
            .arg(aterm_uds::lifeline::STDIN.to_string())
            .arg("--control-sock")
            .arg(&world.sock)
            .current_dir(&world.home)
            .stdin(far)
            .stdout(log)
            .stderr(err);
        let child = cmd
            .spawn()
            .map_err(|e| CouldNotRun(format!("cannot launch {}: {e}", bin.display())))?;
        let mut inst = Self {
            child,
            _lifeline: lifeline,
            log: world.log.clone(),
            ctl: Ctl {
                bin: bin.to_path_buf(),
                sock: world.sock.clone(),
                home: world.home.clone(),
                budget,
            },
        };
        let sock = world.sock.clone();
        budget.await_event("the control socket listening", || {
            if let Ok(Some(status)) = inst.child.try_wait() {
                return Some(Err(status));
            }
            std::os::unix::net::UnixStream::connect(&sock)
                .is_ok()
                .then_some(Ok(()))
        })?
        .map_err(|status| {
            CouldNotRun(format!(
                "the instance exited ({status}) before its control socket listened; log tail:\n{}",
                inst.log_tail()
            ))
        })?;
        inst.ctl.ok(&["ready"])?;
        Ok(inst)
    }

    /// The last lines of the instance log, for a reason.
    #[must_use]
    pub fn log_tail(&self) -> String {
        let body = std::fs::read_to_string(&self.log).unwrap_or_default();
        let lines: Vec<&str> = body.lines().collect();
        lines[lines.len().saturating_sub(30)..].join("\n")
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One `ctl` request's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    /// The client's exit code; `None` when it could not be run or was killed.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Reply {
    #[must_use]
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// The first line of stdout.
    #[must_use]
    pub fn line(&self) -> &str {
        self.stdout.lines().next().unwrap_or("")
    }
}

/// The binary's own control client, aimed at one instance.
#[derive(Clone, Debug)]
pub struct Ctl {
    bin: PathBuf,
    sock: PathBuf,
    home: PathBuf,
    budget: Budget,
}

impl Ctl {
    /// Run `bin ctl --sock <sock> --timeout <secs> <args…>` and collect it.
    /// The deadline is [`CTL_TIMEOUT_SECS`] or what is left of the budget,
    /// whichever is less; a spent budget runs nothing (`code` is `None`).
    #[must_use]
    pub fn call(&self, args: &[&str]) -> Reply {
        let Some(left) = self.budget.left() else {
            return Reply {
                code: None,
                stdout: String::new(),
                stderr: self.budget.spent(&format!("ctl {}", args.join(" "))).0,
            };
        };
        let timeout = left.as_secs().saturating_add(1).min(CTL_TIMEOUT_SECS);
        let out = spawnfd::command(&self.bin)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .arg("ctl")
            .arg("--sock")
            .arg(&self.sock)
            .arg("--timeout")
            .arg(timeout.to_string())
            .args(args)
            .stdin(spawnfd::Stdio::null())
            .output();
        match out {
            Ok(out) => Reply {
                code: out.status.code(),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            },
            Err(e) => Reply {
                code: None,
                stdout: String::new(),
                stderr: format!("could not run ctl: {e}"),
            },
        }
    }

    /// A request that must succeed: its first stdout line.
    ///
    /// # Errors
    /// The request failed; the reason quotes it.
    pub fn ok(&self, args: &[&str]) -> Result<String, CouldNotRun> {
        let reply = self.call(args);
        if reply.ok() {
            Ok(reply.line().to_string())
        } else if self.budget.left().is_none() {
            Err(self.budget.spent(&format!("ctl {}", args.join(" "))))
        } else {
            could_not_run(format!(
                "ctl {} failed (exit {:?}): {} {}",
                args.join(" "),
                reply.code,
                reply.stdout.trim(),
                reply.stderr.trim()
            ))
        }
    }

    /// [`Budget::await_event`] on this client's budget.
    ///
    /// # Errors
    /// [`CouldNotRun`] naming `what` once the minute or the budget is gone.
    pub fn await_event<T>(
        &self,
        what: &str,
        probe: impl FnMut() -> Option<T>,
    ) -> Result<T, CouldNotRun> {
        self.budget.await_event(what, probe)
    }

    /// The `key=value` field `key` of a request's first line.
    ///
    /// # Errors
    /// The request failed or its line has no such field.
    pub fn field(&self, args: &[&str], key: &str) -> Result<String, CouldNotRun> {
        let line = self.ok(args)?;
        field(&line, key).map(str::to_string).ok_or_else(|| {
            CouldNotRun(format!(
                "ctl {} reported no {key}= ({line})",
                args.join(" ")
            ))
        })
    }
}

/// The `key=value` token `key` of a reply line.
#[must_use]
pub fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
}

/// A numeric `key=value` field.
#[must_use]
pub fn uint_field(line: &str, key: &str) -> Option<u64> {
    field(line, key).and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spent_budget_gives_up_at_once_but_still_asks_the_probe_once() {
        let spent = Budget::from_now(0);
        assert_eq!(spent.left(), None);
        assert_eq!(spent.await_event("x", || Some(7)), Ok(7));
        let mut asked = 0;
        let got = spent.await_event("x", || {
            asked += 1;
            None::<()>
        });
        assert!(got.is_err());
        assert_eq!(asked, 1);
    }

    #[test]
    fn a_live_budget_leaves_the_moment_the_event_lands() {
        let budget = Budget::from_now(60);
        assert!(budget.left().is_some());
        let mut asked = 0;
        let got = budget.await_event("x", || {
            asked += 1;
            (asked == 3).then_some(asked)
        });
        assert_eq!(got, Ok(3));
    }

    /// The measure world's `aterm.toml` sets the audible bell off once, ahead
    /// of every table, even when the head it is given opens one (the cold
    /// streamer's `[sparkle_words]`): a `bell_sound` line after a table header
    /// would be that table's key, not the root one aterm reads.
    #[test]
    fn a_measure_world_starts_with_the_audible_bell_off() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let head = "cursor_trail_style = \"off\"\n[sparkle_words]\nenabled = false";
        let world = World::prepare(dir.path(), head).expect("world");
        let text =
            std::fs::read_to_string(world.root.join("cfg/aterm/aterm.toml")).expect("written");
        let keys = |key: &str| {
            text.lines()
                .filter(|l| l.split('=').next().map(str::trim) == Some(key))
                .count()
        };
        assert_eq!(text.lines().next(), Some("bell_sound = false"), "{text}");
        assert_eq!(keys("bell_sound"), 1, "one bell key: {text}");
        assert!(text.contains(head), "the head follows: {text}");
        assert!(
            text.ends_with(CONFIG_OFF),
            "the lanes-off tables close it: {text}"
        );
    }

    #[test]
    fn a_client_whose_budget_is_spent_runs_nothing() {
        let client = |secs| Ctl {
            bin: PathBuf::from("/usr/bin/true"),
            sock: PathBuf::from("/nonexistent.sock"),
            home: PathBuf::from("/"),
            budget: Budget::from_now(secs),
        };
        assert_eq!(client(60).call(&["status"]).code, Some(0));
        assert!(client(60).ok(&["status"]).is_ok());
        assert_eq!(client(0).call(&["status"]).code, None);
        assert!(client(0).ok(&["status"]).is_err());
    }
}
