// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm machines person` and `screen` flows over Screen Sharing.

use super::inventory::Paths;
use super::ssh::{self, Target};
use crate::agent::watch;
use crate::envelope::Exec;
use crate::items::person::{self, Kind, Known, PersonStep};
use crate::model::{HostReport, Outcome, Probe, User};
use crate::port::{Cmd, Ran, RunFailed, System};
use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

/// How long one wait for a person may last before the flow decides nobody is there.
pub const PERSON_HANG: Duration = Duration::from_secs(15 * 60);

/// The pause between two re-probes while a person works.
pub const POLL: Duration = Duration::from_secs(2);

const EXEC_BUDGET_SECS: u64 = 30;

/// One step the flow walks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Which switch.
    pub step: PersonStep,
    /// The report read it outstanding.
    pub measured: bool,
    /// Why it could not be read, when it was not measured.
    pub why: Option<String>,
}

impl Step {
    /// The step id (`full_disk_access:aterm`).
    #[must_use]
    pub fn id(&self) -> String {
        self.step.id()
    }

    /// The pane's URL.
    #[must_use]
    pub const fn url(&self) -> &'static str {
        self.step.kind.pane()
    }

    /// What the person does.
    #[must_use]
    pub fn how(&self) -> String {
        self.step.how()
    }
}

/// The outstanding person steps in `report`, in the order a person does them.
#[must_use]
pub fn prepare_steps(report: &HostReport) -> Vec<Step> {
    let mut steps: Vec<Step> = report
        .items
        .iter()
        .filter(|row| row.family == "person")
        .filter_map(|row| {
            let measured = match row.outcome {
                Outcome::Person => true,
                Outcome::Unknown => false,
                _ => return None,
            };
            let id = row.id.strip_prefix("person.")?;
            Some(Step {
                step: PersonStep::parse(id).ok()?,
                measured,
                why: if measured { None } else { row.why.clone() },
            })
        })
        .collect();
    steps.sort_by_key(|s| s.step.kind);
    steps
}

/// `steps` narrowed to the one step `id` (`screen <host> <step>`).
/// # Errors
/// One line when `id` is not a step id.
pub fn only(steps: Vec<Step>, id: &str) -> Result<Vec<Step>, String> {
    let wanted = PersonStep::parse(id)?;
    Ok(vec![
        steps
            .into_iter()
            .find(|s| s.step == wanted)
            .unwrap_or(Step {
                step: wanted,
                measured: false,
                why: Some("the report does not list it outstanding".into()),
            }),
    ])
}

/// The account `name` as the host reported it.
/// # Errors
/// One line when the report's facts do not list it.
pub fn account(report: &HostReport, name: &str) -> Result<User, String> {
    report
        .facts
        .users
        .iter()
        .find(|u| u.name == name)
        .cloned()
        .ok_or_else(|| {
            format!(
                "the host's last report lists no account {name}: run aterm machines plan on it first"
            )
        })
}

/// Refuse `verb` unless a person is there: standard input and standard error are terminals.
/// # Errors
/// The refusal, one line.
pub fn require_terminals(verb: &str, stdin_tty: bool, stderr_tty: bool) -> Result<(), String> {
    if stdin_tty && stderr_tty {
        Ok(())
    } else {
        Err(format!(
            "aterm machines {verb}: not run: it walks a person through System Settings, and \
             standard input or standard error is not a terminal"
        ))
    }
}

/// The host, as the flow reaches it.
pub trait Remote {
    /// Run `exec` on the host as one signed request and return how it ended and what it printed.
    /// # Errors
    /// One line when the request did not run (unreachable, refused, not signed).
    fn exec(&self, exec: &Exec) -> Result<Ran, String>;
}

/// Opens a URL on the controller's own screen.
pub trait Opener {
    /// Open `url`.
    /// # Errors
    /// One line when it did not open.
    fn open(&self, url: &str) -> Result<(), String>;
}

/// The live opener: `/usr/bin/open <url>` through the port.
pub struct LiveOpener<'a> {
    /// The controller's port.
    pub sys: &'a dyn System,
}

impl Opener for LiveOpener<'_> {
    fn open(&self, url: &str) -> Result<(), String> {
        match self.sys.run(&Cmd::new("/usr/bin/open").arg(url)) {
            Ok(ran) if ran.success() => Ok(()),
            Ok(ran) => Err(format!("open {url}: {}", ran.describe())),
            Err(e) => Err(format!("open {url}: {e}")),
        }
    }
}

/// Time, as the flow's waits see it.
pub trait Clock {
    /// Time since some fixed start.
    fn elapsed(&self) -> Duration;
    /// Wait `d`.
    fn pause(&self, d: Duration);
}

/// The wall clock.
#[derive(Debug)]
pub struct WallClock {
    start: Instant,
}

impl WallClock {
    /// A clock starting now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for WallClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for WallClock {
    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    fn pause(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// `cmd` as an `exec` payload.
/// # Errors
/// One line when `cmd` needs what `exec` cannot carry (a login session, a variable, input).
pub fn exec_of(cmd: &Cmd) -> Result<Exec, String> {
    if cmd.session || !cmd.env.is_empty() || cmd.stdin.is_some() {
        return Err(format!(
            "{} cannot travel as an exec request",
            cmd.display()
        ));
    }
    let mut argv = vec![cmd.program.to_string_lossy().into_owned()];
    argv.extend(cmd.args.iter().map(|a| a.to_string_lossy().into_owned()));
    Ok(Exec {
        argv,
        user: cmd.user.as_ref().map(|u| u.name.clone()),
        budget_secs: cmd.budget.as_secs().max(1),
    })
}

/// Probe `step` on the host now, through `remote`, with nothing assumed: every read is fresh.
pub fn reprobe(remote: &dyn Remote, step: &PersonStep) -> Probe {
    person::probe_step(step, Known::default(), &mut |cmd| {
        let exec = exec_of(cmd).map_err(RunFailed::Refused)?;
        remote
            .exec(&exec)
            .map_err(|why| RunFailed::Spawn(format!("exec on the host: {why}")))
    })
}

fn in_session(user: &User, program: &str, args: &[&str]) -> Exec {
    let shell = if user.shell.is_empty() {
        "/bin/zsh"
    } else {
        user.shell.as_str()
    };
    let mut argv = vec![
        person::LAUNCHCTL.to_owned(),
        "asuser".to_owned(),
        user.uid.to_string(),
        "/usr/bin/sudo".to_owned(),
        "-n".to_owned(),
        "-u".to_owned(),
        user.name.clone(),
        "-H".to_owned(),
        "--".to_owned(),
        "/usr/bin/env".to_owned(),
        "-i".to_owned(),
        format!("HOME={}", user.home),
        format!("USER={}", user.name),
        format!("LOGNAME={}", user.name),
        format!("SHELL={shell}"),
        "PATH=/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
        program.to_owned(),
    ];
    argv.extend(args.iter().map(|a| (*a).to_owned()));
    Exec {
        argv,
        user: None,
        budget_secs: EXEC_BUDGET_SECS,
    }
}

/// The request that opens `url` in `user`'s login session on the host.
#[must_use]
pub fn open_pane_cmd(user: &User, url: &str) -> Exec {
    in_session(user, "/usr/bin/open", &[url])
}

/// The request that locks `user`'s login session on the host.
#[must_use]
pub fn lock_cmd(user: &User) -> Exec {
    in_session(
        user,
        crate::protocol::AGENT_EXE,
        &["machines", "agent", "lock-screen"],
    )
}

/// The request that asks whether `user` has a login session.
#[must_use]
pub fn session_cmd(user: &User) -> Exec {
    Exec {
        argv: vec![
            person::LAUNCHCTL.to_owned(),
            "print".to_owned(),
            format!("gui/{}", user.uid),
        ],
        user: None,
        budget_secs: EXEC_BUDGET_SECS,
    }
}

/// The request that arms the host's screen watch for `user` before anything is relayed.
#[must_use]
pub fn arm_cmd(user: &User) -> Exec {
    let uid = user.uid.to_string();
    Exec {
        argv: [
            crate::protocol::AGENT_EXE,
            "machines",
            "agent",
            "screen-watch",
            "arm",
            "--uid",
            uid.as_str(),
            "--user",
            user.name.as_str(),
        ]
        .map(str::to_owned)
        .to_vec(),
        user: None,
        budget_secs: EXEC_BUDGET_SECS,
    }
}

/// The request that disarms the host's screen watch `id`: the agent's `screen-watch disarm`.
#[must_use]
pub fn disarm_cmd(id: &str) -> Exec {
    Exec {
        argv: [
            crate::protocol::AGENT_EXE,
            "machines",
            "agent",
            "screen-watch",
            "disarm",
            id,
        ]
        .map(str::to_owned)
        .to_vec(),
        user: None,
        budget_secs: EXEC_BUDGET_SECS,
    }
}

fn first_line(ran: &Ran) -> String {
    ran.out()
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// Arm the host's screen watch for `admin` ([`arm_cmd`]); the arm's id.
/// # Errors
/// Why the screen watch is not armed, how the run ended, or that the request did not run.
fn arm_watch(remote: &dyn Remote, admin: &User) -> Result<String, String> {
    let ran = remote
        .exec(&arm_cmd(admin))
        .map_err(|why| format!("the request did not run: {why}"))?;
    let line = first_line(&ran);
    if ran.success()
        && let Some(id) = line
            .strip_prefix("armed ")
            .and_then(|rest| rest.split(' ').next())
            .filter(|id| crate::agent::is_run_id(id))
    {
        return Ok(id.to_owned());
    }
    Err(match line.strip_prefix("not armed: ") {
        Some(why) => why.to_owned(),
        None if line.is_empty() => ran.describe(),
        None => format!("{}: {line}", ran.describe()),
    })
}

fn disarm_watch(remote: &dyn Remote, say: &dyn Fn(&str), admin: &User, id: &str) {
    let failed = match remote.exec(&disarm_cmd(id)) {
        Ok(ran) if ran.success() => {
            let line = first_line(&ran);
            if let Some(ended) = line
                .strip_prefix(&format!("disarmed {id} (it had ended: "))
                .and_then(|rest| rest.strip_suffix(')'))
            {
                say(&format!(
                    "the host's screen watch had already ended: {ended}"
                ));
            }
            return;
        }
        Ok(ran) => match first_line(&ran) {
            line if line.is_empty() => ran.describe(),
            line => line
                .strip_prefix("not disarmed: ")
                .map_or_else(|| format!("{}: {line}", ran.describe()), str::to_owned),
        },
        Err(why) => format!("the request did not run: {why}"),
    };
    say(&format!(
        "the host's screen watch was not disarmed ({failed}): once no request has reached the \
         host for {}s it locks {}'s session if there is one, which is harmless",
        watch::QUIET.as_secs(),
        admin.name
    ));
}

/// Interval between requests before a walk with the screen watch armed re-arms the watch.
pub const REARM: Duration = Duration::from_secs(watch::QUIET.as_secs() / 2);

struct Watched<'a> {
    remote: &'a dyn Remote,
    clock: &'a dyn Clock,
    say: &'a dyn Fn(&str),
    admin: &'a User,
    /// The last acknowledged arm, retained until its replacement is acknowledged.
    id: RefCell<Option<String>>,
    /// When the last request that ran came back; `None` after a replacement refusal,
    /// so the next successful request retries it and no live replacement is promised.
    ran: Cell<Option<Duration>>,
}

impl Watched<'_> {
    fn id(&self) -> Option<String> {
        self.id.borrow().clone()
    }

    fn arm_again(&self) {
        let old = self.id.borrow().clone();
        let pending = self.ran.get().is_none();
        match arm_watch(self.remote, self.admin) {
            Ok(id) => {
                *self.id.borrow_mut() = Some(id.clone());
                self.ran.set(Some(self.clock.elapsed()));
                if let Some(old) = &old
                    && old != &id
                {
                    disarm_watch(self.remote, self.say, self.admin, old);
                }
                if old.is_none() || pending {
                    (self.say)("the host's screen watch is armed again");
                }
            }
            Err(why) => {
                self.ran.set(None);
                if old.is_some() && !pending {
                    (self.say)(&format!(
                        "the host's screen watch was not armed again ({why}): if aterm is killed or \
                         loses the Mac before the walk's own lock, {}'s session stays as it is left; \
                         tried again at the next request",
                        self.admin.name
                    ));
                }
            }
        }
    }
}

impl Remote for Watched<'_> {
    fn exec(&self, exec: &Exec) -> Result<Ran, String> {
        let got = self.remote.exec(exec);
        if got.is_ok() {
            let now = self.clock.elapsed();
            if self
                .ran
                .get()
                .is_none_or(|last| now.saturating_sub(last) >= REARM)
                || self.id.borrow().is_none()
            {
                self.arm_again();
            } else {
                self.ran.set(Some(now));
            }
        }
        got
    }
}

/// What each connection to `screen`'s relay runs.
/// # Errors
/// One line for a target `ssh` could misread.
pub fn relay_cmd(paths: &Paths, target: &Target) -> Result<Cmd, String> {
    ssh::request_cmd(paths, target, "screen").map_err(|e| e.to_string())
}

/// What Screen Sharing.app opens: the admin at the relay's local end.
#[must_use]
pub fn vnc_url(admin: &str, local_port: u16) -> String {
    format!("vnc://{admin}@127.0.0.1:{local_port}")
}

/// Whether something on the host answers `relay`'s request with an RFB greeting.
/// # Errors
/// One line: the host's own `refused: …`, `ssh`'s error, or what replaced the greeting.
pub fn screen_answers(sys: &dyn System, relay: &Cmd) -> Result<(), String> {
    let ran = sys
        .run(&relay.clone().budget(Duration::from_secs(EXEC_BUDGET_SECS)))
        .map_err(|e| e.to_string())?;
    if ran.success() && ran.stdout.starts_with(b"RFB ") {
        return Ok(());
    }
    let first = ran.out().lines().next().unwrap_or_default().to_owned();
    if first.starts_with("refused: ") {
        return Err(first);
    }
    if ran.success() {
        return Err(format!("no RFB greeting came back: {first:?}"));
    }
    Err(ran.describe())
}

/// What became of one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    /// It already read granted; no pane was opened.
    AlreadyGranted,
    /// The person granted it while the flow waited.
    Granted,
    /// It needs the Mac's own keyboard (`person <host> --here`): what to do there.
    AtConsole(String),
    /// It did not end granted, and why.
    NotGranted(String),
}

/// One step's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Walked {
    /// The step id.
    pub step: String,
    /// What became of it.
    pub done: Done,
}

/// What the walk needs.
pub struct Flow<'a> {
    /// The controller's port (`screen`'s check and the relay it opens).
    pub sys: &'a dyn System,
    /// The host.
    pub remote: &'a dyn Remote,
    /// Time.
    pub clock: &'a dyn Clock,
    /// Where progress lines for the person go.
    pub say: &'a dyn Fn(&str),
    /// The hang detector's bound ([`PERSON_HANG`] outside tests).
    pub hang: Duration,
}

/// Wait until `step` reads granted on the host, re-probing every [`POLL`].
/// # Errors
/// The last reading once [`Flow::hang`] passes, or the signal that stopped the walk.
pub fn await_granted(flow: &Flow<'_>, step: &PersonStep) -> Result<String, String> {
    let start = flow.clock.elapsed();
    loop {
        let last = match reprobe(flow.remote, step) {
            Probe::Ok { now } => return Ok(now),
            Probe::Person { how, .. } => format!("still outstanding: {how}"),
            Probe::Unusable { why } | Probe::Blocked { why } => why,
            other => format!("{other:?}"),
        };
        if let Some(sig) = flow.sys.stopped() {
            return Err(stopped_by(sig));
        }
        if flow.clock.elapsed().saturating_sub(start) >= flow.hang {
            return Err(format!(
                "{}: not granted after {}s ({last})",
                step.id(),
                flow.hang.as_secs()
            ));
        }
        flow.clock.pause(POLL);
    }
}

/// Open `url` in `admin`'s session ([`open_pane_cmd`]).
/// # Errors
/// How the open ran when it failed, the last reason it did not run, or the stopping signal.
fn open_pane(flow: &Flow<'_>, admin: &User, url: &str) -> Result<(), String> {
    let start = flow.clock.elapsed();
    loop {
        let why = match flow.remote.exec(&open_pane_cmd(admin, url)) {
            Ok(ran) if ran.success() => return Ok(()),
            Ok(ran) => return Err(ran.describe()),
            Err(why) => format!("the request did not run: {why}"),
        };
        if let Some(sig) = flow.sys.stopped() {
            return Err(format!("{} ({why})", stopped_by(sig)));
        }
        if flow.clock.elapsed().saturating_sub(start) >= flow.hang {
            return Err(format!("{why} (sent for {}s)", flow.hang.as_secs()));
        }
        flow.clock.pause(POLL);
    }
}

fn stopped_by(sig: i32) -> String {
    format!("stopped by {}", crate::port::stop_name(sig))
}

/// One reading of whether `admin` has a login session ([`session_cmd`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    /// `launchctl` found it, locked or not.
    There,
    /// `launchctl` answered that there is none: what it said.
    Absent(String),
}

/// Read whether `admin` has a login session now.
/// # Errors
/// Why the request did not run (the host unreachable, the envelope refused): it read nothing.
pub fn read_session(flow: &Flow<'_>, admin: &User) -> Result<Reading, String> {
    match flow.remote.exec(&session_cmd(admin)) {
        Ok(ran) if ran.success() => Ok(Reading::There),
        Ok(ran) if ran.status.is_none() => Err(format!(
            "the session query did not exit normally: {}",
            ran.describe()
        )),
        Ok(ran) => Ok(Reading::Absent(format!(
            "launchctl print gui/{}: {}",
            admin.uid,
            ran.describe()
        ))),
        Err(why) => Err(format!("the request did not run: {why}")),
    }
}

/// How the admin's session came to be there when [`await_session`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// No reading said it was missing, the one before the wait included.
    Existing,
    /// A reading found no session and a later one found it.
    LoggedIn,
}

/// Wait until `admin` has a login session on the host, and say whether a reading found none first.
/// # Errors
/// One line once [`Flow::hang`] passes without one, or when a signal stopped the walk.
pub fn await_session(flow: &Flow<'_>, admin: &User, mut absent: bool) -> Result<Found, String> {
    await_session_observed(flow, admin, &mut absent)
}

fn await_session_observed(
    flow: &Flow<'_>,
    admin: &User,
    absent: &mut bool,
) -> Result<Found, String> {
    let start = flow.clock.elapsed();
    let mut said = false;
    loop {
        let last = match read_session(flow, admin) {
            Ok(Reading::There) => {
                return Ok(if *absent {
                    Found::LoggedIn
                } else {
                    Found::Existing
                });
            }
            Ok(Reading::Absent(what)) => {
                *absent = true;
                what
            }
            Err(why) => why,
        };
        if let Some(sig) = flow.sys.stopped() {
            return Err(format!(
                "{} while waiting for {} to log in ({last})",
                stopped_by(sig),
                admin.name
            ));
        }
        if !said {
            (flow.say)(&format!(
                "waiting for {} to log in at the Mac ({last})",
                admin.name
            ));
            said = true;
        }
        if flow.clock.elapsed().saturating_sub(start) >= flow.hang {
            return Err(format!(
                "{} has not logged in on the host after {}s ({last})",
                admin.name,
                flow.hang.as_secs()
            ));
        }
        flow.clock.pause(POLL);
    }
}

fn walk(flow: &Flow<'_>, admin: &User, steps: &[Step], at_console: bool) -> Vec<Walked> {
    let mut out = Vec::with_capacity(steps.len());
    let mut stopped: Option<String> = None;
    for s in steps {
        let step = s.id();
        if let Some(sig) = flow.sys.stopped() {
            out.push(Walked {
                step,
                done: Done::NotGranted(format!("not attempted: {}", stopped_by(sig))),
            });
            continue;
        }
        if let Some(on) = &stopped {
            out.push(Walked {
                step,
                done: Done::NotGranted(format!("not attempted: {on} was not granted")),
            });
            continue;
        }
        if s.step.kind.at_console() && !at_console {
            out.push(Walked {
                step,
                done: Done::AtConsole(s.how()),
            });
            continue;
        }
        if let Probe::Ok { .. } = reprobe(flow.remote, &s.step) {
            (flow.say)(&format!("{step}: already granted"));
            out.push(Walked {
                step,
                done: Done::AlreadyGranted,
            });
            continue;
        }
        if let Err(why) = open_pane(flow, admin, s.url()) {
            stopped = Some(step.clone());
            out.push(Walked {
                step,
                done: Done::NotGranted(format!("the pane did not open: {why}")),
            });
            continue;
        }
        (flow.say)(&format!("{step}: {} \u{2014} waiting", s.how()));
        let done = match await_granted(flow, &s.step) {
            Ok(_) => {
                (flow.say)(&format!("{step}: granted"));
                Done::Granted
            }
            Err(why) => {
                stopped = Some(step.clone());
                Done::NotGranted(why)
            }
        };
        out.push(Walked { step, done });
    }
    out
}

/// The terminals `screen` and `person --here` need, as booleans so a test can drive them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terminals {
    /// Standard input is a terminal.
    pub stdin: bool,
    /// Standard error is a terminal.
    pub stderr: bool,
}

/// The Screen Sharing step `steps` lists measured outstanding.
#[must_use]
pub fn screen_sharing_outstanding(steps: &[Step]) -> Option<&Step> {
    steps
        .iter()
        .find(|s| s.measured && s.step.kind == Kind::ScreenSharing)
}

/// The refusal for a host at `address` whose Screen Sharing `step` is outstanding.
#[must_use]
pub fn screen_sharing_off(address: &str, step: &Step) -> String {
    format!(
        "Screen Sharing is not on with control on {address}: {}; do it at the Mac (aterm \
         machines person <host> --here)",
        step.how()
    )
}

/// What `screen` left the admin's session as at the Mac's display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    /// It was there before the relay opened, so it is macOS's to keep as it was.
    Kept,
    /// The admin logged in over the relay, and `screen` locked the session after it.
    Locked,
    /// The lock after the walk did not read back.
    Unlocked {
        /// Why the lock failed.
        why: String,
        /// Why the last reading did not run, when none saw the login.
        unread: Option<String>,
        /// The host's screen watch stays armed, to try the lock itself.
        watched: bool,
    },
}

impl Session {
    /// The one line `screen` ends with when it left the Mac logged in and unlocked, or may have.
    #[must_use]
    pub fn unlocked_line(&self, host: &str, admin: &str) -> Option<String> {
        let Self::Unlocked {
            why,
            unread,
            watched,
        } = self
        else {
            return None;
        };
        let then = if *watched {
            format!(
                "; the host's screen watch locks it once no request has reached the host for {}s",
                watch::QUIET.as_secs()
            )
        } else {
            String::new()
        };
        Some(match unread {
            None => format!(
                "{host}: the Mac is still logged in as {admin} and unlocked at its display: \
                 the lock after the walk failed ({why}){then}"
            ),
            Some(read) => format!(
                "{host}: the Mac may be logged in as {admin} and unlocked at its display: \
                 whether {admin} logged in over Screen Sharing is not known (the last reading: \
                 {read}), and the lock after the walk failed ({why}){then}"
            ),
        })
    }
}

/// What `screen` did: each step, and the session it left.
#[derive(Debug)]
pub struct Screened {
    /// Each step's result.
    pub walked: Vec<Walked>,
    /// The admin's session at the end.
    pub session: Session,
    /// The stops [`System::catch_stops`] caught from the relay's opening, still caught.
    pub stops: Box<dyn std::any::Any>,
}

fn lock_session(flow: &Flow<'_>, admin: &User, unread: Option<String>, watched: bool) -> Session {
    let why = match flow.remote.exec(&lock_cmd(admin)) {
        Ok(ran) if ran.success() => {
            (flow.say)(&format!(
                "locked {}'s session at the Mac again (logged in over Screen Sharing for this \
                 walk)",
                admin.name
            ));
            return Session::Locked;
        }
        Ok(ran) => match first_line(&ran) {
            line if line.is_empty() => ran.describe(),
            line => line
                .strip_prefix("not locked: ")
                .map_or_else(|| format!("{}: {line}", ran.describe()), str::to_owned),
        },
        Err(why) => format!("the request did not run: {why}"),
    };
    Session::Unlocked {
        why,
        unread,
        watched,
    }
}

fn logged_in_line(admin: &User, watched: bool) -> String {
    let otherwise = if watched {
        format!(
            "the host locks it itself once no request has reached it for {}s",
            watch::QUIET.as_secs()
        )
    } else {
        "the Mac stays logged in and unlocked at its display".to_owned()
    };
    format!(
        "{} logged in over Screen Sharing: the session is locked again when the walk ends or is \
         stopped; if aterm is killed or loses the Mac first, {otherwise}",
        admin.name
    )
}

fn decide(
    flow: &Flow<'_>,
    admin: &User,
    steps: &[Step],
    waited: Result<(Vec<Walked>, Found), String>,
    absent: bool,
    watched: bool,
) -> Result<(Vec<Walked>, Session), String> {
    let (walked, unread) = match waited {
        Ok((walked, Found::Existing)) => return Ok((walked, Session::Kept)),
        Ok((walked, Found::LoggedIn)) => (walked, None),
        Err(why) if !absent => return Err(why),
        Err(why) => {
            let unread = match read_session(flow, admin) {
                Ok(Reading::Absent(_)) => return Err(why),
                Ok(Reading::There) => None,
                Err(read) => Some(read),
            };
            let walked = if flow.sys.stopped().is_some() {
                walk(flow, admin, steps, false)
            } else {
                steps
                    .iter()
                    .map(|s| Walked {
                        step: s.id(),
                        done: Done::NotGranted(format!("not attempted: {why}")),
                    })
                    .collect()
            };
            (walked, unread)
        }
    };
    Ok((walked, lock_session(flow, admin, unread, watched)))
}

/// `aterm machines screen <host>`.
/// # Errors
/// One line when the flow could not start.
pub fn screen(
    flow: &Flow<'_>,
    paths: &Paths,
    target: &Target,
    admin: &User,
    steps: &[Step],
    opener: &dyn Opener,
    terminals: Terminals,
) -> Result<Screened, String> {
    require_terminals("screen", terminals.stdin, terminals.stderr)?;
    if let Some(s) = screen_sharing_outstanding(steps) {
        return Err(screen_sharing_off(&target.address, s));
    }
    if steps.is_empty() {
        (flow.say)("nothing outstanding");
        return Ok(Screened {
            walked: Vec::new(),
            session: Session::Kept,
            stops: Box::new(()),
        });
    }
    match reprobe(flow.remote, &PersonStep::of(Kind::ScreenSharing)) {
        Probe::Ok { .. } => {}
        Probe::Person { how, .. } => {
            return Err(format!(
                "Screen Sharing is not on with control on {}: {how}; do it at the Mac \
                 (aterm machines person <host> --here)",
                target.address
            ));
        }
        Probe::Unusable { why } | Probe::Blocked { why } => {
            return Err(format!(
                "Screen Sharing on {} could not be read ({why}); nothing was relayed",
                target.address
            ));
        }
        other => {
            return Err(format!(
                "Screen Sharing on {} reads {other:?}; nothing was relayed",
                target.address
            ));
        }
    }
    let mut absent = match read_session(flow, admin) {
        Ok(reading) => reading != Reading::There,
        Err(why) => {
            return Err(format!(
                "whether {} is logged in on {} could not be read ({why}); nothing was \
                 relayed",
                admin.name, target.address
            ));
        }
    };
    let cmd = relay_cmd(paths, target)?;
    screen_answers(flow.sys, &cmd)
        .map_err(|e| format!("Screen Sharing on {} did not answer: {e}", target.address))?;
    let watching = if absent {
        let id = arm_watch(flow.remote, admin).map_err(|why| {
            format!(
                "the screen watch on {} was not armed ({why}), so no lock could be promised \
                 if aterm is killed or loses the Mac; nothing was relayed",
                target.address
            )
        })?;
        Some(Watched {
            remote: flow.remote,
            clock: flow.clock,
            say: flow.say,
            admin,
            id: RefCell::new(Some(id)),
            ran: Cell::new(Some(flow.clock.elapsed())),
        })
    } else {
        None
    };
    let flow = &Flow {
        sys: flow.sys,
        remote: watching.as_ref().map_or(flow.remote, |w| w as &dyn Remote),
        clock: flow.clock,
        say: flow.say,
        hang: flow.hang,
    };
    let stops = flow.sys.catch_stops();
    let watched = watching.is_some();
    let ended = match flow.sys.relay(&cmd) {
        Err(e) => Err(format!("the relay to {} did not open: {e}", target.address)),
        Ok(mut relay) => {
            let waited = (|| {
                opener.open(&vnc_url(&target.user, relay.port()))?;
                (flow.say)(&format!(
                    "Screen Sharing is opening {}: log in as {}",
                    target.address, admin.name
                ));
                let found = await_session_observed(flow, admin, &mut absent)?;
                if found == Found::LoggedIn {
                    (flow.say)(&logged_in_line(admin, watched));
                }
                Ok::<_, String>((walk(flow, admin, steps, false), found))
            })();
            if let Err(e) = relay.close() {
                (flow.say)(&format!("the relay did not close cleanly: {e}"));
            }
            decide(flow, admin, steps, waited, absent, watched)
        }
    };
    let held = watching.as_ref().and_then(Watched::id);
    let current = watching
        .as_ref()
        .is_some_and(|watch| watch.ran.get().is_some());
    let ended = ended.map(|(walked, session)| match session {
        Session::Unlocked { why, unread, .. } => (
            walked,
            Session::Unlocked {
                why,
                unread,
                watched: held.is_some() && current,
            },
        ),
        session => (walked, session),
    });
    if let (Some(w), Some(id)) = (&watching, &held)
        && !matches!(&ended, Ok((_, Session::Unlocked { .. })))
    {
        disarm_watch(w.remote, flow.say, admin, id);
    }
    let (walked, session) = ended?;
    Ok(Screened {
        walked,
        session,
        stops,
    })
}

/// `aterm machines person <host> --here`.
/// # Errors
/// One line when the flow could not start: no terminal, or the admin never logged in.
pub fn here(
    flow: &Flow<'_>,
    admin: &User,
    steps: &[Step],
    terminals: Terminals,
) -> Result<Vec<Walked>, String> {
    require_terminals("person --here", terminals.stdin, terminals.stderr)?;
    if steps.is_empty() {
        (flow.say)("nothing outstanding");
        return Ok(Vec::new());
    }
    await_session(flow, admin, false)?;
    Ok(walk(flow, admin, steps, true))
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
