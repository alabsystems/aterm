// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm machines person` and `screen`: the steps only a person can do, over Screen
//! Sharing (design §6).
//!
//! The command layer calls these functions; everything they touch is injected. The host is
//! reached through [`Remote`] (one signed `exec` envelope per request, since the fleet key's
//! forced command runs nothing else), the controller's own sockets and tools through
//! [`System`], Screen Sharing.app through an [`Opener`] (the live arm runs `/usr/bin/open`,
//! the test arm only records), and time through a [`Clock`], so a test walks the whole flow
//! without sleeping.
//!
//! `screen <host>`:
//!
//! 1. refuses unless standard input and standard error are terminals, and while the report
//!    lists Screen Sharing itself outstanding;
//! 2. reads Screen Sharing on the host afresh and refuses unless it is on with control: the
//!    relay goes to the host's port 5900 only when screensharingd is what holds it;
//! 3. reads whether the admin has a login session (`launchctl print gui/<uid>`) and refuses
//!    when the request did not run. Nothing is relayed yet, so no login the walk can cause
//!    comes before this reading, and it alone says whether the session was there before;
//! 4. makes the fleet's `screen` request once with no input and refuses unless an RFB
//!    greeting comes back ([`screen_answers`]: something answers, nothing more);
//! 5. opens a relay on a free local port ([`System::relay`]): each connection to it runs the
//!    fleet's `screen` request ([`relay_cmd`]), whose bytes the host relays to its own
//!    screensharingd. The fleet key forwards no port, so this is the only way in. From here
//!    until the end, SIGINT, SIGTERM and SIGHUP stop the walk rather than the process
//!    ([`System::catch_stops`]), so steps 8 and 9 still run;
//! 6. opens `vnc://<admin>@127.0.0.1:<port>`, and waits until the admin's login session
//!    exists;
//! 7. for each outstanding step, Full Disk Access before Developer Tools, opens the pane in
//!    the admin's session and re-probes until the row reads granted;
//! 8. closes the relay, which ends every connection still open;
//! 9. when step 3 found no session and one exists now, locks it ([`lock_cmd`]): macOS relocks
//!    a session Screen Sharing found locked when the connection closes, and leaves one logged
//!    in over it unlocked at the Mac's display. A session that was there at step 3 is left to
//!    macOS. A lock that does not read back is reported ([`Session::Unlocked`]), never taken
//!    as done. Nothing on the controller can lock after it is killed (`SIGKILL`) or loses the
//!    host, so the walk says so once the admin has logged in over the relay.
//!
//! `person <host> --here` is the same walk with no relay and no viewer, for a person at the
//! Mac's own keyboard, who can also do the two steps that need it (Remote Login and Screen
//! Sharing), and is never locked: the person is at the keyboard. Every wait is a hang detector
//! ([`PERSON_HANG`]), never a latency budget: a person takes the time they take, and the bound
//! only notices that nobody is there.

use super::inventory::Paths;
use super::ssh::{self, Target};
use crate::envelope::Exec;
use crate::items::person::{self, Kind, Known, PersonStep};
use crate::model::{HostReport, Outcome, Probe, User};
use crate::port::{Cmd, Ran, RunFailed, System};
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
    /// The report read it outstanding. `false` when its row could not be read (the TCC
    /// database closed to the agent until remote FDA is on, a read that timed out): the
    /// walk reads it again, and `person` lists it as not measured, never as done.
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

/// The outstanding person steps in `report`, in the order a person does them (Remote Login
/// and its FDA switch, Screen Sharing, every Full Disk Access, then every Developer Tools
/// entry). A row that probed `Person` is outstanding; a row that could not be read, for any
/// reason (remote FDA off, a TCC read that timed out, `launchctl` failing), is listed
/// unmeasured with why: a value nobody read is never a step done.
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
    // Stable: within a kind, the report's order (the baseline's) holds.
    steps.sort_by_key(|s| s.step.kind);
    steps
}

/// `steps` narrowed to the one step `id` (`screen <host> <step>`). A step the report does not
/// list outstanding is walked anyway, unmeasured: the report may be older than the Mac.
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

/// The account `name` as the host reported it (its uid is what `launchctl asuser` and
/// `gui/<uid>` take).
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

/// Refuse `verb` unless a person is there: standard input and standard error are terminals
/// (design §10).
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
    /// printed.
    ///
    /// # Errors
    /// One line when the request did not run (unreachable, refused, not signed).
    fn exec(&self, exec: &Exec) -> Result<Ran, String>;
}

/// Opens a URL on the controller's own screen.
pub trait Opener {
    /// Open `url`.
    ///
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

/// `cmd` as an `exec` payload. A person read is a plain argv, as root or as one user.
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
/// the host: root runs `launchctl asuser <uid>`, which runs the user primitive of design §3.5
/// (`sudo -n -u <user> -H -- env -i …`) around it. No shell.
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

/// The request that opens `url` in `user`'s login session on the host (design §1.9, VM: it
/// opened System Settings at the pane on the console): `/usr/bin/open` [`in_session`].
#[must_use]
pub fn open_pane_cmd(user: &User, url: &str) -> Exec {
    in_session(user, "/usr/bin/open", &[url])
}

/// The request that locks `user`'s login session on the host: the agent's own `lock-screen`
/// ([`crate::protocol::AGENT_EXE`], `crate::agent::lock`) [`in_session`], as the pane opens.
/// The lock is loginwindow's Lock Screen, which reaches loginwindow only from inside the
/// session (VM, 27.0: from root's own bootstrap it locked nothing). It exits 0 only once the
/// session reads locked.
#[must_use]
pub fn lock_cmd(user: &User) -> Exec {
    in_session(
        user,
        crate::protocol::AGENT_EXE,
        &["machines", "agent", "lock-screen"],
    )
}

/// The request that asks whether `user` has a login session: `launchctl print gui/<uid>`
/// exits 0 with one, locked or not, and fails without: 112 (`Could not find domain for user
/// gui: <uid>`), or 125 (`Could not print domain: 125: Domain does not support specified
/// action`) at the login window of a Mac nobody has logged in to since it booted (VM, 27.0).
/// Its exit status is the answer: on 27 the listing runs past the 64 KiB of output an `exec`
/// row keeps, and its tail does not name the session (VM, 450 services).
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

/// What each connection to `screen`'s relay runs: the fleet's `screen` request of `target`
/// ([`ssh::request_cmd`]), with the connection as its standard input and output.
///
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

/// Whether something on the host answers `relay`'s request with an RFB greeting: one run of
/// it with no input, before anything listens. screensharingd speaks first (`RFB 003.889\n`)
/// and closes at the end of the input (measured in the VM: the greeting and exit 0, 0.05 s
/// over an open connection), so this one run proves the key and the agent's `screen`, and
/// that the port answers. It proves liveness only: any program on the host listening on
/// 127.0.0.1:5900 can send the same greeting, so who listens is [`screen`]'s fresh read of
/// Screen Sharing, never this. The relay's own connections discard standard error and cannot
/// say why they closed: without this, a host that cannot answer showed only as the viewer's
/// connection error, and the flow then waited out the hang detector for a login that could
/// not happen.
///
/// # Errors
/// One line: the host's own `refused: …` (Screen Sharing off, or an agent too old for
/// `screen`), `ssh`'s error, or what came back in place of the greeting.
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
/// The last reading, once [`Flow::hang`] passes without a grant; or the signal that stopped
/// the walk ([`System::stopped`]).
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
    /// No reading said it was missing, the one before the wait included: it was there
    /// before the walk, locked or not.
    Existing,
    /// A reading found no session and a later one found it: the admin logged in while the
    /// walk waited (over the relay, for `screen`).
    LoggedIn,
}

/// Wait until `admin` has a login session on the host, and say whether a reading found none
/// first; `absent` when the reading taken before the wait already had (`screen`'s, before
/// the relay opened). A request that did not run read nothing, so it is never "no session".
///
/// # Errors
/// One line once [`Flow::hang`] passes without one, or when a signal stopped the walk
/// ([`System::stopped`]).
pub fn await_session(flow: &Flow<'_>, admin: &User, mut absent: bool) -> Result<Found, String> {
    await_session_observed(flow, admin, &mut absent)
}

// Keep a reading of absence even when the wait fails: a login may arrive before
// the relay closes, and the caller still owns that cleanup decision.
fn await_session_observed(
    flow: &Flow<'_>,
    admin: &User,
    absent: &mut bool,
) -> Result<Found, String> {
    let start = flow.clock.elapsed();
    let mut said = false;
    loop {
        // The last reading goes into the hang message: a request that never ran (the host
        // unreachable, the envelope refused) is not the same as nobody logging in.
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
/// [`Done::AtConsole`] unless `at_console`. The walk stops at the first step that is not
/// granted: the person has gone, and what follows (a TCC read after the remote FDA switch, an
/// app's Developer Tools entry after its Full Disk Access) waits on it.
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
        let opened = flow
            .remote
            .exec(&open_pane_cmd(admin, s.url()))
            .and_then(|ran| {
                if ran.success() {
                    Ok(())
                } else {
                    Err(ran.describe())
                }
            });
        if let Err(why) = opened {
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

/// The Screen Sharing step `steps` lists measured outstanding: `screen` cannot work until a
/// person turns it on at the Mac. The command layer asks this of the report's whole list,
/// before `screen <host> <step>` narrows it to one step.
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
    /// It was there before the relay opened, so it is macOS's to keep as it was:
    /// ScreensharingAgent locks again a session the connection found locked. Also a walk
    /// that never reached the session.
    Kept,
    /// The admin logged in over the relay, and `screen` locked the session after it.
    Locked,
    /// The admin logged in over the relay and the lock did not read back: why. The Mac is
    /// logged in and unlocked at its display.
    Unlocked(String),
}

impl Session {
    /// The one line `screen` ends with when it left the Mac logged in and unlocked, after the
    /// walk's own lines and its closing plan; `None` otherwise.
    #[must_use]
    pub fn unlocked_line(&self, host: &str, admin: &str) -> Option<String> {
        match self {
            Self::Unlocked(why) => Some(format!(
                "{host}: the Mac is still logged in as {admin} and unlocked at its display: \
                 the lock after the walk failed ({why})"
            )),
            Self::Kept | Self::Locked => None,
        }
    }
}

/// What `screen` did: each step, and the session it left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screened {
    /// Each step's result.
    pub walked: Vec<Walked>,
    /// The admin's session at the end.
    pub session: Session,
}

/// Lock the session `admin` logged in to over the relay ([`lock_cmd`]). macOS leaves a login
/// made over Screen Sharing unlocked at the Mac's display when the connection closes (VM,
/// 27.0: 3 of 3 logins, watched for 5 minutes), so `screen` does what ScreensharingAgent does
/// for a session it found locked. Lock, never log out: a logout would end what the person
/// opened in that session.
fn lock_session(flow: &Flow<'_>, admin: &User) -> Session {
    let why = match flow.remote.exec(&lock_cmd(admin)) {
        Ok(ran) if ran.success() => {
            (flow.say)(&format!(
                "locked {}'s session at the Mac again (logged in over Screen Sharing for this \
                 walk)",
                admin.name
            ));
            return Session::Locked;
        }
        // The agent's own `not locked: <why>`; else how the run ended and what it printed (an
        // agent from before `lock-screen` refuses the verb on standard output).
        Ok(ran) => {
            let out = ran.out();
            match out.lines().map(str::trim).find(|l| !l.is_empty()) {
                Some(line) => line
                    .strip_prefix("not locked: ")
                    .map_or_else(|| format!("{}: {line}", ran.describe()), str::to_owned),
                None => ran.describe(),
            }
        }
        Err(why) => format!("the request did not run: {why}"),
    };
    Session::Unlocked(why)
}

/// `aterm machines screen <host>`: walk `steps` over Screen Sharing through a relay
/// [`Flow::sys`] opens on a free local port, then, after the relay closes, lock the admin's
/// session if it was not there before the relay opened and is now ([`Found::LoggedIn`]).
///
/// Before the relay opens, Screen Sharing is read on the host afresh and must be on with
/// control, whatever the report said: with it off, launchd holds no socket on port 5900,
/// and any account on the host could listen there and be handed the admin's login. The
/// admin's session is read then too, so no login over the relay can come before the reading
/// the lock is decided by. A signal ([`System::catch_stops`]) from the relay's opening on
/// ends the walk, not the process: the relay closes and the lock is decided the same way.
///
/// # Errors
/// One line when the flow could not start: no terminal, Screen Sharing itself outstanding or
/// not reading on, the session not read (nothing is relayed, since no lock could then be
/// promised), the host did not answer ([`screen_answers`]), the relay or the viewer did not
/// open, or the admin never logged in (a signal stopped the wait for it, or the hang
/// detector did). A lock that failed is not an error here: it is [`Screened::session`],
/// which the command prints last.
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
    // Held until the lock is decided: a stop from here on closes the relay first.
    let stops = flow.sys.catch_stops();
    let mut relay = flow
        .sys
        .relay(&cmd)
        .map_err(|e| format!("the relay to {} did not open: {e}", target.address))?;
    let result = (|| {
        opener.open(&vnc_url(&target.user, relay.port()))?;
        (flow.say)(&format!(
            "Screen Sharing is opening {}: log in as {}",
            target.address, admin.name
        ));
        let found = await_session_observed(flow, admin, &mut absent)?;
        if found == Found::LoggedIn {
            (flow.say)(&format!(
                "{} logged in over Screen Sharing: the session is locked again when the walk \
                 ends or is stopped; if aterm is killed or loses the Mac first, the Mac stays \
                 logged in and unlocked at its display",
                admin.name
            ));
        }
        Ok::<_, String>((walk(flow, admin, steps, false), found))
    })();
    if let Err(e) = relay.close() {
        (flow.say)(&format!("the relay did not close cleanly: {e}"));
    }
    let (walked, found) = match result {
        Ok(done) => done,
        // Every failed open-relay path keeps its observed absence: a login may
        // have arrived after the last reading. Close first, then decide the lock.
        // An unread final state cannot establish that there is nothing to lock.
        Err(why) if absent => match read_session(flow, admin) {
            Ok(Reading::Absent(_)) => return Err(why),
            Ok(Reading::There) | Err(_) => {
                (flow.say)(&why);
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
                (walked, Found::LoggedIn)
            }
        },
        Err(why) => return Err(why),
    };
    // After the close, so the lock is the last thing that happens at the display; measured
    // on 27 that it still holds then (§11).
    let session = match found {
        Found::Existing => Session::Kept,
        Found::LoggedIn => lock_session(flow, admin),
    };
    drop(stops);
    Ok(Screened { walked, session })
}

/// `aterm machines person <host> --here`: the same walk for a person at the Mac's keyboard,
/// with no relay and no viewer. The panes open in the admin's own session, so the person
/// logs in as the admin first.
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
    // Whoever logged in is at the keyboard: their session is never locked under them, and
    // a signal ends this flow as it ends any command.
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
    /// repeating; every request recorded. The admin has a session unless `sessions` scripts
    /// the readings, and the lock reads back locked unless `lock` says otherwise.
    #[derive(Default)]
    struct Host<'a> {
        replies: Mutex<BTreeMap<String, Vec<Ran>>>,
        seen: Mutex<Vec<String>>,
        /// `launchctl print gui/501`, reading by reading, the last repeating.
        sessions: Mutex<Vec<Result<Ran, String>>>,
        /// The lock request's answer (`locked` when `None`).
        lock: Option<Ran>,
        /// The port whose relay the lock request is checked against.
        sys: Option<&'a Fake>,
        /// Whether that relay had closed when the lock request came.
        closed_at_lock: Mutex<Option<bool>>,
        /// Whether that relay had opened when the first session reading came.
        relayed_at_first_reading: Mutex<Option<bool>>,
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
                return Ok(self.lock.clone().unwrap_or_else(|| Ran::ok("locked\n")));
            }
            if exec.argv.get(1).map(String::as_str) == Some("asuser") {
                return Ok(Ran::ok(""));
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
                // A read that timed out is not "nothing outstanding".
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
    /// agent's PostEvent and ScreenCapture rows granted.
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
    /// close, and the stops restored once the lock is decided.
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
        // No forward is asked for, and none the owner's configuration names gets in.
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
        // VM (27.0): no session read exit 125 at the login window after a boot, and 112 in
        // an earlier run; a session's listing runs past the 64 KiB an exec row keeps, and
        // the tail that arrives is all services, no `session = Aqua`.
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
        // Either failure is "not yet": the wait polled through both.
        assert_eq!(clock.elapsed(), POLL * 2);
        // One line while it waits, so a person knows what it waits for.
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
        // FDA: off when walked, off at the first re-probe, granted at the second; then
        // Developer Tools: missing when walked, granted at the first re-probe.
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
            // An agent from before `screen`, or Screen Sharing off: the host's own line.
            (
                Ran {
                    status: Some(2),
                    stdout: format!("{old_agent}\n").into_bytes(),
                    stderr: Vec::new(),
                },
                old_agent,
            ),
            // ssh itself failed.
            (
                Ran::exit(255, "admin@lab-07.local: Permission denied (publickey).\n"),
                "Permission denied (publickey).",
            ),
            // Something answered, but not screensharingd.
            (Ran::ok("SSH-2.0-x\n"), "SSH-2.0-x"),
            // The host greets; the relay is what fails (the fake's, with no port scripted).
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
            // A relay only after the greeting; the stops it caught are put back.
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
        // The report lists only an app's step, yet Screen Sharing is off on the host now:
        // port 5900 is then anyone's to listen on, so nothing is relayed to it.
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
    /// and decides the lock from the absence it actually observed.
    #[test]
    fn a_failed_login_wait_keeps_the_session_cleanup_obligation() {
        // Ordinary granted walks first: the existing session is left alone, and
        // a login observed during the relay is locked after the relay closes.
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
                // An intentional immediate refusal; the clock never sleeps.
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
            );
            let locks = lock_requests(&host);
            let closed = *host.closed_at_lock.lock().unwrap();
            let requests = host.seen();
            (result, locks, closed, requests, sys.calls())
        };
        // Still absent after the failed wait: no session is locked. A session
        // never seen absent is not ours to lock when a later read fails either.
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
        // A login appears only at the final post-close read, after a failed wait.
        // The second case first observes absence inside that wait, not before it.
        // An unread final state cannot discharge that same lock obligation.
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
        // The same way into the session: launchctl asuser, then sudo -n -u <admin> with a
        // fixed environment and no shell.
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
    /// off, then granted at the first re-probe. Returns what it did and every line it said.
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
        (screened, said.into_inner().unwrap())
    }

    fn lock_requests(host: &Host<'_>) -> usize {
        let lock = lock_cmd(&admin()).argv.join(" ");
        host.seen().iter().filter(|s| **s == lock).count()
    }

    #[test]
    fn a_walk_that_waited_for_the_login_locks_that_session_once_the_relay_closed() {
        // The login window at the first reading (VM, 27.0: exit 125), the admin logged in
        // over the relay by the second: macOS would leave that session unlocked at the Mac.
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
            host.seen().last(),
            Some(&lock_cmd(&admin()).argv.join(" ")),
            "the lock is the walk's last request"
        );
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
        // Locked or not, a session the walk did not see begin is never locked by it.
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        let (screened, said) = screen_fda(&host, &sys);
        assert_eq!(screened.walked[0].done, Done::Granted);
        assert_eq!(screened.session, Session::Kept);
        assert_eq!(lock_requests(&host), 0, "{:?}", host.seen());
        assert!(!said.iter().any(|l| l.starts_with("locked")), "{said:?}");
        // The reading before the relay found the session; one that found none during the
        // walk (a logout and a login over the relay) still means the walk let a login in.
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
        let (screened, _) = screen_fda(&host, &sys);
        assert_eq!(screened.session, Session::Locked);
    }

    #[test]
    fn the_session_is_read_before_anything_is_relayed_and_a_reading_that_did_not_run_relays_nothing()
     {
        // The reading the lock is decided by comes before the relay opens, so no login over
        // the relay can come before it.
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
        // A first request that did not run read nothing, so it cannot say the session was
        // there, and no lock could be promised: nothing is relayed and no viewer opens.
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
        // Ctrl-C at the first reading over the relay; the admin had logged in by the next.
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
        assert_eq!(lock_requests(&host), 1);
        assert_eq!(*host.closed_at_lock.lock().unwrap(), Some(true));
        assert_eq!(
            sys.calls(),
            relay_calls(),
            "caught until the lock was decided"
        );
        assert!(!host.seen().iter().any(|s| s.contains("/usr/bin/open")));
        assert!(said.iter().any(|l| l.starts_with("locked admin's session")));
        // Nobody had logged in: nothing to lock, and the walk ends with the stop.
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
        assert_eq!(lock_requests(&host), 0);
        assert_eq!(sys.calls(), relay_calls());
    }

    #[test]
    fn a_stop_during_a_step_still_locks_a_session_logged_in_over_the_relay_and_none_other() {
        // The admin logged in at the second reading; Ctrl-C while the FDA row still read off.
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
        // Said once the login was seen: a kill or a lost host leaves it unlocked.
        assert!(
            said.iter()
                .any(|l| l.starts_with("admin logged in over Screen Sharing")
                    && l.contains("if aterm is killed")),
            "{said:?}"
        );
        // A session there before the relay: the stop locks nothing.
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
            // The agent's own answer.
            (
                Ran {
                    status: Some(1),
                    stdout: b"not locked: SACLockScreenImmediate answered 22\n".to_vec(),
                    stderr: Vec::new(),
                },
                "SACLockScreenImmediate answered 22".to_owned(),
            ),
            // An agent from before `lock-screen`.
            (
                Ran {
                    status: Some(2),
                    stdout: format!("{refused_verb}\n").into_bytes(),
                    stderr: Vec::new(),
                },
                format!("exit 2: {refused_verb}"),
            ),
            // sudo or launchctl failed before the agent ran.
            (
                Ran::exit(1, "sudo: a password is required\n"),
                "exit 1: sudo: a password is required".to_owned(),
            ),
        ] {
            let sys = relaying();
            let host = Host {
                sys: Some(&sys),
                lock: Some(answer),
                ..Host::default()
            };
            host.logs_in_after(2);
            let (screened, said) = screen_fda(&host, &sys);
            assert_eq!(screened.session, Session::Unlocked(why.clone()));
            assert_eq!(lock_requests(&host), 1);
            assert!(!said.iter().any(|l| l.starts_with("locked")), "{said:?}");
            let line = screened.session.unlocked_line("lab-07", "admin").unwrap();
            assert_eq!(
                line,
                format!(
                    "lab-07: the Mac is still logged in as admin and unlocked at its display: \
                     the lock after the walk failed ({why})"
                )
            );
        }
        // The lock request itself did not run.
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
            matches!(&screened.session, Session::Unlocked(why) if why.starts_with("the request did not run") && why.contains("Connection refused")),
            "{:?}",
            screened.session
        );
    }

    #[test]
    fn here_never_locks_even_after_waiting_for_the_login() {
        // A person at the keyboard logged in themselves: their session is theirs.
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
        assert_eq!(lock_requests(&host), 0, "{:?}", host.seen());
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
}
