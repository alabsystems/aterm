// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ONE SANDBOXED GROK BUILD SIGN-IN PER MACHINE (`aterm harness signin --cli grok`,
//! `docs/DESIGN-ai-accounts-multi-cli-2026-10-23.md` §6, "Grok sign-in gate").
//!
//! Signed out, Grok Build draws its device sign-in instead of a composer and opens accounts.x.ai
//! in the default browser through LaunchServices, whatever its `PATH` (§5 (g)). Four signed-out
//! launches opened four browser tabs, and nothing deduplicated them. This module is the one
//! place a signed-out Grok Build under aterm is signed in:
//! - **Signed in is read from files and the environment, never by running grok** ([`signed_in`]):
//!   `GROK_AUTH` or `XAI_API_KEY` set, else the login file `GROK_AUTH_PATH` names, else
//!   `${GROK_HOME:-~/.grok}/auth.json`, non-empty and not refused: a login file grok refused (a
//!   grok started after it was written still drew its sign-in screen, [`wall_login`]) carries a
//!   `<file>.refused` marker at least as new as itself, and counts as signed out until a newer
//!   login replaces it ([`mark_refused`]).
//! - **One sign-in at a time on this machine**: a signed-out caller takes the `flock(2)` on
//!   [`LOCK_FILE`] in the harness state directory. A caller that finds it held waits on it
//!   (`--wait`) and never starts a second sign-in: once it holds the lock, a login another
//!   sign-in placed signs it in, and a sign-in that ended without one ends it too ([`run`]).
//!   Only a caller takes the lock: whether a sign-in runs is read from the leader's record
//!   ([`RUNNING_FILE`], [`running`]), so a reader never holds the lock a caller needs.
//! - **Grok's own device route under the no-desk sandbox**: `grok login --device-auth` in a
//!   private staging home, under `/usr/bin/sandbox-exec` with
//!   [`super::login_drive::NO_DESK`], after [`super::login_drive::sandbox_refusal`]. Its browser
//!   open fails there, and it still prints its page and code. A missing or refusing sandbox runs
//!   nothing: there is no unsandboxed fallback.
//! - **One owner need, and aterm opens no page**: the page must be `https` on auth.x.ai or
//!   accounts.x.ai; it and the code become one need ([`NEED_OWNER`], [`need_text`]), printed in
//!   the caller's terminal as a link and published on its session ([`Owner`]).
//! - **The login goes to the home grok reads**: once grok says `Signed in` and exits 0 with a
//!   non-empty login file, the file is placed `0600` at [`target`], the need is taken down, and
//!   the accounts ledger keeps the page's host, the flow and the outcome only.
//! - **Never a loop**: one invocation runs one sign-in at most. After one that ended without a
//!   login, the next starts only for a person's own launch (a terminal, [`Gate::ack`]); a
//!   background caller waits [`PERSON_BACKOFF_SECS`] ([`Ended::Backoff`]).

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::login_drive::{self, Approval, Approver, Drove, Handoff, Outcome};
use crate::ai::connector::{Cli, Drive, Flow, GROK_SIGNIN};

/// The machine-wide lock one sign-in holds, in the harness state directory.
pub const LOCK_FILE: &str = "signin-grok.lock";

/// The record of the last sign-in's end ([`Record`]), beside [`LOCK_FILE`].
pub const RECORD_FILE: &str = "signin-grok.state";

/// The record of the sign-in that runs now, beside [`LOCK_FILE`]: `pid=<n> birth=<s.us|-> t=<unix>`,
/// written by the leader while it holds the lock and removed before it lets go ([`running`]).
pub const RUNNING_FILE: &str = "signin-grok.running";

/// The suffix of a login file's refusal marker (`auth.json.refused`, [`mark_refused`]).
pub const REFUSED_SUFFIX: &str = ".refused";

/// The keyed attention owner of the one need (`meta set attention owner=signin:grok kind=ask`).
pub const NEED_OWNER: &str = "signin:grok";

/// The account the accounts ledger names for the default home's login.
pub const LEDGER_ACCOUNT: &str = "default-home";

/// `EX_UNAVAILABLE`: the exit of every end that leaves this machine signed out.
pub const EXIT_SIGNED_OUT: u8 = 69;

/// `EX_TEMPFAIL`: another sign-in runs, and the caller did not ask to wait for it.
pub const EXIT_BUSY: u8 = 75;

/// How long a background caller leaves the sign-in alone after one ended without a login (the
/// accounts upkeep's `PERSON_BACKOFF`): each try starts a live device sign-in.
pub const PERSON_BACKOFF_SECS: i64 = 6 * 60 * 60;

/// How long a waiter waits for the lock past the sign-in's own hang detector.
const LOCK_GRACE: Duration = Duration::from_secs(2 * 60);

/// How often a waiter tries the lock, and the approver looks for an end.
const POLL: Duration = Duration::from_millis(100);

/// The most bytes of a login file that are placed.
const LOGIN_MAX: u64 = 1 << 20;

/// A staging home left by a caller that was killed is removed once it is this old.
const STALE_STAGING: Duration = Duration::from_secs(60 * 60);

/// The prefix of a sign-in's staging home in the harness state directory.
const STAGING_PREFIX: &str = ".signin-grok-";

/// Why this machine counts as signed in to Grok Build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signed {
    /// A credential variable is set (`GROK_AUTH`, `XAI_API_KEY`).
    Env(&'static str),
    /// A non-empty login file.
    File(PathBuf),
}

/// The variables whose non-empty value signs a Grok Build launch in, ahead of any login file
/// (`connector::GROK.credential_env`).
pub const CREDENTIAL_ENV: [&str; 2] = ["GROK_AUTH", "XAI_API_KEY"];

/// The variable naming the login file grok reads in place of its home's (lab gate (b)).
pub const LOGIN_FILE_ENV: &str = "GROK_AUTH_PATH";

/// The variable naming grok's home.
pub const HOME_ENV: &str = "GROK_HOME";

fn set(var: &dyn Fn(&str) -> Option<OsString>, name: &str) -> Option<OsString> {
    var(name).filter(|v| !v.is_empty())
}

fn non_empty_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// The refusal marker of the login file `file` ([`REFUSED_SUFFIX`]).
#[must_use]
pub fn refused_marker(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_os_string();
    name.push(REFUSED_SUFFIX);
    PathBuf::from(name)
}

/// Whether the login file `file` was refused: its marker ([`refused_marker`]) is there and not
/// older than it. A login written after the marker (grok's own sign-in, or this gate's) is not.
fn refused(file: &Path) -> bool {
    let Ok(marker) = std::fs::metadata(refused_marker(file)).and_then(|m| m.modified()) else {
        return false;
    };
    std::fs::metadata(file)
        .and_then(|m| m.modified())
        .is_ok_and(|written| written <= marker)
}

/// Whether the login file `file` signs a launch in: a non-empty file grok has not refused.
fn counts(file: &Path) -> bool {
    non_empty_file(file) && !refused(file)
}

/// Whether the login file `file` is one grok would start on: there, non-empty, and not refused
/// ([`refused_marker`] at least as new as it). Every reader of a Grok login outside this gate
/// that decides whether grok may start (a worker's sign-in floor) asks this, so none reads a
/// login grok refused as signed in.
#[must_use]
pub fn login_accepted(file: &Path) -> bool {
    counts(file)
}

/// The home grok reads its login from: `GROK_HOME` (absolute), else `<home>/.grok`.
fn grok_home(var: &dyn Fn(&str) -> Option<OsString>, home: Option<&Path>) -> Option<PathBuf> {
    match set(var, HOME_ENV) {
        Some(dir) => Some(PathBuf::from(dir)).filter(|d| d.is_absolute()),
        None => home
            .filter(|h| h.is_absolute())
            .map(|h| h.join(Cli::Grok.accounts().default_home)),
    }
}

/// Whether a Grok Build launch with this environment is signed in, read from files and the
/// environment only: a credential variable ([`CREDENTIAL_ENV`]); else, where
/// [`LOGIN_FILE_ENV`] is set, the file it names (grok then reads no other); else
/// `${GROK_HOME:-<home>/.grok}/auth.json`. Each file counts when it is a non-empty file grok has
/// not refused ([`refused_marker`]).
#[must_use]
pub fn signed_in(var: &dyn Fn(&str) -> Option<OsString>, home: Option<&Path>) -> Option<Signed> {
    if let Some(name) = CREDENTIAL_ENV.into_iter().find(|n| set(var, n).is_some()) {
        return Some(Signed::Env(name));
    }
    if let Some(file) = set(var, LOGIN_FILE_ENV) {
        let file = PathBuf::from(file);
        return counts(&file).then_some(Signed::File(file));
    }
    let file = grok_home(var, home)?.join("auth.json");
    counts(&file).then_some(Signed::File(file))
}

/// What a Grok Build sign-in wall says of this machine's login ([`wall_login`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WallLogin {
    /// No login signs a launch in.
    SignedOut,
    /// A login the walled grok may not have read: it appeared after that grok started (or the
    /// start is unknown, or a credential variable is set), so a restart there reads it.
    SignedIn(Signed),
    /// A login grok refused: written before the walled grok started, which read it and drew its
    /// sign-in screen anyway (an expired login it could not renew). [`mark_refused`] marks it.
    Refused(PathBuf),
}

/// What a Grok Build session on its sign-in wall says of this machine's login, where its grok
/// started at `started`: a login file older than that start was read by it and refused.
#[must_use]
pub fn wall_login(
    var: &dyn Fn(&str) -> Option<OsString>,
    home: Option<&Path>,
    started: Option<std::time::SystemTime>,
) -> WallLogin {
    match signed_in(var, home) {
        None => WallLogin::SignedOut,
        Some(Signed::File(file)) => {
            let written = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
            match (written, started) {
                (Some(written), Some(started)) if written < started => WallLogin::Refused(file),
                _ => WallLogin::SignedIn(Signed::File(file)),
            }
        }
        Some(signed @ Signed::Env(_)) => WallLogin::SignedIn(signed),
    }
}

/// Marks the login file `file` refused ([`refused_marker`]): every gate then reads this machine
/// as signed out, so the next launch is signed in through the one sandboxed sign-in instead of
/// a grok that opens its own page. A login written after the mark undoes it.
/// # Errors
/// The marker cannot be written.
pub fn mark_refused(file: &Path) -> io::Result<()> {
    super::upgrade_models::write_atomic_mode(
        &refused_marker(file),
        "grok drew its sign-in screen over this login; aterm reads it as signed out until a newer \
         login replaces it\n",
        Some(0o600),
    )
}

/// Where this gate places a login: `auth.json` in the home grok reads (`GROK_HOME`, else
/// `<home>/.grok`).
/// # Errors
/// [`LOGIN_FILE_ENV`] is set (a pooled login, which `aterm harness accounts renew --cli grok`
/// signs in), `GROK_HOME` is relative, or no home is known.
pub fn target(
    var: &dyn Fn(&str) -> Option<OsString>,
    home: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(file) = set(var, LOGIN_FILE_ENV) {
        return Err(format!(
            "{LOGIN_FILE_ENV} names {}, a pooled login, which `aterm harness accounts renew \
             --cli grok` signs in; this gate signs in only Grok Build's own home",
            PathBuf::from(file).display()
        ));
    }
    if set(var, HOME_ENV).is_some_and(|d| !Path::new(&d).is_absolute()) {
        return Err(format!("{HOME_ENV} is not an absolute path"));
    }
    grok_home(var, home)
        .map(|dir| dir.join("auth.json"))
        .ok_or_else(|| "no home directory is known".to_owned())
}

/// The last sign-in's end, as [`RECORD_FILE`] keeps it: `gen=<n> t=<unix> outcome=<word>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    /// How many sign-ins have ended on this machine.
    pub generation: u64,
    /// When the last one ended (unix seconds).
    pub t: i64,
    /// How it ended ([`Outcome::word`], or `signed-in-elsewhere`, `stopped`).
    pub outcome: String,
}

impl Record {
    /// Whether it ended without a login.
    #[must_use]
    pub fn unsigned(&self) -> bool {
        self.generation > 0 && !matches!(self.outcome.as_str(), "signed-in" | "signed-in-elsewhere")
    }

    fn line(&self) -> String {
        format!(
            "gen={} t={} outcome={}\n",
            self.generation, self.t, self.outcome
        )
    }

    fn parse(text: &str) -> Record {
        let mut record = Record::default();
        for word in text.split_whitespace() {
            if let Some(v) = word.strip_prefix("gen=") {
                record.generation = v.parse().unwrap_or(0);
            } else if let Some(v) = word.strip_prefix("t=") {
                record.t = v.parse().unwrap_or(0);
            } else if let Some(v) = word.strip_prefix("outcome=") {
                record.outcome = v
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .take(32)
                    .collect();
            }
        }
        record
    }
}

/// The record in `state` ([`RECORD_FILE`]); the default where there is none.
#[must_use]
pub fn read_record(state: &Path) -> Record {
    std::fs::read_to_string(state.join(RECORD_FILE))
        .map(|t| Record::parse(&t))
        .unwrap_or_default()
}

fn write_record(state: &Path, record: &Record) -> io::Result<()> {
    super::upgrade_models::write_atomic_mode(&state.join(RECORD_FILE), &record.line(), Some(0o600))
}

/// The one need's words (`meta set attention owner=signin:grok kind=ask`): the code first, so a
/// clipped row still shows it, then the page, else its host where the whole line would pass the
/// attention cap ([`aterm_types::control_verbs::META_ATTENTION_KEYED_MAX`]).
#[must_use]
pub fn need_text(url: &str, code: Option<&str>) -> String {
    let code = code.unwrap_or("none");
    let whole = format!("approve Grok Build sign-in: code {code} at {url}");
    if whole.len() <= aterm_types::control_verbs::META_ATTENTION_KEYED_MAX {
        return whole;
    }
    let host = login_drive::https_host(url).unwrap_or_default();
    format!("approve Grok Build sign-in: code {code} at https://{host}")
}

/// The terminal's line for the need, a report line aterm reads (`NEEDS YOU: …`).
#[must_use]
pub fn needs_you_line(url: &str, code: Option<&str>) -> String {
    format!(
        "NEEDS YOU: approve Grok Build sign-in, code {}, at {url}",
        code.unwrap_or("none")
    )
}

/// Where the one need goes and the person reads: the caller's terminal and session.
pub trait Owner {
    /// Puts the need up, or replaces it.
    fn need(&mut self, text: &str, line: &str, url: &str);
    /// Takes the need down.
    fn clear(&mut self);
    /// Tells the person one line.
    fn say(&mut self, line: &str);
}

/// One gate's inputs, injected so a test drives it with a stand-in vendor and sandbox.
pub struct Gate<'a> {
    /// The harness state directory: the lock, the record, the staging homes.
    pub state: &'a Path,
    /// The owner's home.
    pub home: Option<&'a Path>,
    /// This process's environment.
    pub var: &'a dyn Fn(&str) -> Option<OsString>,
    /// grok's binary ([`login_drive::program_of`]) and the arguments before its own.
    pub program: (&'a Path, &'a [OsString]),
    /// The sandbox its sign-in runs under ([`login_drive::SANDBOX_EXEC`]).
    pub sandbox: Option<&'a Path>,
    /// The sign-in's hang detector ([`login_drive::HANG`]).
    pub hang: Duration,
    /// Waits for a sign-in another caller runs, instead of ending [`Ended::Busy`].
    pub wait: bool,
    /// A person's own launch (a terminal): it may start a sign-in after one that ended without
    /// a login.
    pub ack: bool,
    /// Whether this caller was asked to stop (Ctrl-C, a hangup, a stop).
    pub stopped: &'a dyn Fn() -> bool,
    /// Unix seconds now.
    pub now: &'a dyn Fn() -> i64,
}

/// How one gate ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// Signed in before anything ran.
    Already(Signed),
    /// This caller's sign-in placed the login at the path, as the email (where grok named one).
    SignedIn {
        /// The login file.
        file: PathBuf,
        /// The account grok said it signed in.
        email: Option<String>,
    },
    /// Another sign-in, or a login made some other way, signed this machine in.
    SignedInElsewhere(Signed),
    /// Another caller's sign-in runs, and this one did not ask to wait.
    Busy,
    /// The sign-in this caller waited on ended without a login, how ([`Record::outcome`]).
    Waited(String),
    /// The last sign-in ended without a login at the time, and a background caller leaves it
    /// alone until `until`.
    Backoff {
        /// How it ended.
        outcome: String,
        /// When a background caller may start one again (unix seconds).
        until: i64,
    },
    /// Nothing ran, why.
    NotRun(String),
    /// This caller's sign-in ended without a login: its ledger word and why.
    Unsigned {
        /// [`Outcome::word`], or `stopped`.
        word: String,
        /// In words.
        why: String,
    },
}

impl Ended {
    /// The exit status: 0 where this machine is signed in, [`EXIT_BUSY`] for [`Ended::Busy`],
    /// else [`EXIT_SIGNED_OUT`].
    #[must_use]
    pub fn exit(&self) -> u8 {
        match self {
            Ended::Already(_) | Ended::SignedIn { .. } | Ended::SignedInElsewhere(_) => 0,
            Ended::Busy => EXIT_BUSY,
            _ => EXIT_SIGNED_OUT,
        }
    }

    /// Its word, as `--json` names it.
    #[must_use]
    pub fn word(&self) -> &str {
        match self {
            Ended::Already(_) => "already-signed-in",
            Ended::SignedIn { .. } => "signed-in",
            Ended::SignedInElsewhere(_) => "signed-in-elsewhere",
            Ended::Busy => "busy",
            Ended::Waited(_) => "waited-unsigned",
            Ended::Backoff { .. } => "backoff",
            Ended::NotRun(_) => "not-run",
            Ended::Unsigned { word, .. } => word,
        }
    }

    /// One line for the person.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Ended::Already(Signed::Env(name)) => {
                format!("Grok Build is signed in on this machine ({name} is set)")
            }
            Ended::Already(Signed::File(file)) | Ended::SignedInElsewhere(Signed::File(file)) => {
                format!(
                    "Grok Build is signed in on this machine ({})",
                    file.display()
                )
            }
            Ended::SignedInElsewhere(Signed::Env(name)) => {
                format!("Grok Build is signed in on this machine ({name} is set)")
            }
            Ended::SignedIn { file, email } => match email {
                Some(email) => format!(
                    "Grok Build is signed in on this machine as {email} ({})",
                    file.display()
                ),
                None => format!(
                    "Grok Build is signed in on this machine ({})",
                    file.display()
                ),
            },
            Ended::Busy => "another session is signing Grok Build in on this machine; \
                            `aterm harness signin --cli grok --wait` waits for it"
                .to_owned(),
            Ended::Waited(outcome) => format!(
                "the Grok Build sign-in this waited on ended without a login ({outcome}); \
                 nothing more was started"
            ),
            Ended::Backoff { outcome, until } => format!(
                "the last Grok Build sign-in ended without a login ({outcome}); a launch from a \
                 terminal starts a new one, and a background one waits until {}",
                aterm_types::rfc3339::format_rfc3339(u64::try_from(*until).unwrap_or(0))
            ),
            Ended::NotRun(why) => format!("Grok Build was not signed in: {why}"),
            Ended::Unsigned { why, .. } => format!("Grok Build was not signed in: {why}"),
        }
    }
}

/// The lock file, opened 0600 in `state` (made private where it is missing).
fn lock_file(state: &Path) -> io::Result<std::fs::File> {
    aterm_types::fs_restricted::ensure_private_dir(state)?;
    let mut open = std::fs::OpenOptions::new();
    open.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600);
    }
    open.open(state.join(LOCK_FILE))
}

/// A held [`LOCK_FILE`], released when it drops.
struct Held(std::fs::File);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn try_take(file: std::fs::File) -> io::Result<Result<Held, std::fs::File>> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard's own take: its Drop releases by LOCK_UN"
    )]
    let taken = file.try_lock();
    match taken {
        Ok(()) => Ok(Ok(Held(file))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(Err(file)),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// This process's kernel start record, as [`RUNNING_FILE`] keeps it (`-` where there is none).
fn birth_word(pid: u32) -> String {
    i32::try_from(pid)
        .ok()
        .and_then(aterm_uds::process::read_process_birth)
        .map_or_else(
            || "-".to_owned(),
            |b| format!("{}.{:06}", b.seconds, b.microseconds),
        )
}

/// The leader's record ([`RUNNING_FILE`]) while it signs this machine in: written once it holds
/// the lock and leads, removed when it drops (before the lock it is declared after).
struct Leading(PathBuf);

impl Leading {
    fn write(state: &Path, now: i64) -> Leading {
        let pid = std::process::id();
        let file = state.join(RUNNING_FILE);
        let _ = super::upgrade_models::write_atomic_mode(
            &file,
            &format!("pid={pid} birth={} t={now}\n", birth_word(pid)),
            Some(0o600),
        );
        Leading(file)
    }
}

impl Drop for Leading {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Whether a sign-in runs in `state` now, read from its leader's record ([`RUNNING_FILE`]) and
/// never from the lock, which a reader would hold for an instant a caller needs: the record names
/// a live process with the start it recorded, and is younger than a sign-in can run
/// ([`login_drive::HANG`] and [`LOCK_GRACE`]). A leader killed outright leaves a record whose
/// process is gone, or another one under its pid with a different start.
#[must_use]
pub fn running(state: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(state.join(RUNNING_FILE)) else {
        return false;
    };
    let (mut pid, mut birth, mut t) = (None, None, None);
    for word in text.split_whitespace() {
        if let Some(v) = word.strip_prefix("pid=") {
            pid = v.parse::<u32>().ok();
        } else if let Some(v) = word.strip_prefix("birth=") {
            birth = Some(v.to_owned());
        } else if let Some(v) = word.strip_prefix("t=") {
            t = v.parse::<i64>().ok();
        }
    }
    let (Some(pid), Some(birth), Some(t)) = (pid, birth, t) else {
        return false;
    };
    let longest = i64::try_from((login_drive::HANG + LOCK_GRACE).as_secs()).unwrap_or(i64::MAX);
    let age = crate::supervise::limit::unix_now().saturating_sub(t);
    (0..=longest).contains(&age) && aterm_uds::process::pid_alive(pid) && birth_word(pid) == birth
}

/// Signs this machine in to Grok Build once ([`Gate`]), the one need through `owner`, the end's
/// ledger line through `ledger` (`<host> device <outcome>`, never the page or the code).
pub fn run(gate: &Gate<'_>, owner: &mut dyn Owner, ledger: &mut dyn FnMut(&str)) -> Ended {
    if let Some(signed) = signed_in(gate.var, gate.home) {
        return Ended::Already(signed);
    }
    let target = match target(gate.var, gate.home) {
        Ok(target) => target,
        Err(why) => return Ended::NotRun(why),
    };
    let file = match lock_file(gate.state) {
        Ok(file) => file,
        Err(e) => {
            return Ended::NotRun(format!(
                "the sign-in lock in {} cannot be opened: {e}",
                gate.state.display()
            ));
        }
    };
    let before = read_record(gate.state);
    let _held = match try_take(file) {
        Ok(Ok(held)) => held,
        Ok(Err(_)) if !gate.wait => return Ended::Busy,
        Ok(Err(file)) => {
            match wait_for(file, gate, owner) {
                Ok(held) => {
                    if let Some(signed) = signed_in(gate.var, gate.home) {
                        return Ended::SignedInElsewhere(signed);
                    }
                    let after = read_record(gate.state);
                    if after.generation != before.generation {
                        return Ended::Waited(after.outcome);
                    }
                    // The sign-in it waited on ended without a record (its caller was killed):
                    // this caller leads.
                    held
                }
                Err(ended) => return ended,
            }
        }
        Err(e) => return Ended::NotRun(format!("the sign-in lock failed: {e}")),
    };
    if let Some(signed) = signed_in(gate.var, gate.home) {
        return Ended::Already(signed);
    }
    let last = read_record(gate.state);
    let now = (gate.now)();
    if !gate.ack && last.unsigned() && now.saturating_sub(last.t) < PERSON_BACKOFF_SECS {
        return Ended::Backoff {
            until: last.t.saturating_add(PERSON_BACKOFF_SECS),
            outcome: last.outcome,
        };
    }
    remove_stale_staging(gate.state);
    let leading = Leading::write(gate.state, now);
    let ended = lead(gate, &target, owner);
    drop(leading);
    owner.clear();
    let (word, host) = match &ended {
        Lead::NotRun(why) => return Ended::NotRun(why.clone()),
        Lead::Drove(drove) => (drove.outcome.word(), drove.host.clone()),
        Lead::Stopped(host) => ("stopped".to_owned(), host.clone()),
        Lead::Elsewhere(host, _) => ("signed-in-elsewhere".to_owned(), host.clone()),
    };
    let _ = write_record(
        gate.state,
        &Record {
            generation: last.generation.max(before.generation).saturating_add(1),
            t: (gate.now)(),
            outcome: word.clone(),
        },
    );
    ledger(&format!(
        "{} {} {word}",
        host.as_deref().unwrap_or("no-page"),
        Flow::Device.as_str()
    ));
    match ended {
        Lead::Drove(Drove {
            outcome: Outcome::SignedIn { email, .. },
            ..
        }) => Ended::SignedIn {
            file: target,
            email,
        },
        Lead::Drove(drove) => Ended::Unsigned {
            why: unsigned_why(&drove.outcome),
            word,
        },
        Lead::Elsewhere(_, signed) => Ended::SignedInElsewhere(signed),
        Lead::Stopped(_) | Lead::NotRun(_) => Ended::Unsigned {
            word,
            why: "it was stopped before the page was approved".to_owned(),
        },
    }
}

fn unsigned_why(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Hung => format!(
            "nobody approved its page within {} min",
            login_drive::HANG.as_secs() / 60
        ),
        Outcome::OffPin => "it named a page off auth.x.ai and accounts.x.ai, so nothing was \
                            shown and it was ended"
            .to_owned(),
        Outcome::Exited(Some(code)) => {
            format!("grok exited {code} without saying it signed in (an expired code exits so)")
        }
        Outcome::Exited(None) => "grok was ended by a signal".to_owned(),
        Outcome::SignedIn { .. } => "grok said it signed in but left no login file".to_owned(),
        Outcome::NotRun(why) => why.clone(),
        Outcome::Refused(why) | Outcome::NeedsYou(why) | Outcome::NeedsSignIn(why) => why.clone(),
        Outcome::Failed => "grok said its sign-in failed".to_owned(),
    }
}

/// Waits on the held lock for the sign-in another caller runs, up to its hang detector and
/// [`LOCK_GRACE`]. It tells the person it waits once a leader's record shows one runs
/// ([`running`]): a caller that holds the lock only to read the files ends at once, and is
/// never announced as a sign-in.
fn wait_for(
    mut file: std::fs::File,
    gate: &Gate<'_>,
    owner: &mut dyn Owner,
) -> Result<Held, Ended> {
    let until = Instant::now() + gate.hang + LOCK_GRACE;
    let mut told = false;
    loop {
        if !told && running(gate.state) {
            told = true;
            owner.say(
                "another session is signing Grok Build in on this machine; waiting for it (its \
                 code is on the menu bar)",
            );
        }
        if (gate.stopped)() {
            return Err(Ended::Unsigned {
                word: "stopped".to_owned(),
                why: "it was stopped while it waited".to_owned(),
            });
        }
        if Instant::now() >= until {
            return Err(Ended::NotRun(format!(
                "another sign-in held the lock past {} min",
                (gate.hang + LOCK_GRACE).as_secs() / 60
            )));
        }
        std::thread::sleep(POLL);
        file = match try_take(file) {
            Ok(Ok(held)) => return Ok(held),
            Ok(Err(file)) => file,
            Err(e) => return Err(Ended::NotRun(format!("the sign-in lock failed: {e}"))),
        };
    }
}

fn remove_stale_staging(state: &Path) {
    let Ok(entries) = std::fs::read_dir(state) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(STAGING_PREFIX) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= STALE_STAGING);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// How the leader's sign-in ended.
enum Lead {
    /// It ran to an end.
    Drove(Drove),
    /// The caller was stopped while the need stood; the page's host.
    Stopped(Option<String>),
    /// A login made some other way appeared while the need stood; the page's host.
    Elsewhere(Option<String>, Signed),
    /// It did not run.
    NotRun(String),
}

/// The approver of the leader's sign-in: it approves nothing. It puts the one need up and waits
/// until grok says it is done (or closes its output), the deadline passes, the caller is
/// stopped, or a login made some other way appears.
struct NeedApprover<'a, 'b> {
    owner: &'a mut dyn Owner,
    gate: &'a Gate<'b>,
    stopped: bool,
    elsewhere: Option<Signed>,
}

impl Approver for NeedApprover<'_, '_> {
    fn approve(&mut self, handoff: &Handoff<'_>) -> Approval {
        self.owner.need(
            &need_text(handoff.url, handoff.code),
            &needs_you_line(handoff.url, handoff.code),
            handoff.url,
        );
        loop {
            if (handoff.finished)() || Instant::now() >= handoff.deadline {
                return Approval::Approved { page: false };
            }
            if (self.gate.stopped)() {
                self.stopped = true;
                return Approval::Refused("stopped".to_owned());
            }
            if let Some(signed) = signed_in(self.gate.var, self.gate.home) {
                self.elsewhere = Some(signed);
                return Approval::Refused("signed in elsewhere".to_owned());
            }
            std::thread::sleep(POLL);
        }
    }
}

/// The leader's sign-in: grok's device route in a private staging home under the no-desk
/// sandbox, its page handed to the need, its login placed at `target`.
fn lead(gate: &Gate<'_>, target: &Path, owner: &mut dyn Owner) -> Lead {
    let Some(route) = GROK_SIGNIN.route(Some(Flow::Device)) else {
        return Lead::NotRun("this build has no Grok Build device sign-in".to_owned());
    };
    let Drive::Lines {
        grammar,
        completion,
        ..
    } = &route.drive
    else {
        return Lead::NotRun("Grok Build's device sign-in is not a line route".to_owned());
    };
    let Some(hosts) = route.hosts.of(None) else {
        return Lead::NotRun("Grok Build's device sign-in names no hosts".to_owned());
    };
    let sandbox = if GROK_SIGNIN.no_desk.contains(&std::env::consts::OS) {
        let Some(sandbox) = gate.sandbox.filter(|s| s.is_file()) else {
            return Lead::NotRun(format!(
                "Grok Build opens this machine's default browser through the OS, and the sandbox \
                 that stops it ({}) is not here; nothing was run",
                gate.sandbox
                    .unwrap_or(Path::new(login_drive::SANDBOX_EXEC))
                    .display()
            ));
        };
        if let Some(why) = login_drive::sandbox_refusal(sandbox) {
            return Lead::NotRun(format!(
                "Grok Build opens this machine's default browser through the OS, and the sandbox \
                 that stops it cannot run here: {why}; nothing was run"
            ));
        }
        Some(sandbox)
    } else {
        None
    };
    let (program, lead_args) = gate.program;
    let (program, path, _opener_free) = match login_drive::child_program(
        program,
        None,
        GROK_SIGNIN.no_opener,
        gate.state,
        gate.var,
    ) {
        Ok(child) => child,
        Err(e) => return Lead::NotRun(format!("no private PATH could be made: {e}")),
    };
    if !program.is_file() {
        return Lead::NotRun(format!(
            "`{}` is not installed here (`aterm pkg install grok` installs it)",
            program.display()
        ));
    }
    let staging = match aterm_tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(gate.state)
    {
        Ok(dir) => dir,
        Err(e) => return Lead::NotRun(format!("no staging home could be made: {e}")),
    };
    let sandboxed: Vec<OsString>;
    let (program, lead_args): (&Path, &[OsString]) = match sandbox {
        Some(sandbox) => {
            sandboxed = login_drive::no_desk_argv(&program, lead_args);
            (sandbox, sandboxed.as_slice())
        }
        None => (program.as_path(), lead_args),
    };
    let env = login_drive::child_env(Cli::Grok, &GROK_SIGNIN, staging.path(), &path, gate.var);
    let args = login_drive::child_args(route, None);
    let mut approver = NeedApprover {
        owner,
        gate,
        stopped: false,
        elsewhere: None,
    };
    let drove = login_drive::drive(
        &login_drive::Spec {
            program,
            lead: lead_args,
            args: &args,
            env: &env,
            cwd: staging.path(),
            grammar,
            completion: *completion,
            hosts,
            email: None,
            hang: gate.hang,
        },
        &mut approver,
    );
    if approver.stopped {
        return Lead::Stopped(drove.host);
    }
    if let Some(signed) = approver.elsewhere.take() {
        return Lead::Elsewhere(drove.host, signed);
    }
    let Outcome::SignedIn { .. } = drove.outcome else {
        return Lead::Drove(drove);
    };
    match place(&staging.path().join("auth.json"), target) {
        Ok(()) => Lead::Drove(drove),
        Err(why) => {
            if let Some(signed) = signed_in(gate.var, gate.home) {
                return Lead::Elsewhere(drove.host, signed);
            }
            Lead::Drove(Drove {
                outcome: Outcome::NotRun(why),
                host: drove.host,
            })
        }
    }
}

/// Places the staging home's login at `target`, `0600`, its directory made `0700` where it is
/// missing, and takes down its refusal marker; a login already at `target` that signs a launch in
/// (non-empty, not refused) is kept.
/// # Errors
/// The staging login is missing, empty, too large or not text, or it cannot be written.
fn place(staged: &Path, target: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(staged)
        .map_err(|_| "grok said it signed in but left no login file".to_owned())?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > LOGIN_MAX {
        return Err("grok's login file is empty, too large or not a file".to_owned());
    }
    let text = std::fs::read_to_string(staged)
        .map_err(|e| format!("grok's login file cannot be read: {e}"))?;
    if counts(target) {
        return Ok(());
    }
    if let Some(dir) = target.parent() {
        aterm_types::fs_restricted::create_dir_all_unshared(dir, 0o700)
            .map_err(|e| format!("{} cannot be made: {e}", dir.display()))?;
    }
    super::upgrade_models::write_atomic_mode(target, &text, Some(0o600))
        .map_err(|e| format!("{} cannot be written: {e}", target.display()))?;
    // The new login is newer than any marker, so it counts even where the marker stays.
    let _ = std::fs::remove_file(refused_marker(target));
    Ok(())
}

/// The caller's terminal and session as the need's [`Owner`]: lines on `err`, the page as an
/// OSC 8 link where `err` is a terminal, and the need as the keyed attention entry
/// [`NEED_OWNER`] on the caller's own session through `aterm ctl @self` where it runs in one.
pub struct SessionOwner<'a> {
    /// Where the person reads.
    pub err: &'a mut dyn io::Write,
    /// `err` is a terminal.
    pub link: bool,
    /// The control client of the caller's session, where it runs in one.
    pub ctl: Option<crate::CtlClient>,
    /// The need stands on the session.
    pub up: bool,
}

impl SessionOwner<'_> {
    fn attention(&self, args: &[&str]) {
        if let Some(ctl) = &self.ctl {
            let mut argv = vec!["@self", "meta"];
            argv.extend_from_slice(args);
            let _ = ctl.run(&argv);
        }
    }
}

impl Owner for SessionOwner<'_> {
    fn need(&mut self, text: &str, line: &str, url: &str) {
        let shown = if self.link {
            line.replace(
                url,
                &format!("\u{1b}]8;;{url}\u{1b}\\{url}\u{1b}]8;;\u{1b}\\"),
            )
        } else {
            line.to_owned()
        };
        let _ = writeln!(self.err, "{shown}");
        let _ = writeln!(
            self.err,
            "waiting up to {} min for the approval; aterm opens no page (Ctrl-C stops)",
            login_drive::HANG.as_secs() / 60
        );
        let _ = self.err.flush();
        let owner = format!("owner={NEED_OWNER}");
        self.attention(&["set", "attention", &owner, "kind=ask", text]);
        self.up = true;
    }

    fn clear(&mut self) {
        if self.up {
            let owner = format!("owner={NEED_OWNER}");
            self.attention(&["unset", "attention", &owner]);
            self.up = false;
        }
    }

    fn say(&mut self, line: &str) {
        let _ = writeln!(self.err, "{line}");
        let _ = self.err.flush();
    }
}

#[cfg(test)]
#[path = "grok_signin_tests.rs"]
mod tests;
