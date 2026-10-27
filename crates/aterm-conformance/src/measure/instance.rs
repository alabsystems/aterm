// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private instance of the binary under measurement, headless or windowed, and the client that
//! drives it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd;

/// A wait for something that must happen: a hang detector, never a latency budget.
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
pub enum CouldNotRun {
    /// The instance, the machine or the instrument stopped the take.
    Refused(String),
    /// The subject has no answer to what the take asked: it does not know a verb, or its reply
    /// carries no field the row reads. A build older than the row answers so; what that says about
    /// the subject is for whoever knows which build it is (`verify measure` compares its commit
    /// with the checkout's).
    Unanswered(String),
}

impl CouldNotRun {
    /// The reason, in words.
    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Self::Refused(why) | Self::Unanswered(why) => why,
        }
    }

    /// The same kind of reason, reworded by `f`.
    #[must_use]
    pub fn map(self, f: impl FnOnce(String) -> String) -> Self {
        match self {
            Self::Refused(why) => Self::Refused(f(why)),
            Self::Unanswered(why) => Self::Unanswered(f(why)),
        }
    }
}

impl std::fmt::Display for CouldNotRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.why())
    }
}

impl std::error::Error for CouldNotRun {}

/// Shorthand for a [`CouldNotRun::Refused`] result.
pub fn could_not_run<T>(why: impl Into<String>) -> Result<T, CouldNotRun> {
    Err(CouldNotRun::Refused(why.into()))
}

/// Shorthand for a [`CouldNotRun::Unanswered`] result.
pub fn unanswered<T>(why: impl Into<String>) -> Result<T, CouldNotRun> {
    Err(CouldNotRun::Unanswered(why.into()))
}

/// A take's whole-run limit.
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
        CouldNotRun::Refused(format!(
            "the take's {}s budget ran out during {what}",
            self.secs
        ))
    }

    /// Poll `probe` until it answers `Some`: for at most [`MUST_HAPPEN`], and never past the budget.
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
                    CouldNotRun::Refused(format!("{what} did not happen within {MUST_HAPPEN:?}"))
                });
            }
            std::thread::sleep(POLL_GAP.min(deadline - now));
        }
    }
}

/// Run `program --version` once, unclocked: a fresh binary's first exec waits on the system's code check, which no take's timeline should carry.
pub fn first_exec(program: &Path) {
    let _ = spawnfd::command(program)
        .arg("--version")
        .stdin(spawnfd::Stdio::null())
        .stdout(spawnfd::Stdio::null())
        .stderr(spawnfd::Stdio::null())
        .status();
}

/// A private `0700` directory under `/tmp` itself, removed on drop unless kept for a
/// diagnostic read. It is made outside `/tmp`'s quiet folder (`aterm_tempfile::QUIET`, a dot
/// folder): the instance's HOME, runtime dir and socket sit where an installed aterm's would,
/// since its file wakes refuse a root under a dot folder, and what it measures stays what a
/// person's aterm does.
pub struct Scratch {
    path: PathBuf,
    /// The removal guard; `None` once kept.
    dir: Option<aterm_tempfile::TempDir>,
}

impl Scratch {
    /// A fresh directory named `<prefix>-<pid>_<random>_<n>`, made exclusively.
    /// # Errors
    /// The directory could not be made.
    pub fn new(prefix: &str) -> std::io::Result<Self> {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("{prefix}-"))
            .heard()
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
    pub const fn keep(&mut self) {
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

/// The automatic lanes off: no update check, no package loop, the machine's own preferences left alone.
pub const CONFIG_OFF: &str = "[update]\nenabled = false\nauto_apply = false\n\
                              [packages]\nenabled = false\n\
                              [machine]\nuniversal_control = \"leave\"\n";

impl World {
    /// Lay out `root`: `home/` with a two-column `/bin/sh` prompt (so a typed burst never wraps on a long host name), `cfg/aterm/aterm.toml` holding the audible bell off, `config_head` then [`CONFIG_OFF`], and `run/`.
    /// # Errors
    /// A file or directory could not be written.
    pub fn prepare(root: &Path, config_head: &str) -> std::io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let home = root.join("home");
        for dir in [home.clone(), root.join("cfg/aterm"), root.join("run")] {
            std::fs::create_dir_all(&dir)?;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
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
            ("HISTFILE", "/dev/null".into()),
        ];
        if let Some(v) = std::env::var_os("ATERM_TRACE_SPAWN") {
            env.push(("ATERM_TRACE_SPAWN", v));
        }
        env
    }
}

/// `PRIO_DARWIN_ROLE` (`<sys/resource.h>`, private): `setpriority`'s darwin-role
/// selector.
#[cfg(target_os = "macos")]
const PRIO_DARWIN_ROLE: libc::c_int = 6;

/// `PRIO_DARWIN_ROLE_UI_FOCAL`: the role LaunchServices gives the frontmost app.
#[cfg(target_os = "macos")]
const ROLE_UI_FOCAL: libc::c_int = 1;

/// The scheduling band the subject runs in: aterm.app's, whatever launched the measurement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Band {
    /// The subject holds `UI_FOCAL`, aterm.app's band when frontmost.
    Focal,
    /// The kernel refused the role, with its errno (a DAEMON-type task, such as a launchd job's, gets `ENOTSUP`): the subject runs in the role-less band.
    Refused(i32),
    /// Not macOS: there are no darwin roles.
    NoRoles,
}

impl Band {
    /// Give `pid`, a child of this process, aterm.app's band.
    #[must_use]
    pub fn take(pid: u32) -> Self {
        Self::take_role(pid)
    }

    /// Give this process, the bench, aterm.app's band: the driver and the sampler then run in the
    /// subject's band, not the role-less one every other session's builds run in.
    #[must_use]
    pub fn take_own() -> Self {
        Self::take_role(0)
    }

    /// `setpriority(PRIO_DARWIN_ROLE, who, UI_FOCAL)`; `who` 0 is this process.
    fn take_role(who: u32) -> Self {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: `setpriority` takes no pointers.
            if unsafe { libc::setpriority(PRIO_DARWIN_ROLE, who, ROLE_UI_FOCAL) } == 0 {
                Self::Focal
            } else {
                Self::Refused(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = who;
            Self::NoRoles
        }
    }
}

impl std::fmt::Display for Band {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Focal => f.write_str("ui_focal"),
            Self::Refused(errno) => {
                write!(f, "refused({})", std::io::Error::from_raw_os_error(*errno))
            }
            Self::NoRoles => f.write_str("no_roles"),
        }
    }
}

/// How an instance runs: `--headless` on its lifeline, or in a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Face {
    Headless,
    Windowed,
}

/// A running instance.
pub struct Instance {
    child: spawnfd::Child,
    /// A headless instance's lifeline; a window takes none.
    _lifeline: Option<aterm_uds::lifeline::Lifeline>,
    pub log: PathBuf,
    pub ctl: Ctl,
    /// The band the subject runs in.
    pub band: Band,
}

impl Instance {
    /// Launch `bin --headless` in `world` and wait until its control socket listens and its session is ready.
    /// # Errors
    /// The launch failed, the instance exited before it listened (its log tail is in the reason), or it never listened.
    pub fn launch(bin: &Path, world: &World, budget: Budget) -> Result<Self, CouldNotRun> {
        Self::start(bin, world, budget, Face::Headless)
    }

    /// [`Instance::launch`] with a window: AppKit's loop and its proxies, which `--headless`
    /// never runs. A window takes no lifeline (aterm refuses `--lifeline-fd` without
    /// `--headless`: a window is a person's to close), so it is this process's child in this
    /// process's group, ended on drop by its pid with the helpers it started
    /// ([`end_with_helpers`]), and with the group when `verify`'s ceiling or an interrupt ends the
    /// run.
    /// # Errors
    /// As [`Instance::launch`].
    pub fn launch_windowed(bin: &Path, world: &World, budget: Budget) -> Result<Self, CouldNotRun> {
        Self::start(bin, world, budget, Face::Windowed)
    }

    fn start(bin: &Path, world: &World, budget: Budget, face: Face) -> Result<Self, CouldNotRun> {
        let log = std::fs::File::create(&world.log).map_err(|e| {
            CouldNotRun::Refused(format!("cannot create {}: {e}", world.log.display()))
        })?;
        let err = log
            .try_clone()
            .map_err(|e| CouldNotRun::Refused(format!("cannot share the instance log: {e}")))?;
        let mut cmd = spawnfd::command(bin);
        cmd.env_clear()
            .envs(world.env())
            .current_dir(&world.home)
            .stdout(log)
            .stderr(err);
        let lifeline = match face {
            Face::Headless => {
                let (lifeline, far) =
                    aterm_uds::lifeline::Lifeline::open(&world.root).map_err(|e| {
                        CouldNotRun::Refused(format!("the lifeline could not be made: {e}"))
                    })?;
                cmd.args(["--headless", "--no-reroute"])
                    .arg(aterm_uds::lifeline::FLAG)
                    .arg(aterm_uds::lifeline::STDIN.to_string())
                    .stdin(far);
                Some(lifeline)
            }
            Face::Windowed => {
                cmd.arg("--no-reroute").stdin(spawnfd::Stdio::null());
                None
            }
        };
        cmd.arg("--control-sock").arg(&world.sock);
        let child = cmd
            .spawn()
            .map_err(|e| CouldNotRun::Refused(format!("cannot launch {}: {e}", bin.display())))?;
        let band = Band::take(child.id());
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
            band,
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
            CouldNotRun::Refused(format!(
                "the instance exited ({status}) before its control socket listened; log tail:\n{}",
                inst.log_tail()
            ))
        })?;
        inst.ctl.ok(&["ready"])?;
        Ok(inst)
    }

    /// The instance's process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
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
        end_with_helpers(&mut self.child);
    }
}

/// How long a helper an instance left has after its `SIGTERM` before its `SIGKILL`.
const HELPER_GRACE: Duration = Duration::from_secs(2);

/// Kill and reap `child`, an instance this process launched, then end the children it had in this
/// process's group. Once launchd adopts them they outlive it: a window's consent sentinel
/// (`log stream … --timeout 15m`) would run for a quarter of an hour, and the stage's group sweep
/// names it. Each child is listed with its identity while the instance lives, and ended once the
/// instance is reaped through [`aterm_uds::process::end_proven`], which re-reads the identity
/// before each signal and signals only a process still in this group, so a pid reused meanwhile is
/// never signalled. A child that set up a group of its own (a session's shell) is not this
/// process's to end. One the instance starts between the listing and its kill is not listed; the
/// stage's sweep names it.
fn end_with_helpers(child: &mut spawnfd::Child) {
    let helpers = children_of(child.id());
    let _ = child.kill();
    let _ = child.wait();
    for helper in helpers {
        let _ = aterm_uds::process::end_proven(
            helper,
            HELPER_GRACE,
            &mut aterm_uds::process::EndControlLive,
            |id| in_this_group(id.pid),
        );
    }
}

/// `pid`'s children, each with its identity.
#[cfg(target_vendor = "apple")]
fn children_of(pid: u32) -> Vec<aterm_uds::process::Identity> {
    i32::try_from(pid)
        .ok()
        .and_then(aterm_uds::process::child_pids)
        .unwrap_or_default()
        .into_iter()
        .filter_map(aterm_uds::process::read_identity)
        .collect()
}

/// No child lister is wired here: the instance is ended alone.
#[cfg(not(target_vendor = "apple"))]
fn children_of(_pid: u32) -> Vec<aterm_uds::process::Identity> {
    Vec::new()
}

/// Whether `pid` is in this process's group.
fn in_this_group(pid: i32) -> bool {
    // SAFETY: `getpgrp` takes no arguments and cannot fail.
    let own = unsafe { libc::getpgrp() };
    aterm_uds::process::read_lineage(pid)
        .is_some_and(|lineage| i32::try_from(lineage.pgid).is_ok_and(|pgid| pgid == own))
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

    /// The server's refusal of `verb` itself, wherever the client printed it (on stderr it
    /// leads with `aterm-ctl: `): it does not know the verb (`ERR unknown verb …`), or only a
    /// debug build answers it (`ERR <verb> needs a debug build`).
    #[must_use]
    pub fn refuses_verb(&self, verb: &str) -> Option<&str> {
        let debug_only = format!("ERR {verb} needs a debug build");
        self.stdout
            .lines()
            .chain(self.stderr.lines())
            .map(|line| {
                let line = line.trim();
                line.strip_prefix("aterm-ctl: ").unwrap_or(line)
            })
            .find(|line| line.starts_with("ERR unknown verb") || *line == debug_only)
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
    #[must_use]
    pub fn call(&self, args: &[&str]) -> Reply {
        let Some(left) = self.budget.left() else {
            return Reply {
                code: None,
                stdout: String::new(),
                stderr: self
                    .budget
                    .spent(&format!("ctl {}", args.join(" ")))
                    .to_string(),
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
    /// # Errors
    /// The request failed; the reason quotes it. It is [`CouldNotRun::Unanswered`] when the subject
    /// refuses the verb itself ([`Reply::refuses_verb`]).
    pub fn ok(&self, args: &[&str]) -> Result<String, CouldNotRun> {
        let reply = self.call(args);
        if reply.ok() {
            Ok(reply.line().to_string())
        } else if self.budget.left().is_none() {
            Err(self.budget.spent(&format!("ctl {}", args.join(" "))))
        } else if let Some(refusal) = reply.refuses_verb(args.first().copied().unwrap_or_default())
        {
            unanswered(format!(
                "ctl {}: the subject has no such verb ({refusal})",
                args.join(" ")
            ))
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
    /// # Errors
    /// The request failed, or its line has no such field ([`CouldNotRun::Unanswered`]).
    pub fn field(&self, args: &[&str], key: &str) -> Result<String, CouldNotRun> {
        let line = self.ok(args)?;
        field(&line, key).map(str::to_string).ok_or_else(|| {
            CouldNotRun::Unanswered(format!(
                "ctl {} reported no {key}= ({line})",
                args.join(" ")
            ))
        })
    }
}

/// The session's replay checks as `temporal status` counts them, `witnessed=<n> diverged=<n>` (`-` for a count it does not report).
#[must_use]
pub fn replay_checks(ctl: &Ctl) -> String {
    let reply = ctl.call(&["temporal", "status"]);
    let line = if reply.ok() { reply.line() } else { "" };
    format!(
        "witnessed={} diverged={}",
        field(line, "witnessed").unwrap_or("-"),
        field(line, "diverged").unwrap_or("-")
    )
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
    fn a_verb_the_subject_lacks_is_told_from_other_refusals() {
        let reply = |stdout: &str, stderr: &str| Reply {
            code: Some(1),
            stdout: stdout.into(),
            stderr: stderr.into(),
        };
        let unknown = reply("ERR unknown verb (try: help)\n", "");
        assert_eq!(
            unknown.refuses_verb("rewind"),
            Some("ERR unknown verb (try: help)")
        );
        let unknown = reply("", "aterm-ctl: ERR unknown verb (try: help)\n");
        assert_eq!(
            unknown.refuses_verb("rewind"),
            Some("ERR unknown verb (try: help)")
        );
        assert!(
            reply("ERR aos needs a debug build\n", "")
                .refuses_verb("aos")
                .is_some()
        );
        for other in [
            reply("ERR aos bundle needs a debug build\n", ""),
            reply("ERR aos attach refused: no window\n", ""),
            reply("", "aterm-ctl: connect: No such file or directory\n"),
        ] {
            assert_eq!(other.refuses_verb("aos"), None, "{other:?}");
        }
    }

    /// A refused verb is unanswered; any other failure is refused.
    #[test]
    fn a_request_the_subject_has_no_verb_for_is_unanswered() {
        let client = |bin: &str| Ctl {
            bin: PathBuf::from(bin),
            sock: PathBuf::from("/nonexistent.sock"),
            home: PathBuf::from("/"),
            budget: Budget::from_now(60),
        };
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let stand_in = dir.path().join("refuses");
        std::fs::write(
            &stand_in,
            "#!/bin/sh\necho 'aterm-ctl: ERR unknown verb (try: help)' >&2\nexit 1\n",
        )
        .expect("write");
        std::fs::set_permissions(
            &stand_in,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("chmod");
        let refused = client(stand_in.to_str().expect("utf-8")).ok(&["rewind", "open"]);
        assert!(
            matches!(&refused, Err(CouldNotRun::Unanswered(why)) if why.contains("ctl rewind open")),
            "{refused:?}"
        );
        let failed = client("/usr/bin/false").ok(&["rewind", "open"]);
        assert!(matches!(failed, Err(CouldNotRun::Refused(_))), "{failed:?}");
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

    /// A child starts with no role (the band the subject ran in before), and
    /// [`Band::take`] moves it to `UI_FOCAL`, or names the kernel's refusal
    /// with the child still role-less.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_child_takes_the_app_band_or_names_the_refusal() {
        // SAFETY: `getpriority` takes no pointers.
        let role = |pid: u32| unsafe { libc::getpriority(PRIO_DARWIN_ROLE, pid) };
        let mut child = spawnfd::command("/bin/sleep")
            .arg("60")
            .stdin(spawnfd::Stdio::null())
            .stdout(spawnfd::Stdio::null())
            .stderr(spawnfd::Stdio::null())
            .spawn()
            .expect("spawn a sleep");
        let pid = child.id();
        let before = role(pid);
        let band = Band::take(pid);
        let after = role(pid);
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(before, 0, "a spawned child holds no role");
        match band {
            Band::Focal => assert_eq!(after, ROLE_UI_FOCAL, "the child holds UI_FOCAL"),
            Band::Refused(errno) => {
                assert!(
                    errno == libc::EPERM || errno == libc::ENOTSUP,
                    "the kernel's refusal, not a wrong request: {}",
                    Band::Refused(errno)
                );
                assert_eq!(after, 0, "a refused child keeps no role");
            }
            Band::NoRoles => panic!("macOS has darwin roles"),
        }
    }

    /// A window's consent sentinel outlived the window in the bench's group. The stand-in's helper
    /// is a subshell that never execs (so the identity listed is the one it keeps) blocked reading
    /// a pipe this test holds: only the drop can end it.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn an_instance_ends_with_the_helper_it_started() {
        use std::io::BufRead as _;
        let mut child = spawnfd::command("/bin/sh")
            .args([
                "-c",
                "exec 3<&0; (read held <&3) & echo $!; exec /bin/sleep 600 3<&-",
            ])
            .stdin(spawnfd::Stdio::piped())
            .stdout(spawnfd::Stdio::piped())
            .stderr(spawnfd::Stdio::null())
            .spawn()
            .expect("spawn the stand-in instance");
        let held = child.stdin.take().expect("the helper's held input");
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().expect("piped stdout"))
            .read_line(&mut line)
            .expect("the helper's pid");
        let helper: i32 = line.trim().parse().expect("a pid");
        end_with_helpers(&mut child);
        let presence = aterm_uds::process::process_presence(helper);
        drop(held);
        assert!(
            matches!(
                presence,
                aterm_uds::process::ProcessPresence::Absent
                    | aterm_uds::process::ProcessPresence::Exited
            ),
            "the helper (pid {helper}) outlived its instance: {presence:?}"
        );
    }

    /// The bench takes aterm.app's band itself ([`Band::take_own`], the first thing
    /// [`super::super::paint_probe::measure`] does): this process then holds `UI_FOCAL`, or the
    /// kernel's refusal is named with the process still in the role it had.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_bench_takes_the_app_band_itself() {
        // SAFETY: `getpriority` takes no pointers.
        let role = || unsafe { libc::getpriority(PRIO_DARWIN_ROLE, 0) };
        let before = role();
        let band = Band::take_own();
        let after = role();
        match band {
            Band::Focal => assert_eq!(after, ROLE_UI_FOCAL, "the bench holds UI_FOCAL"),
            Band::Refused(errno) => {
                assert!(
                    errno == libc::EPERM || errno == libc::ENOTSUP,
                    "the kernel's refusal, not a wrong request: {}",
                    Band::Refused(errno)
                );
                assert_eq!(after, before, "a refused bench keeps its role");
            }
            Band::NoRoles => panic!("macOS has darwin roles"),
        }
    }
}
