// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm machines person` and `screen`: the steps only a person can do, over Screen Sharing (design §6).

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

/// How long the host may take to open a pane or answer a read.
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

/// The outstanding person steps in `report`, in the order a person does them (Remote Login and its FDA switch, Screen Sharing, every Full Disk Access, then every Developer Tools entry).
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
///
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

/// The account `name` as the host reported it (its uid is what `launchctl asuser` and `gui/<uid>` take).
///
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

/// Refuse `verb` unless a person is there: standard input and standard error are terminals (design §10).
///
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
    /// Run `exec` on the host as one signed request and return how it ended and what it
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
///
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

/// The request that runs `program` with `args` as `user` inside `user`'s login session on
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

/// The request that opens `url` in `user`'s login session on the host (design §1.9, VM: it opened System Settings at the pane on the console).
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

/// The first line a request printed that is not blank.
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
/// Why it is not armed: the agent's own `not armed: <why>`, how the run ended and what it
/// printed (an agent from before the watch refuses the verb), or that the request did not run.
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

/// Disarm the host's screen watch `id` ([`disarm_cmd`]). A watcher that had ended first (it
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

/// How long a walk with the screen watch armed may go between two requests that ran before it arms the watch again.
pub const REARM: Duration = Duration::from_secs(watch::QUIET.as_secs() / 2);

/// `screen`'s requests to the host once its screen watch is armed. A watcher that locked
struct Watched<'a> {
    remote: &'a dyn Remote,
    clock: &'a dyn Clock,
    say: &'a dyn Fn(&str),
    admin: &'a User,
    /// The last acknowledged arm, retained until its replacement is acknowledged.
    id: RefCell<Option<String>>,
    /// When the last request that ran came back; `None` after a replacement refusal,
    ran: Cell<Option<Duration>>,
}

impl Watched<'_> {
    /// The arm held now, if any.
    fn id(&self) -> Option<String> {
        self.id.borrow().clone()
    }

    /// Acknowledge the replacement before retiring the arm held, if any.
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
///
/// # Errors
/// One line for a target `ssh` could misread.
pub fn relay_cmd(paths: &Paths, target: &Target) -> Result<Cmd, String> {
    ssh::request_cmd(paths, target, "screen").map_err(|e| e.to_string())
}

/// What Screen Sharing.app opens: the admin at the relay's local end.
#[must_use]
pub fn vnc_url(admin: &str, local_port: u16) -> String {
    // {admin}@127.0.0.1:{local_port}")
    format!("vnc://{admin}@127.0.0.1:{local_port}")
}

/// Whether something on the host answers `relay`'s request with an RFB greeting.
///
/// # Errors
/// One line: the host's own `refused: …` (Screen Sharing off, or an agent too old for `screen`), `ssh`'s error, or what came back in place of the greeting.
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
///
/// # Errors
/// The last reading, once [`Flow::hang`] passes without a grant; or the signal that stopped the walk ([`System::stopped`]).
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

/// Open `url` in `admin`'s session ([`open_pane_cmd`]). A request that did not run is sent
/// # Errors
/// How the open ran, when it failed; the last reason it did not run once [`Flow::hang`]
/// passes; or the signal that stopped the walk ([`System::stopped`]).
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

/// The line a walk a signal stopped ends a step or a wait with.
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
///
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
    /// No reading said it was missing, the one before the wait included: it was there before the walk, locked or not.
    Existing,
    /// A reading found no session and a later one found it: the admin logged in while the walk waited (over the relay, for `screen`).
    LoggedIn,
}

/// Wait until `admin` has a login session on the host, and say whether a reading found none first.
///
/// # Errors
/// One line once [`Flow::hang`] passes without one, or when a signal stopped the walk ([`System::stopped`]).
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

/// Walk `steps` in order. A step that needs the Mac's keyboard is left as
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

/// The Screen Sharing step `steps` lists measured outstanding: `screen` cannot work until a person turns it on at the Mac.
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
        /// Why the last reading did not run, when none saw the login: the wait for it ended
        unread: Option<String>,
        /// The host's screen watch stays armed, to try the lock itself.
        watched: bool,
    },
}

impl Session {
    /// The one line `screen` ends with when it left the Mac logged in and unlocked, or may have, after the walk's own lines and its closing plan.
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

/// Lock the session `admin` logged in to over the relay ([`lock_cmd`]). macOS leaves a login
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

/// The line once the admin logged in over the relay: what locks the session, and when.
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

/// After the relay closed: the steps, and the session decided. A session there before the
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
///
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

/// `aterm machines person <host> --here`: the same walk for a person at the Mac's keyboard, with no relay and no viewer.
///
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
mod tests {
    use super::*;
    use crate::items::person::NEEDS_REMOTE_FDA;
    use crate::model::{Effect, HostFacts, ItemReport, Mode};
    use crate::port::Fake;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    fn admin() -> User {
        User {
            name: "admin".into(),
            uid: 501,
            gid: 20,
            home: "/Users//admin".into(),
            shell: "/bin/zsh".into(),
            admin: true,
        }
    }

    fn row(id: &str, outcome: Outcome, why: Option<&str>) -> ItemReport {
        ItemReport {
            id: id.into(),
            family: id.split('.').next().unwrap().into(),
            title: id.into(),
            outcome,
            from: None,
            to: None,
            why: why.map(str::to_owned),
            revert: None,
            effect: Effect::Now,
            detail: None,
            layer: None,
            class: None,
            explain: None,
            stood_aside: None,
            essential: false,
        }
    }

    fn report(items: Vec<ItemReport>) -> HostReport {
        HostReport {
            schema: 1,
            mode: Mode::Plan,
            serial: 1,
            key_id: String::new(),
            run_id: String::new(),
            started: 0,
            finished: 0,
            facts: HostFacts::default(),
            items,
            refused: None,
        }
    }

    /// A scripted host: replies keyed by the exec's argv, consumed in order, the last one
    #[derive(Default)]
    struct Host<'a> {
        replies: Mutex<BTreeMap<String, Vec<Ran>>>,
        seen: Mutex<Vec<String>>,
        /// `launchctl print gui/501`, reading by reading, the last repeating.
        sessions: Mutex<Vec<Result<Ran, String>>>,
        /// Each pane open's answer in turn, the last repeating.
        panes: Mutex<Vec<Result<Ran, String>>>,
        /// The lock request's answer (`locked` when `None`).
        lock: Option<Result<Ran, String>>,
        /// Each arm's answer in turn, the last repeating (`armed 1800000000-77` when none).
        arm: Mutex<Vec<Result<Ran, String>>>,
        /// Each disarm's answer in turn, the last repeating (`disarmed 1800000000-77` when
        disarm: Mutex<Vec<Result<Ran, String>>>,
        /// The port whose relay the lock request is checked against.
        sys: Option<&'a Fake>,
        /// The viewer the arm is checked against.
        viewer: Option<&'a Recorder>,
        /// Whether that relay had closed when the lock request came.
        closed_at_lock: Mutex<Option<bool>>,
        /// Whether that relay had opened when the first session reading came.
        relayed_at_first_reading: Mutex<Option<bool>>,
        /// Whether the relay had opened, and the viewer, when the arm came.
        opened_at_arm: Mutex<Option<(bool, bool)>>,
    }

    /// The id the scripted host arms.
    const WATCH: &str = "1800000000-77";

    /// The next answer of `script`, the last repeating; `None` when it is empty.
    fn next(script: &Mutex<Vec<Result<Ran, String>>>) -> Option<Result<Ran, String>> {
        let mut script = script.lock().unwrap();
        match script.len() {
            0 => None,
            1 => Some(script[0].clone()),
            _ => Some(script.remove(0)),
        }
    }

    impl Host<'_> {
        /// The admin at the login window for `absent` readings, then logged in.
        fn logs_in_after(&self, absent: usize) {
            let mut script: Vec<Result<Ran, String>> = (0..absent)
                .map(|_| {
                    Ok(Ran::exit(
                        125,
                        "Could not print domain: 125: Domain does not support specified action\n",
                    ))
                })
                .collect();
            script.push(Ok(Ran::ok("")));
            *self.sessions.lock().unwrap() = script;
        }

        fn on(&self, cmd: &Cmd, reply: Ran) {
            let key = exec_of(cmd).unwrap().argv.join(" ");
            self.replies
                .lock()
                .unwrap()
                .entry(key)
                .or_default()
                .push(reply);
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Remote for Host<'_> {
        fn exec(&self, exec: &Exec) -> Result<Ran, String> {
            let key = exec.argv.join(" ");
            self.seen.lock().unwrap().push(key.clone());
            if exec.argv.last().map(String::as_str) == Some("lock-screen") {
                if let Some(sys) = self.sys {
                    *self.closed_at_lock.lock().unwrap() =
                        Some(sys.calls().last().map(String::as_str) == Some("relay closed"));
                }
                return self.lock.clone().unwrap_or_else(|| Ok(Ran::ok("locked\n")));
            }
            match exec.argv.get(3..5) {
                Some([verb, arm]) if verb == "screen-watch" && arm == "arm" => {
                    *self.opened_at_arm.lock().unwrap() = Some((
                        self.sys
                            .is_some_and(|sys| sys.calls().iter().any(|c| c.starts_with("relay "))),
                        self.viewer
                            .is_some_and(|viewer| !viewer.0.lock().unwrap().is_empty()),
                    ));
                    return next(&self.arm)
                        .unwrap_or_else(|| Ok(Ran::ok(&format!("armed {WATCH}\n"))));
                }
                Some([verb, disarm]) if verb == "screen-watch" && disarm == "disarm" => {
                    return next(&self.disarm)
                        .unwrap_or_else(|| Ok(Ran::ok(&format!("disarmed {WATCH}\n"))));
                }
                _ => {}
            }
            if exec.argv.get(1).map(String::as_str) == Some("asuser") {
                return next(&self.panes).unwrap_or_else(|| Ok(Ran::ok("")));
            }
            if exec.argv.get(1).map(String::as_str) == Some("print") {
                if let Some(sys) = self.sys {
                    self.relayed_at_first_reading
                        .lock()
                        .unwrap()
                        .get_or_insert_with(|| sys.calls().iter().any(|c| c.starts_with("relay ")));
                }
                let mut script = self.sessions.lock().unwrap();
                return match script.len() {
                    0 => Ok(Ran::ok("gui/501 = {\n\ttype = login\n\tsession = Aqua\n")),
                    1 => script[0].clone(),
                    _ => script.remove(0),
                };
            }
            let mut replies = self.replies.lock().unwrap();
            match replies.get_mut(&key) {
                Some(list) if list.len() > 1 => Ok(list.remove(0)),
                Some(list) if !list.is_empty() => Ok(list[0].clone()),
                _ => Err(format!("no reply scripted for {key}")),
            }
        }
    }

    /// A clock that moves only when the flow pauses.
    #[derive(Default)]
    struct TestClock(Mutex<Duration>);

    impl Clock for TestClock {
        fn elapsed(&self) -> Duration {
            *self.0.lock().unwrap()
        }
        fn pause(&self, d: Duration) {
            *self.0.lock().unwrap() += d;
        }
    }

    /// The inert opener: records each URL, opens nothing.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl Opener for Recorder {
        fn open(&self, url: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(url.to_owned());
            Ok(())
        }
    }

    const ATERM_ROWS_OFF: &str = "com.aterm.aterm|kTCCServiceSystemPolicyAllFiles|0\n";
    const ATERM_ROWS_FDA: &str = "com.aterm.aterm|kTCCServiceSystemPolicyAllFiles|2\n";
    const ATERM_ROWS_BOTH: &str = "com.aterm.aterm|kTCCServiceSystemPolicyAllFiles|2\ncom.aterm.aterm|kTCCServiceDeveloperTool|2\n";

    fn tcc_aterm() -> Cmd {
        person::tcc_cmd(&["com.aterm.aterm"])
    }

    fn paths() -> Paths {
        Paths {
            root: PathBuf::from("/Users//owner/.config/aterm/machines"),
        }
    }

    fn target() -> Target {
        Target {
            user: "admin".into(),
            address: "lab-07.local".into(),
            alias: Some("4C4C4544-0000-1000-8000-000000000007".into()),
        }
    }

    fn steps(ids: &[&str]) -> Vec<Step> {
        ids.iter()
            .map(|id| Step {
                step: PersonStep::parse(id).unwrap(),
                measured: true,
                why: None,
            })
            .collect()
    }

    const TTY: Terminals = Terminals {
        stdin: true,
        stderr: true,
    };

    #[test]
    fn outstanding_steps_come_from_the_report_with_fda_before_developer_tools() {
        let r = report(vec![
            row("person.developer_tools:aterm", Outcome::Person, Some("…")),
            row("power.ac.sleep", Outcome::Person, None),
            row("person.remote_login", Outcome::Ok, None),
            row(
                "person.full_disk_access:aterm",
                Outcome::Unknown,
                Some(NEEDS_REMOTE_FDA),
            ),
            row("person.remote_full_disk_access", Outcome::Person, Some("…")),
            row(
                "person.screen_sharing",
                Outcome::Unknown,
                Some("launchctl: timed out"),
            ),
        ]);
        let got: Vec<(String, bool)> = prepare_steps(&r)
            .into_iter()
            .map(|s| (s.id(), s.measured))
            .collect();
        assert_eq!(
            got,
            [
                ("remote_full_disk_access".to_owned(), true),
                ("screen_sharing".to_owned(), false),
                ("full_disk_access:aterm".to_owned(), false),
                ("developer_tools:aterm".to_owned(), true),
            ]
        );
        let unread = prepare_steps(&r)
            .into_iter()
            .find(|s| s.id() == "screen_sharing")
            .unwrap();
        assert_eq!(unread.why.as_deref(), Some("launchctl: timed out"));
    }

    #[test]
    fn one_named_step_is_walked_even_when_the_report_does_not_list_it() {
        let got = only(steps(&["full_disk_access:aterm"]), "developer_tools:aterm").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id(), "developer_tools:aterm");
        assert!(!got[0].measured);
        assert!(only(Vec::new(), "nope").is_err());
    }

    #[test]
    fn the_admins_uid_comes_from_the_hosts_own_report() {
        let mut r = report(Vec::new());
        assert!(
            account(&r, "admin")
                .unwrap_err()
                .contains("no account admin")
        );
        r.facts.users.push(admin());
        assert_eq!(account(&r, "admin").unwrap().uid, 501);
    }

    #[test]
    fn the_pane_opens_in_the_admins_session_through_launchctl_asuser_with_no_shell() {
        let exec = open_pane_cmd(&admin(), person::PANE_FULL_DISK_ACCESS);
        assert_eq!(exec.user, None, "launchctl asuser needs root");
        assert_eq!(
            exec.argv,
            [
                "/bin/launchctl",
                "asuser",
                "501",
                "/usr/bin/sudo",
                "-n",
                "-u",
                "admin",
                "-H",
                "--",
                "/usr/bin/env",
                "-i",
                "HOME=/Users//admin",
                "USER=admin",
                "LOGNAME=admin",
                "SHELL=/bin/zsh",
                "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                "/usr/bin/open",
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles",
            ]
        );
    }

    /// Script `host` so Screen Sharing reads on with control: the service enabled, and the
    fn screen_sharing_on(host: &Host) {
        host.on(&person::print_disabled_cmd(), Ran::ok(
            "\n\tdisabled services = {\n\t\t\"com.apple.screensharing\" => enabled\n\t\t\"com.openssh.sshd\" => enabled\n\t}\n",
        ));
        host.on(
            &person::tcc_cmd(&[person::SCREEN_SHARING_AGENT]),
            Ran::ok("com.apple.screensharing.agent|kTCCServicePostEvent|2\ncom.apple.screensharing.agent|kTCCServiceScreenCapture|2\n"),
        );
    }

    /// A port where the host greets as screensharingd and the relay listens on 55901.
    fn relaying() -> Fake {
        let mut sys = Fake::default();
        sys.on(
            &relay_cmd(&paths(), &target()).unwrap().display(),
            Ran::ok("RFB 003.889\n"),
        );
        sys.relay_port = 55_901;
        sys
    }

    /// What the fake port records for the check, then the stops caught, the relay's open and
    fn relay_calls() -> [String; 5] {
        let cmd = relay_cmd(&paths(), &target()).unwrap().display();
        [
            cmd.clone(),
            "catch stops".into(),
            format!("relay {cmd}"),
            "relay closed".into(),
            "stops restored".into(),
        ]
    }

    #[test]
    fn each_relayed_connection_is_the_fleets_screen_request_and_nothing_else() {
        let cmd = relay_cmd(&paths(), &target()).unwrap();
        assert_eq!(cmd.program, PathBuf::from("/usr/bin/ssh"));
        let args: Vec<String> = cmd
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let mut want = vec!["-T".to_owned()];
        want.extend(
            ssh::base_options(&paths(), &target())
                .iter()
                .map(|a| a.to_string_lossy().into_owned()),
        );
        want.extend(["--", "admin@lab-07.local", "screen"].map(str::to_owned));
        assert_eq!(args, want);
        assert!(
            !args
                .iter()
                .any(|a| matches!(a.as_str(), "-L" | "-R" | "-D" | "-W" | "-N"))
        );
        assert!(args.iter().any(|a| a == "ClearAllForwardings=yes"));
        assert_eq!(vnc_url("admin", 55_901), "vnc://admin@127.0.0.1:55901");
        let mut odd = target();
        odd.address = "-oProxyCommand=evil".into();
        assert!(relay_cmd(&paths(), &odd).is_err());
    }

    /// A host whose `launchctl print gui/<uid>` answers from a script, in order.
    struct Sessions(Mutex<Vec<Ran>>);

    impl Remote for Sessions {
        fn exec(&self, _: &Exec) -> Result<Ran, String> {
            let mut left = self.0.lock().unwrap();
            Ok(if left.len() > 1 {
                left.remove(0)
            } else {
                left[0].clone()
            })
        }
    }

    #[test]
    fn a_session_is_its_exit_status_even_when_the_kept_output_never_names_it() {
        let host = Sessions(Mutex::new(vec![
            Ran::exit(
                125,
                "Could not print domain: 125: Domain does not support specified action\n",
            ),
            Ran::exit(112, "Could not find domain for user gui: 501\n"),
            Ran::ok("\t\t       0    M   D   com.apple.metadata.mdflagwriter\n\t}\n"),
        ]));
        let clock = TestClock::default();
        let said = Mutex::new(Vec::new());
        let say = |line: &str| said.lock().unwrap().push(line.to_owned());
        let flow = Flow {
            sys: &Fake::default(),
            remote: &host,
            clock: &clock,
            say: &say,
            hang: Duration::from_secs(60),
        };
        assert_eq!(
            await_session(&flow, &admin(), false).unwrap(),
            Found::LoggedIn
        );
        assert_eq!(clock.elapsed(), POLL * 2);
        assert_eq!(said.lock().unwrap().len(), 1);
    }

    #[test]
    fn without_terminals_both_flows_refuse_before_anything_runs() {
        let sys = Fake::default();
        let host = Host::default();
        let opener = Recorder::default();
        let clock = TestClock::default();
        let flow = Flow {
            sys: &sys,
            remote: &host,
            clock: &clock,
            say: &|_| {},
            hang: Duration::from_secs(60),
        };
        let piped = Terminals {
            stdin: false,
            stderr: true,
        };
        let s = steps(&["full_disk_access:aterm"]);
        let err = screen(&flow, &paths(), &target(), &admin(), &s, &opener, piped).unwrap_err();
        assert!(err.contains("not a terminal"), "{err}");
        let err = here(
            &flow,
            &admin(),
            &s,
            Terminals {
                stdin: true,
                stderr: false,
            },
        )
        .unwrap_err();
        assert!(err.starts_with("aterm machines person --here"), "{err}");
        assert!(sys.calls().is_empty());
        assert!(host.seen().is_empty());
        assert!(opener.0.lock().unwrap().is_empty());
    }

    #[test]
    fn screen_relays_opens_the_viewer_then_walks_fda_before_developer_tools() {
        let sys = relaying();
        let host = Host::default();
        screen_sharing_on(&host);
        for rows in [
            ATERM_ROWS_OFF,
            ATERM_ROWS_OFF,
            ATERM_ROWS_FDA,
            ATERM_ROWS_FDA,
            ATERM_ROWS_BOTH,
        ] {
            host.on(&tcc_aterm(), Ran::ok(rows));
        }
        let opener = Recorder::default();
        let clock = TestClock::default();
        let said = Mutex::new(Vec::new());
        let say = |line: &str| said.lock().unwrap().push(line.to_owned());
        let flow = Flow {
            sys: &sys,
            remote: &host,
            clock: &clock,
            say: &say,
            hang: Duration::from_secs(60),
        };
        let screened = screen(
            &flow,
            &paths(),
            &target(),
            &admin(),
            &steps(&["full_disk_access:aterm", "developer_tools:aterm"]),
            &opener,
            TTY,
        )
        .unwrap();
        assert_eq!(
            screened.walked,
            [
                Walked {
                    step: "full_disk_access:aterm".into(),
                    done: Done::Granted
                },
                Walked {
                    step: "developer_tools:aterm".into(),
                    done: Done::Granted
                },
            ]
        );
        assert_eq!(*opener.0.lock().unwrap(), ["vnc://admin@127.0.0.1:55901"]);
        let panes: Vec<String> = host
            .seen()
            .into_iter()
            .filter(|s| s.contains("/usr/bin/open"))
            .collect();
        assert_eq!(panes.len(), 2);
        assert!(panes[0].ends_with("Privacy_AllFiles"), "{panes:?}");
        assert!(panes[1].ends_with("Privacy_DevTools"), "{panes:?}");
        let walked_requests: Vec<String> = host
            .seen()
            .into_iter()
            .filter(|s| !s.contains("print-disabled") && !s.contains("screensharing.agent"))
            .collect();
        assert!(
            walked_requests[0].ends_with("print gui/501"),
            "the session comes first: {walked_requests:?}"
        );
        assert_eq!(sys.calls()[..4], relay_calls()[..4]);
        drop(screened.stops);
        assert_eq!(sys.calls(), relay_calls());
        assert!(
            said.lock()
                .unwrap()
                .iter()
                .any(|l| l.contains("log in as admin"))
        );
    }

    #[test]
    fn a_step_nobody_grants_ends_at_the_hang_detector_and_the_rest_is_not_attempted() {
        let sys = relaying();
        let host = Host::default();
        screen_sharing_on(&host);
        host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_OFF));
        let clock = TestClock::default();
        let flow = Flow {
            sys: &sys,
            remote: &host,
            clock: &clock,
            say: &|_| {},
            hang: Duration::from_secs(60),
        };
        let walked = screen(
            &flow,
            &paths(),
            &target(),
            &admin(),
            &steps(&["full_disk_access:aterm", "developer_tools:aterm"]),
            &Recorder::default(),
            TTY,
        )
        .unwrap()
        .walked;
        assert!(
            matches!(&walked[0].done, Done::NotGranted(why) if why.contains("not granted after 60s")),
            "{walked:?}"
        );
        assert!(
            matches!(&walked[1].done, Done::NotGranted(why) if why.starts_with("not attempted")),
            "{walked:?}"
        );
        assert!(clock.elapsed() >= Duration::from_secs(60));
        assert_eq!(
            sys.calls(),
            relay_calls(),
            "the relay closed after the hang"
        );
    }

    fn pane_opens(host: &Host<'_>) -> usize {
        host.seen()
            .iter()
            .filter(|s| s.contains("/usr/bin/open"))
            .count()
    }

    /// What the walk's first two requests after a login over the relay got (VM, 27.0).
    fn not_answered() -> Result<Ran, String> {
        Err(
            "the agent answered no report: exit 255: Connection timed out during banner exchange"
                .to_owned(),
        )
    }

    #[test]
    fn a_pane_open_that_did_not_run_is_sent_again_until_it_runs() {
        let sys = relaying();
        let host = Host::default();
        *host.panes.lock().unwrap() = vec![not_answered(), not_answered(), Ok(Ran::ok(""))];
        let (screened, _) = screen_fda(&host, &sys);
        assert_eq!(screened.walked[0].done, Done::Granted);
        assert_eq!(pane_opens(&host), 3);
    }

    #[test]
    fn a_pane_open_that_ran_and_failed_ends_the_step_at_once() {
        let sys = relaying();
        let host = Host::default();
        *host.panes.lock().unwrap() = vec![Ok(Ran::exit(1, "LSOpenURLsWithRole() failed\n"))];
        let (screened, _) = screen_fda(&host, &sys);
        assert_eq!(
            screened.walked[0].done,
            Done::NotGranted("the pane did not open: exit 1: LSOpenURLsWithRole() failed".into())
        );
        assert_eq!(pane_opens(&host), 1);
    }

    #[test]
    fn a_pane_open_that_never_runs_ends_the_step_at_the_hang_detector() {
        let sys = relaying();
        let host = Host::default();
        *host.panes.lock().unwrap() = vec![not_answered()];
        let (screened, _) = screen_fda(&host, &sys);
        assert!(
            matches!(&screened.walked[0].done, Done::NotGranted(why)
                if why.starts_with("the pane did not open: the request did not run: the agent answered no report")
                    && why.ends_with("(sent for 60s)")),
            "{:?}",
            screened.walked
        );
        assert_eq!(
            pane_opens(&host),
            31,
            "one open, then one every 2 s for 60 s"
        );
    }

    #[test]
    fn screen_refuses_while_screen_sharing_itself_is_outstanding() {
        let sys = relaying();
        let host = Host::default();
        let clock = TestClock::default();
        let flow = Flow {
            sys: &sys,
            remote: &host,
            clock: &clock,
            say: &|_| {},
            hang: Duration::from_secs(60),
        };
        let err = screen(
            &flow,
            &paths(),
            &target(),
            &admin(),
            &steps(&["screen_sharing", "full_disk_access:aterm"]),
            &Recorder::default(),
            TTY,
        )
        .unwrap_err();
        assert!(err.contains("--here"), "{err}");
        assert!(sys.calls().is_empty(), "no relay was opened");
    }

    #[test]
    fn no_viewer_opens_unless_the_host_greets_as_screen_sharing_and_the_relay_opens() {
        let check = relay_cmd(&paths(), &target()).unwrap().display();
        let old_agent = "refused: \"screen\" is not a request this key may make (hello, put \
                         <sha256>, apply, attach <run>)";
        let cases = [
            (
                Ran {
                    status: Some(2),
                    stdout: format!("{old_agent}\n").into_bytes(),
                    stderr: Vec::new(),
                },
                old_agent,
            ),
            (
                Ran::exit(255, "admin@lab-07.local: Permission denied (publickey).\n"),
                "Permission denied (publickey).",
            ),
            (Ran::ok("SSH-2.0-x\n"), "SSH-2.0-x"),
            (Ran::ok("RFB 003.889\n"), "fake: no relay port scripted"),
        ];
        for (answer, why) in cases {
            let sys = Fake::default();
            sys.on(&check, answer.clone());
            let host = Host::default();
            screen_sharing_on(&host);
            let opener = Recorder::default();
            let clock = TestClock::default();
            let flow = Flow {
                sys: &sys,
                remote: &host,
                clock: &clock,
                say: &|_| {},
                hang: Duration::from_secs(60),
            };
            let err = screen(
                &flow,
                &paths(),
                &target(),
                &admin(),
                &steps(&["full_disk_access:aterm"]),
                &opener,
                TTY,
            )
            .unwrap_err();
            assert!(err.contains("lab-07.local") && err.contains(why), "{err}");
            assert!(opener.0.lock().unwrap().is_empty(), "{why}: no viewer");
            assert!(
                host.seen().iter().all(|s| s.contains("print-disabled")
                    || s.contains("screensharing.agent")
                    || s.ends_with("print gui/501")),
                "{why}: nothing asked of the host but Screen Sharing's and the session's \
                 readings: {:?}",
                host.seen()
            );
            let calls = relay_calls();
            let want: Vec<&String> = if answer.stdout.starts_with(b"RFB ") {
                vec![&calls[0], &calls[1], &calls[2], &calls[4]]
            } else {
                vec![&calls[0]]
            };
            assert_eq!(sys.calls().iter().collect::<Vec<_>>(), want, "{why}");
        }
    }

    #[test]
    fn no_relay_opens_unless_screen_sharing_reads_on_now_whatever_the_report_said() {
        let off = "\n\tdisabled services = {\n\t\t\"com.apple.screensharing\" => disabled\n\t}\n";
        for (reply, want) in [
            (Ran::ok(off), "is not on with control"),
            (Ran::exit(1, "launchctl: timed out\n"), "could not be read"),
        ] {
            let sys = relaying();
            let host = Host::default();
            host.on(&person::print_disabled_cmd(), reply);
            let opener = Recorder::default();
            let clock = TestClock::default();
            let flow = Flow {
                sys: &sys,
                remote: &host,
                clock: &clock,
                say: &|_| {},
                hang: Duration::from_secs(60),
            };
            let err = screen(
                &flow,
                &paths(),
                &target(),
                &admin(),
                &steps(&["full_disk_access:aterm"]),
                &opener,
                TTY,
            )
            .unwrap_err();
            assert!(err.contains(want), "{err}");
            assert!(
                sys.calls().is_empty(),
                "no greeting asked for, no relay opened"
            );
            assert!(opener.0.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn here_walks_without_a_relay_or_viewer_and_skips_what_is_already_granted() {
        let sys = relaying();
        let host = Host::default();
        host.on(&person::print_disabled_cmd(), Ran::ok(
            "\n\tdisabled services = {\n\t\t\"com.apple.screensharing\" => enabled\n\t\t\"com.openssh.sshd\" => enabled\n\t}\n",
        ));
        host.on(
            &person::tcc_cmd(&[person::SCREEN_SHARING_AGENT]),
            Ran::ok("com.apple.screensharing.agent|kTCCServicePostEvent|2\ncom.apple.screensharing.agent|kTCCServiceScreenCapture|2\n"),
        );
        host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_OFF));
        host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_FDA));
        let clock = TestClock::default();
        let flow = Flow {
            sys: &sys,
            remote: &host,
            clock: &clock,
            say: &|_| {},
            hang: Duration::from_secs(60),
        };
        let walked = here(
            &flow,
            &admin(),
            &steps(&["screen_sharing", "full_disk_access:aterm"]),
            TTY,
        )
        .unwrap();
        assert_eq!(walked[0].done, Done::AlreadyGranted);
        assert_eq!(walked[1].done, Done::Granted);
        assert!(sys.calls().is_empty(), "no relay at the Mac");
        let panes = host
            .seen()
            .into_iter()
            .filter(|s| s.contains("/usr/bin/open"))
            .count();
        assert_eq!(panes, 1, "the granted step opened no pane");
    }

    /// A host every request fails on.
    struct Unreachable;

    impl Remote for Unreachable {
        fn exec(&self, _: &Exec) -> Result<Ran, String> {
            Err("ssh: connect to host lab-07.local port 22: Operation timed out".into())
        }
    }

    #[test]
    fn a_session_wait_that_ends_at_the_hang_detector_says_what_it_last_read() {
        let clock = TestClock::default();
        let sys = Fake::default();
        let flow = Flow {
            sys: &sys,
            remote: &Unreachable,
            clock: &clock,
            say: &|_| {},
            hang: Duration::from_secs(60),
        };
        let err = await_session(&flow, &admin(), false).unwrap_err();
        assert!(err.contains("the request did not run"), "{err}");
        assert!(err.contains("Operation timed out"), "{err}");
    }

    #[test]
    fn over_screen_sharing_the_keyboard_only_steps_are_left_for_the_mac() {
        let host = Host::default();
        let clock = TestClock::default();
        let sys = Fake::default();
        let flow = Flow {
            sys: &sys,
            remote: &host,
            clock: &clock,
            say: &|_| {},
            hang: Duration::from_secs(60),
        };
        let walked = walk(&flow, &admin(), &steps(&["remote_login"]), false);
        assert!(matches!(&walked[0].done, Done::AtConsole(how) if how.contains("Remote Login")));
        assert!(host.seen().is_empty());
    }

    /// Once a relay may have admitted a login, every failed wait still closes it
    #[test]
    fn a_failed_login_wait_keeps_the_session_cleanup_obligation() {
        for newly_logged_in in [false, true] {
            let sys = relaying();
            let host = Host {
                sys: Some(&sys),
                ..Host::default()
            };
            if newly_logged_in {
                host.logs_in_after(1);
            }
            let (screened, _) = screen_fda(&host, &sys);
            assert_eq!(screened.walked[0].done, Done::Granted);
            assert_eq!(
                screened.session,
                if newly_logged_in {
                    Session::Locked
                } else {
                    Session::Kept
                }
            );
            assert_eq!(lock_requests(&host), usize::from(newly_logged_in));
            assert_eq!(sys.calls(), relay_calls());
        }
        let missing = || Ok(Ran::exit(112, "no login session"));
        let present = || Ok(Ran::ok(""));
        let unread = || Err("synthetic session read refused".to_owned());
        let run = |script: Vec<Result<Ran, String>>| {
            let sys = relaying();
            let host = Host {
                sys: Some(&sys),
                ..Host::default()
            };
            *host.sessions.lock().unwrap() = script;
            screen_sharing_on(&host);
            let clock = TestClock::default();
            let flow = Flow {
                sys: &sys,
                remote: &host,
                clock: &clock,
                say: &|_| {},
                hang: Duration::ZERO,
            };
            let result = screen(
                &flow,
                &paths(),
                &target(),
                &admin(),
                &steps(&["full_disk_access:aterm"]),
                &Recorder::default(),
                TTY,
            )
            .map(|mut screened| {
                screened.stops = Box::new(());
                screened
            });
            let locks = lock_requests(&host);
            let closed = *host.closed_at_lock.lock().unwrap();
            let requests = host.seen();
            (result, locks, closed, requests, sys.calls())
        };
        for script in [
            vec![missing(), missing(), missing()],
            vec![present(), unread(), present()],
        ] {
            let (result, locks, _, _, calls) = run(script);
            assert!(result.is_err());
            assert_eq!(locks, 0);
            assert_eq!(calls, relay_calls());
        }
        let mut wrong = Vec::new();
        for (case, script) in [
            ("initially absent", vec![missing(), missing(), present()]),
            ("became absent", vec![present(), missing(), present()]),
            ("unknown after close", vec![missing(), missing(), unread()]),
        ] {
            let (result, locks, closed, requests, calls) = run(script);
            let kept_failure = match &result {
                Ok(screened) => {
                    screened.session == Session::Locked
                        && matches!(&screened.walked[..], [Walked { done: Done::NotGranted(why), .. }]
                        if why.contains("has not logged in") && why.contains("no login session"))
                }
                Err(_) => false,
            };
            if !kept_failure
                || locks != 1
                || closed != Some(true)
                || calls != relay_calls()
                || requests.iter().any(|r| r.contains("/usr/bin/open"))
            {
                wrong.push(format!("{case}: result={result:?}, locks={locks}, closed={closed:?}, calls={calls:?}, requests={requests:?}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "RELAY_FAILURE_LOST_SESSION_CUSTODY: {wrong:?}"
        );
    }

    #[test]
    fn the_lock_runs_the_agents_own_verb_in_the_admins_session_like_the_pane() {
        let lock = lock_cmd(&admin());
        let pane = open_pane_cmd(&admin(), person::PANE_DEVELOPER_TOOLS);
        assert_eq!(lock.user, None, "launchctl asuser needs root");
        assert_eq!(lock.argv[..16], pane.argv[..16]);
        assert_eq!(
            lock.argv[16..],
            [
                "/Library/Management/aterm/bin/aterm",
                "machines",
                "agent",
                "lock-screen"
            ]
        );
    }

    /// `screen` over a relay that greets, with Screen Sharing on and aterm's Full Disk Access
    fn screen_fda(host: &Host<'_>, sys: &Fake) -> (Screened, Vec<String>) {
        screen_sharing_on(host);
        host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_OFF));
        host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_OFF));
        host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_FDA));
        let clock = TestClock::default();
        let said = Mutex::new(Vec::new());
        let say = |line: &str| said.lock().unwrap().push(line.to_owned());
        let flow = Flow {
            sys,
            remote: host,
            clock: &clock,
            say: &say,
            hang: Duration::from_secs(60),
        };
        let mut screened = screen(
            &flow,
            &paths(),
            &target(),
            &admin(),
            &steps(&["full_disk_access:aterm"]),
            &Recorder::default(),
            TTY,
        )
        .unwrap();
        screened.stops = Box::new(());
        (screened, said.into_inner().unwrap())
    }

    fn lock_requests(host: &Host<'_>) -> usize {
        let lock = lock_cmd(&admin()).argv.join(" ");
        host.seen().iter().filter(|s| **s == lock).count()
    }

    /// The host's requests that are the lock or the screen watch's, in order: `lock`, `arm`,
    fn lock_and_watch(host: &Host<'_>) -> Vec<String> {
        let lock = lock_cmd(&admin()).argv.join(" ");
        let arm = arm_cmd(&admin()).argv.join(" ");
        host.seen()
            .into_iter()
            .filter_map(|s| {
                if s == lock {
                    Some("lock".to_owned())
                } else if s == arm {
                    Some("arm".to_owned())
                } else {
                    s.strip_prefix(&disarm_cmd("").argv.join(" "))
                        .map(|id| format!("disarm {id}"))
                }
            })
            .collect()
    }

    #[test]
    fn a_walk_that_waited_for_the_login_locks_that_session_once_the_relay_closed() {
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        host.logs_in_after(1);
        let (screened, said) = screen_fda(&host, &sys);
        assert_eq!(screened.walked[0].done, Done::Granted);
        assert_eq!(screened.session, Session::Locked);
        assert_eq!(lock_requests(&host), 1);
        assert_eq!(
            lock_and_watch(&host),
            ["arm", "lock", &format!("disarm {WATCH}")]
        );
        let seen = host.seen();
        assert_eq!(seen[seen.len() - 2], lock_cmd(&admin()).argv.join(" "));
        assert_eq!(
            *host.closed_at_lock.lock().unwrap(),
            Some(true),
            "the lock comes after the relay closed"
        );
        assert_eq!(sys.calls(), relay_calls());
        assert!(
            said.iter().any(|l| l.starts_with("locked admin's session")),
            "{said:?}"
        );
        assert_eq!(screened.session.unlocked_line("lab-07", "admin"), None);
    }

    #[test]
    fn a_session_there_at_the_first_reading_is_left_to_macos() {
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        let (screened, said) = screen_fda(&host, &sys);
        assert_eq!(screened.walked[0].done, Done::Granted);
        assert_eq!(screened.session, Session::Kept);
        assert_eq!(
            lock_and_watch(&host),
            Vec::<String>::new(),
            "{:?}",
            host.seen()
        );
        assert!(!said.iter().any(|l| l.starts_with("locked")), "{said:?}");
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        *host.sessions.lock().unwrap() = vec![
            Ok(Ran::ok("")),
            Ok(Ran::exit(112, "Could not find domain for user gui: 501\n")),
            Ok(Ran::ok("")),
        ];
        let (screened, said) = screen_fda(&host, &sys);
        assert_eq!(screened.session, Session::Locked);
        assert_eq!(lock_and_watch(&host), ["lock"]);
        assert!(
            said.iter()
                .any(|l| l.ends_with("the Mac stays logged in and unlocked at its display")),
            "{said:?}"
        );
    }

    #[test]
    fn the_session_is_read_before_anything_is_relayed_and_a_reading_that_did_not_run_relays_nothing()
     {
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        host.logs_in_after(1);
        let (screened, _) = screen_fda(&host, &sys);
        assert_eq!(screened.session, Session::Locked);
        assert_eq!(
            *host.relayed_at_first_reading.lock().unwrap(),
            Some(false),
            "the first session reading came before the relay opened"
        );
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        *host.sessions.lock().unwrap() = vec![
            Err("ssh: connect to host lab-07.local port 22: Operation timed out".into()),
            Ok(Ran::ok("")),
        ];
        screen_sharing_on(&host);
        let opener = Recorder::default();
        let clock = TestClock::default();
        let flow = Flow {
            sys: &sys,
            remote: &host,
            clock: &clock,
            say: &|_| {},
            hang: Duration::from_secs(60),
        };
        let err = screen(
            &flow,
            &paths(),
            &target(),
            &admin(),
            &steps(&["full_disk_access:aterm"]),
            &opener,
            TTY,
        )
        .unwrap_err();
        assert_eq!(
            err,
            "whether admin is logged in on lab-07.local could not be read (the request did not \
             run: ssh: connect to host lab-07.local port 22: Operation timed out); nothing was \
             relayed"
        );
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
        assert!(opener.0.lock().unwrap().is_empty());
        assert_eq!(lock_requests(&host), 0);
    }

    #[test]
    fn a_stop_while_waiting_for_the_login_closes_the_relay_then_locks_a_session_let_in_meanwhile() {
        let mut sys = relaying();
        sys.stop_after = Some(0);
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        host.logs_in_after(2);
        let (screened, said) = screen_fda(&host, &sys);
        assert_eq!(
            screened.walked,
            [Walked {
                step: "full_disk_access:aterm".into(),
                done: Done::NotGranted("not attempted: stopped by SIGINT".into()),
            }]
        );
        assert_eq!(screened.session, Session::Locked);
        assert_eq!(
            lock_and_watch(&host),
            ["arm", "lock", &format!("disarm {WATCH}")]
        );
        assert_eq!(*host.closed_at_lock.lock().unwrap(), Some(true));
        assert_eq!(
            sys.calls(),
            relay_calls(),
            "caught until the command let them go"
        );
        assert!(!host.seen().iter().any(|s| s.contains("/usr/bin/open")));
        assert!(said.iter().any(|l| l.starts_with("locked admin's session")));
        let mut sys = relaying();
        sys.stop_after = Some(0);
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        host.logs_in_after(9);
        screen_sharing_on(&host);
        let clock = TestClock::default();
        let flow = Flow {
            sys: &sys,
            remote: &host,
            clock: &clock,
            say: &|_| {},
            hang: Duration::from_secs(60),
        };
        let err = screen(
            &flow,
            &paths(),
            &target(),
            &admin(),
            &steps(&["full_disk_access:aterm"]),
            &Recorder::default(),
            TTY,
        )
        .unwrap_err();
        assert!(
            err.starts_with("stopped by SIGINT while waiting for admin to log in"),
            "{err}"
        );
        assert_eq!(lock_and_watch(&host), ["arm", &format!("disarm {WATCH}")]);
        assert_eq!(sys.calls(), relay_calls());
    }

    #[test]
    fn a_stop_during_a_step_still_locks_a_session_logged_in_over_the_relay_and_none_other() {
        let mut sys = relaying();
        sys.stop_after = Some(1);
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        host.logs_in_after(1);
        let (screened, said) = screen_fda(&host, &sys);
        assert_eq!(
            screened.walked[0].done,
            Done::NotGranted("stopped by SIGINT".into())
        );
        assert_eq!(screened.session, Session::Locked);
        assert_eq!(*host.closed_at_lock.lock().unwrap(), Some(true));
        assert_eq!(sys.calls(), relay_calls());
        assert!(
            said.iter().any(|l| l
                == "admin logged in over Screen Sharing: the session is locked again when the \
                    walk ends or is stopped; if aterm is killed or loses the Mac first, the host \
                    locks it itself once no request has reached it for 60s"),
            "{said:?}"
        );
        assert_eq!(
            lock_and_watch(&host),
            ["arm", "lock", &format!("disarm {WATCH}")]
        );
        let mut sys = relaying();
        sys.stop_after = Some(0);
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        let (screened, said) = screen_fda(&host, &sys);
        assert_eq!(
            screened.walked[0].done,
            Done::NotGranted("not attempted: stopped by SIGINT".into())
        );
        assert_eq!(screened.session, Session::Kept);
        assert_eq!(lock_requests(&host), 0, "{:?}", host.seen());
        assert!(
            !said
                .iter()
                .any(|l| l.contains("logged in over Screen Sharing"))
        );
        assert_eq!(sys.calls(), relay_calls());
    }

    #[test]
    fn a_lock_that_does_not_read_back_is_reported_as_a_mac_left_unlocked() {
        let refused_verb = "refused: `aterm machines agent lock-screen` is not an agent verb (ssh, \
                            hello, put, apply, run-job, attach, put-file, install, version)";
        for (answer, why) in [
            (
                Ran {
                    status: Some(1),
                    stdout: b"not locked: SACLockScreenImmediate answered 22\n".to_vec(),
                    stderr: Vec::new(),
                },
                "SACLockScreenImmediate answered 22".to_owned(),
            ),
            (
                Ran {
                    status: Some(2),
                    stdout: format!("{refused_verb}\n").into_bytes(),
                    stderr: Vec::new(),
                },
                format!("exit 2: {refused_verb}"),
            ),
            (
                Ran::exit(1, "sudo: a password is required\n"),
                "exit 1: sudo: a password is required".to_owned(),
            ),
        ] {
            let sys = relaying();
            let host = Host {
                sys: Some(&sys),
                lock: Some(Ok(answer)),
                ..Host::default()
            };
            host.logs_in_after(2);
            let (screened, said) = screen_fda(&host, &sys);
            assert_eq!(
                screened.session,
                Session::Unlocked {
                    why: why.clone(),
                    unread: None,
                    watched: true,
                }
            );
            assert_eq!(lock_and_watch(&host), ["arm", "lock"]);
            assert!(!said.iter().any(|l| l.starts_with("locked")), "{said:?}");
            let line = screened.session.unlocked_line("lab-07", "admin").unwrap();
            assert_eq!(
                line,
                format!(
                    "lab-07: the Mac is still logged in as admin and unlocked at its display: \
                     the lock after the walk failed ({why}); the host's screen watch locks it \
                     once no request has reached the host for 60s"
                )
            );
        }
        struct NoLock<'a>(Host<'a>);
        impl Remote for NoLock<'_> {
            fn exec(&self, exec: &Exec) -> Result<Ran, String> {
                if exec.argv.last().map(String::as_str) == Some("lock-screen") {
                    return Err(
                        "ssh: connect to host lab-07.local port 22: Connection refused".into(),
                    );
                }
                self.0.exec(exec)
            }
        }
        let sys = relaying();
        let host = NoLock(Host::default());
        host.0.logs_in_after(1);
        screen_sharing_on(&host.0);
        host.0.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_FDA));
        let clock = TestClock::default();
        let flow = Flow {
            sys: &sys,
            remote: &host,
            clock: &clock,
            say: &|_| {},
            hang: Duration::from_secs(60),
        };
        let screened = screen(
            &flow,
            &paths(),
            &target(),
            &admin(),
            &steps(&["full_disk_access:aterm"]),
            &Recorder::default(),
            TTY,
        )
        .unwrap();
        assert!(
            matches!(&screened.session, Session::Unlocked { why, unread: None, watched: true } if why.starts_with("the request did not run") && why.contains("Connection refused")),
            "{:?}",
            screened.session
        );
    }

    /// `screen` of the FDA step with `host` and `sys` and the viewer `opener`: what it returned
    fn screen_with(
        host: &Host<'_>,
        sys: &Fake,
        opener: &Recorder,
    ) -> (Result<Screened, String>, Vec<String>) {
        screen_sharing_on(host);
        host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_OFF));
        host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_FDA));
        let clock = TestClock::default();
        let said = Mutex::new(Vec::new());
        let say = |line: &str| said.lock().unwrap().push(line.to_owned());
        let flow = Flow {
            sys,
            remote: host,
            clock: &clock,
            say: &say,
            hang: Duration::from_secs(60),
        };
        let screened = screen(
            &flow,
            &paths(),
            &target(),
            &admin(),
            &steps(&["full_disk_access:aterm"]),
            opener,
            TTY,
        );
        (screened, said.into_inner().unwrap())
    }

    #[test]
    fn with_nobody_logged_in_the_watch_is_armed_before_the_relay_and_the_viewer() {
        let sys = relaying();
        let opener = Recorder::default();
        let host = Host {
            sys: Some(&sys),
            viewer: Some(&opener),
            ..Host::default()
        };
        host.logs_in_after(1);
        let (screened, _) = screen_with(&host, &sys, &opener);
        let screened = screened.unwrap();
        assert_eq!(screened.session, Session::Locked);
        assert_eq!(
            *host.opened_at_arm.lock().unwrap(),
            Some((false, false)),
            "neither the relay nor the viewer had opened when the arm came"
        );
        let seen = host.seen();
        let arm = arm_cmd(&admin()).argv.join(" ");
        let at = seen.iter().position(|s| *s == arm).unwrap();
        assert!(seen[at - 1].ends_with("print gui/501"), "{seen:?}");
        assert_eq!(
            arm_cmd(&admin()).argv,
            [
                "/Library/Management/aterm/bin/aterm",
                "machines",
                "agent",
                "screen-watch",
                "arm",
                "--uid",
                "501",
                "--user",
                "admin"
            ]
        );
        assert_eq!(arm_cmd(&admin()).user, None, "root's");
        assert_eq!(
            lock_and_watch(&host),
            ["arm", "lock", &format!("disarm {WATCH}")]
        );
        assert_eq!(sys.calls().last().map(String::as_str), Some("relay closed"));
        drop(screened.stops);
        assert_eq!(sys.calls(), relay_calls());
    }

    #[test]
    fn a_watch_that_is_not_armed_relays_nothing_and_opens_no_viewer() {
        let old_agent = "refused: `aterm machines agent screen-watch arm --uid 501 --user admin` \
                         is not an agent verb (ssh, hello, put, apply, run-job, attach, put-file, \
                         install, lock-screen, version)";
        for (answer, why) in [
            (
                Ok(Ran {
                    status: Some(1),
                    stdout: b"not armed: account admin is uid 501, not 502\n".to_vec(),
                    stderr: Vec::new(),
                }),
                "account admin is uid 501, not 502".to_owned(),
            ),
            (
                Ok(Ran {
                    status: Some(2),
                    stdout: format!("{old_agent}\n").into_bytes(),
                    stderr: Vec::new(),
                }),
                format!("exit 2: {old_agent}"),
            ),
            (
                Err("ssh: connect to host lab-07.local port 22: Operation timed out".into()),
                "the request did not run: ssh: connect to host lab-07.local port 22: Operation \
                 timed out"
                    .to_owned(),
            ),
        ] {
            let sys = relaying();
            let opener = Recorder::default();
            let host = Host {
                sys: Some(&sys),
                arm: Mutex::new(vec![answer]),
                ..Host::default()
            };
            host.logs_in_after(1);
            let (screened, _) = screen_with(&host, &sys, &opener);
            assert_eq!(
                screened.unwrap_err(),
                format!(
                    "the screen watch on lab-07.local was not armed ({why}), so no lock could \
                     be promised if aterm is killed or loses the Mac; nothing was relayed"
                )
            );
            assert_eq!(
                sys.calls(),
                [relay_cmd(&paths(), &target()).unwrap().display()]
            );
            assert!(opener.0.lock().unwrap().is_empty());
            assert_eq!(lock_and_watch(&host), ["arm"]);
        }
    }

    #[test]
    fn a_disarm_that_fails_is_one_line_and_changes_nothing_else() {
        for (answer, line) in [
            (
                Err("ssh: connect to host lab-07.local port 22: Operation timed out".to_owned()),
                "the host's screen watch was not disarmed (the request did not run: ssh: connect \
                 to host lab-07.local port 22: Operation timed out): once no request has reached \
                 the host for 60s it locks admin's session if there is one, which is harmless",
            ),
            (
                Ok(Ran {
                    status: Some(1),
                    stdout: b"not disarmed: Operation not permitted (os error 1)\n".to_vec(),
                    stderr: Vec::new(),
                }),
                "the host's screen watch was not disarmed (Operation not permitted (os error \
                 1)): once no request has reached the host for 60s it locks admin's session if \
                 there is one, which is harmless",
            ),
            (
                Ok(Ran::ok(&format!(
                    "disarmed {WATCH} (it had ended: locked admin's session: no signed \
                     request reached the host for 61s)\n"
                ))),
                "the host's screen watch had already ended: locked admin's session: no signed \
                 request reached the host for 61s",
            ),
        ] {
            let sys = relaying();
            let host = Host {
                sys: Some(&sys),
                disarm: Mutex::new(vec![answer]),
                ..Host::default()
            };
            host.logs_in_after(1);
            let (screened, said) = screen_fda(&host, &sys);
            assert_eq!(screened.walked[0].done, Done::Granted);
            assert_eq!(screened.session, Session::Locked);
            assert_eq!(screened.session.unlocked_line("lab-07", "admin"), None);
            let watch_lines: Vec<&String> =
                said.iter().filter(|l| l.contains("screen watch")).collect();
            assert_eq!(watch_lines, [line]);
            assert!(said.last().unwrap().contains("screen watch"), "{said:?}");
        }
    }

    /// `n` pane opens that do not run, then one that does: a gap of `2n` s between the
    fn opens_after(host: &Host<'_>, n: usize) {
        let mut script: Vec<Result<Ran, String>> = (0..n).map(|_| not_answered()).collect();
        script.push(Ok(Ran::ok("")));
        *host.panes.lock().unwrap() = script;
    }

    #[test]
    fn a_gap_that_may_have_let_the_watch_lock_and_end_arms_it_again() {
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        host.logs_in_after(1);
        opens_after(&host, 16);
        *host.disarm.lock().unwrap() = vec![
            Ok(Ran::ok(&format!(
                "disarmed {WATCH} (it had ended: locked admin's session: no signed request \
                 reached the host for 60s)\n"
            ))),
            Ok(Ran::ok("disarmed 1800000040-78\n")),
        ];
        *host.arm.lock().unwrap() = vec![
            Ok(Ran::ok(&format!("armed {WATCH}\n"))),
            Ok(Ran::ok("armed 1800000040-78\n")),
        ];
        let (screened, said) = screen_fda(&host, &sys);
        assert_eq!(screened.walked[0].done, Done::Granted);
        assert_eq!(screened.session, Session::Locked);
        assert_eq!(
            lock_and_watch(&host),
            [
                "arm",
                "arm",
                &format!("disarm {WATCH}"),
                "lock",
                "disarm 1800000040-78"
            ]
        );
        let seen = host.seen();
        let open = seen
            .iter()
            .rposition(|s| s.contains("/usr/bin/open"))
            .unwrap();
        assert!(seen[open + 1].contains("screen-watch arm"), "{seen:?}");
        let watch_lines: Vec<&String> =
            said.iter().filter(|l| l.contains("screen watch")).collect();
        assert_eq!(
            watch_lines,
            [
                "the host's screen watch had already ended: locked admin's session: no signed \
              request reached the host for 60s"
            ]
        );
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        host.logs_in_after(1);
        opens_after(&host, 14);
        let (screened, _) = screen_fda(&host, &sys);
        assert_eq!(screened.walked[0].done, Done::Granted);
        assert_eq!(
            lock_and_watch(&host),
            ["arm", "lock", &format!("disarm {WATCH}")]
        );
    }

    #[test]
    fn an_arm_again_that_fails_is_said_tried_at_the_next_request_and_not_promised_at_the_end() {
        let refused = || {
            Ok(Ran {
                status: Some(1),
                stdout: b"not armed: /A/state/screen-watch: No space left on device\n".to_vec(),
                stderr: Vec::new(),
            })
        };
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        host.logs_in_after(1);
        opens_after(&host, 16);
        *host.arm.lock().unwrap() = vec![
            Ok(Ran::ok(&format!("armed {WATCH}\n"))),
            refused(),
            Ok(Ran::ok("armed 1800000040-78\n")),
        ];
        let (screened, said) = screen_fda(&host, &sys);
        assert_eq!(screened.session, Session::Locked);
        assert_eq!(
            lock_and_watch(&host),
            [
                "arm",
                "arm",
                "arm",
                &format!("disarm {WATCH}"),
                "lock",
                "disarm 1800000040-78"
            ]
        );
        let watch_lines: Vec<&String> =
            said.iter().filter(|l| l.contains("screen watch")).collect();
        assert_eq!(
            watch_lines,
            [
                "the host's screen watch was not armed again (/A/state/screen-watch: No space \
                 left on device): if aterm is killed or loses the Mac before the walk's own \
                 lock, admin's session stays as it is left; tried again at the next request",
                "the host's screen watch is armed again",
            ]
        );
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            lock: Some(Ok(Ran::exit(1, "sudo: a password is required\n"))),
            ..Host::default()
        };
        host.logs_in_after(1);
        opens_after(&host, 16);
        *host.arm.lock().unwrap() = vec![Ok(Ran::ok(&format!("armed {WATCH}\n"))), refused()];
        let (screened, _) = screen_fda(&host, &sys);
        assert_eq!(
            screened.session,
            Session::Unlocked {
                why: "exit 1: sudo: a password is required".into(),
                unread: None,
                watched: false,
            }
        );
        let watched = lock_and_watch(&host);
        let disarms: Vec<&String> = watched.iter().filter(|r| r.starts_with("disarm")).collect();
        assert!(disarms.is_empty(), "{watched:?}");
        assert_eq!(
            watched.last().map(String::as_str),
            Some("arm"),
            "{watched:?}"
        );
        assert!(
            !screened
                .session
                .unlocked_line("lab-07", "admin")
                .unwrap()
                .contains("screen watch")
        );
    }

    #[test]
    fn a_reading_after_a_stop_that_did_not_run_does_not_claim_a_login() {
        let mut sys = relaying();
        sys.stop_after = Some(0);
        let host = Host {
            sys: Some(&sys),
            lock: Some(Err(
                "ssh: connect to host lab-07.local port 22: Operation timed out".into(),
            )),
            ..Host::default()
        };
        *host.sessions.lock().unwrap() = vec![
            Ok(Ran::exit(
                125,
                "Could not print domain: 125: Domain does not support specified action\n",
            )),
            Ok(Ran::exit(
                125,
                "Could not print domain: 125: Domain does not support specified action\n",
            )),
            Err("ssh: connect to host lab-07.local port 22: Operation timed out".into()),
        ];
        let (screened, _) = screen_fda(&host, &sys);
        assert_eq!(
            screened.walked[0].done,
            Done::NotGranted("not attempted: stopped by SIGINT".into())
        );
        let line = screened.session.unlocked_line("lab-07", "admin").unwrap();
        assert_eq!(
            line,
            "lab-07: the Mac may be logged in as admin and unlocked at its display: whether \
             admin logged in over Screen Sharing is not known (the last reading: the request \
             did not run: ssh: connect to host lab-07.local port 22: Operation timed out), and \
             the lock after the walk failed (the request did not run: ssh: connect to host \
             lab-07.local port 22: Operation timed out); the host's screen watch locks it once \
             no request has reached the host for 60s"
        );
        assert_eq!(lock_and_watch(&host), ["arm", "lock"]);
    }

    #[test]
    fn a_wait_for_the_login_that_outlasts_the_hang_detector_still_locks_a_session_there_now() {
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        host.logs_in_after(32);
        let (screened, _) = screen_fda(&host, &sys);
        assert!(
            matches!(&screened.walked[0].done, Done::NotGranted(why) if why.starts_with("not attempted: admin has not logged in on the host after 60s")),
            "{:?}",
            screened.walked
        );
        assert_eq!(screened.session, Session::Locked);
        assert_eq!(
            lock_and_watch(&host),
            ["arm", "lock", &format!("disarm {WATCH}")]
        );
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        host.logs_in_after(99);
        let (screened, _) = screen_with(&host, &sys, &Recorder::default());
        assert!(
            screened
                .unwrap_err()
                .starts_with("admin has not logged in on the host after 60s")
        );
        assert_eq!(lock_and_watch(&host), ["arm", &format!("disarm {WATCH}")]);
    }

    #[test]
    fn here_never_locks_even_after_waiting_for_the_login() {
        let sys = Fake::default();
        let host = Host::default();
        host.logs_in_after(1);
        screen_sharing_on(&host);
        host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_OFF));
        host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_FDA));
        let clock = TestClock::default();
        let flow = Flow {
            sys: &sys,
            remote: &host,
            clock: &clock,
            say: &|_| {},
            hang: Duration::from_secs(60),
        };
        let walked = here(&flow, &admin(), &steps(&["full_disk_access:aterm"]), TTY).unwrap();
        assert_eq!(walked[0].done, Done::Granted);
        assert!(
            host.seen().iter().any(|s| s.ends_with("print gui/501")),
            "the walk read the session"
        );
        assert_eq!(
            lock_and_watch(&host),
            Vec::<String>::new(),
            "{:?}",
            host.seen()
        );
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn a_read_that_needs_a_session_or_a_variable_cannot_travel_as_exec() {
        let user = admin();
        let plain = exec_of(&person::tcc_open_cmd()).unwrap();
        assert_eq!(plain.argv[0], "/usr/bin/sqlite3");
        assert_eq!(plain.user, None);
        let as_user = exec_of(&Cmd::new("/bin/cat").arg("/x").as_user(&user)).unwrap();
        assert_eq!(as_user.user.as_deref(), Some("admin"));
        assert!(exec_of(&Cmd::new("/usr/bin/open").as_user(&user).in_session()).is_err());
        assert!(exec_of(&Cmd::new("/usr/bin/true").env("A", "b")).is_err());
    }

    /// The actual request wrapper keeps a previously acknowledged arm through a failed
    #[test]
    fn a_replacement_watch_is_acknowledged_before_the_previous_arm_is_retired() {
        let ordinary = Host::default();
        assert_eq!(arm_watch(&ordinary, &admin()).as_deref(), Ok(WATCH));
        let host = Host {
            arm: Mutex::new(vec![
                Ok(Ran::exit(1, "not armed: replacement refused\n")),
                Ok(Ran::ok("armed 1800000040-78\n")),
            ]),
            ..Host::default()
        };
        let clock = TestClock::default();
        let said = Mutex::new(Vec::new());
        let say = |line: &str| said.lock().unwrap().push(line.to_owned());
        let admin = admin();
        let watched = Watched {
            remote: &host,
            clock: &clock,
            say: &say,
            admin: &admin,
            id: RefCell::new(Some(WATCH.to_owned())),
            ran: Cell::new(Some(clock.elapsed())),
        };
        assert!(watched.exec(&session_cmd(&admin)).is_ok());
        assert_eq!(watched.id().as_deref(), Some(WATCH));
        assert!(lock_and_watch(&host).is_empty());
        clock.pause(REARM);
        assert!(watched.exec(&session_cmd(&admin)).is_ok());
        assert_eq!(watched.id().as_deref(), Some(WATCH));
        assert_eq!(lock_and_watch(&host), ["arm"]);
        assert!(watched.exec(&session_cmd(&admin)).is_ok());
        assert_eq!(watched.id().as_deref(), Some("1800000040-78"));
        assert_eq!(
            lock_and_watch(&host),
            ["arm", "arm", &format!("disarm {WATCH}")]
        );
        let before = host.seen().len();
        assert!(watched.exec(&session_cmd(&admin)).is_ok());
        assert_eq!(host.seen().len(), before + 1);
        assert_eq!(watched.id().as_deref(), Some("1800000040-78"));
    }

    #[test]
    fn a_session_query_that_does_not_exit_normally_is_never_absence() {
        let read = |answer: Result<Ran, String>| {
            let sys = Fake::default();
            let host = Host::default();
            *host.sessions.lock().unwrap() = vec![answer];
            let clock = TestClock::default();
            let flow = Flow {
                sys: &sys,
                remote: &host,
                clock: &clock,
                say: &|_| {},
                hang: Duration::from_secs(60),
            };
            read_session(&flow, &admin())
        };
        assert_eq!(read(Ok(Ran::ok(""))), Ok(Reading::There));
        assert!(read(Err("the transport did not run".into())).is_err());
        for stdout in [
            Vec::new(),
            b"Could not find domain for user gui: 501\n".to_vec(),
        ] {
            assert!(
                read(Ok(Ran {
                    status: None,
                    stdout,
                    stderr: Vec::new()
                }))
                .is_err()
            );
        }
    }
}
