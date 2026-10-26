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
//! - **aterm approves the page itself first** ([`Auto`], [`Ladder`]): the page must be `https`
//!   on auth.x.ai or accounts.x.ai, and before any person is asked it goes to the headless
//!   browser ([`super::headless_oauth::VendorApprover`]) as the Grok row that signs the default
//!   home in ([`pick_row`] over the renewal-bound roster: the default home's row, else the pool's
//!   active row, else the default home's own key [`LEDGER_ACCOUNT`]), in that row's browser
//!   profile, typing its email and reading the vendor's mailed code only where a mailbox is
//!   configured for that email. That rung runs only where a headless browser is there and the
//!   row has an email and, for it, a profile or a mailbox ([`Ladder::ready`]), under the
//!   machine's one sign-in lock for every CLI ([`super::accounts::Store::signin_now`]), for at
//!   most [`AUTO_WITHIN`] of the code's life; where it has one of them and no headless browser
//!   is installed, the pinned chrome-headless-shell is fetched first, once grok can run (its
//!   sandbox, program and staging home are there, [`preflight`]) and before it starts, held to
//!   the browser lane's own pin ([`super::headless_fetch`], [`browser_rung`]). Its refusal
//!   (another code, another account, or grok signing another account in than the row's email
//!   once the rung ran) ends the sign-in fail-closed, with no need and no login placed; the
//!   next sign-in a person launches then skips the rung once and asks them ([`Record::by`]), so
//!   a refusing profile never leaves the machine without a way in. A stop for a person
//!   (`needs-sign-in`, `needs-you`) or no route ready goes on to the person.
//! - **A stop is heard on every rung**: the caller's Ctrl-C (or a hangup) ends the browser's
//!   download ([`super::headless_fetch::run_stoppable`]), keeps grok from starting after it,
//!   ends the automatic rung, and is seen before the need goes up.
//! - **Then one owner need, and aterm opens no page**: the page and the code become one need
//!   ([`NEED_OWNER`], [`need_words`]) that leads with the once-ever step that makes the next
//!   sign-in automatic ([`Setup`]: one sign-in in aterm's browser window where one that opens a
//!   window is installed, else the row's mailbox in the vault) and keeps the code and the page as
//!   the immediate alternative, short enough that the menu row shows the code for the label
//!   `adopt-default` gives; printed in the caller's terminal as a link and published on its
//!   session ([`Owner`]).
//! - **The login goes to the home grok reads**: once grok says `Signed in` and exits 0 with a
//!   non-empty login file, the file is placed `0600` at [`target`], the need is taken down, and
//!   the accounts ledger keeps the page's host, the flow, the outcome and each rung's result
//!   ([`ledger_line`], within [`super::accounts::LEDGER_WHY_MAX`]) only: never the page, the
//!   code or the email.
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

/// How long the automatic rung ([`Auto::approve`]) may hold the device page before the person
/// is asked: a third of the code's life ([`login_drive::HANG`]), so the person still has ten
/// minutes of the same code.
pub const AUTO_WITHIN: Duration = Duration::from_secs(5 * 60);

/// How long a waiter waits for the lock past the sign-in's own hang detector: two minutes, and
/// the longest fetch of the headless browser the leader may run before grok starts
/// ([`super::headless_fetch::FETCH_WITHIN`]).
const LOCK_GRACE: Duration =
    Duration::from_secs(2 * 60 + super::headless_fetch::FETCH_WITHIN.as_secs());

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

/// The last sign-in's end, as [`RECORD_FILE`] keeps it: `gen=<n> t=<unix> outcome=<word>`, and
/// `by=headless` where the automatic rung's refusal ended it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    /// How many sign-ins have ended on this machine.
    pub generation: u64,
    /// When the last one ended (unix seconds).
    pub t: i64,
    /// How it ended ([`Outcome::word`], or `signed-in-elsewhere`, `stopped`).
    pub outcome: String,
    /// The automatic rung refused it (another code, another account): a person's own launch
    /// after it skips that rung once and asks them ([`run`]).
    pub by: bool,
}

impl Record {
    /// Whether it ended without a login.
    #[must_use]
    pub fn unsigned(&self) -> bool {
        self.generation > 0 && !matches!(self.outcome.as_str(), "signed-in" | "signed-in-elsewhere")
    }

    fn line(&self) -> String {
        format!(
            "gen={} t={} outcome={}{}\n",
            self.generation,
            self.t,
            self.outcome,
            if self.by { " by=headless" } else { "" }
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
            } else if word == "by=headless" {
                record.by = true;
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

/// The label `accounts adopt-default` gives the default home's row in the once-ever setup
/// where the roster has none ([`Setup::Adopt`]): the label the first `accounts add` gives it.
pub const ADOPT_LABEL: &str = super::cli::ACCOUNTS_OWN_LABEL;

/// The once-ever step that makes every later Grok Build sign-in on this machine automatic: the
/// need leads with it ([`need_words`], [`needs_you_line`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setup {
    /// The row has an email and a Chromium that opens a window is installed: one sign-in in
    /// aterm's own browser window (`aterm harness accounts browser <label> --cli grok`, which
    /// keeps the row's profile signed in to x.ai), or the row's mailbox in the vault
    /// (`ai-mailbox.toml`), which reads the code x.ai mails.
    Window(String),
    /// The row has an email and no Chromium that opens a window is installed, so `accounts
    /// browser` would refuse: the row's mailbox in the vault, or such a browser installed first.
    Mailbox(String),
    /// The row names no email, which the window and the mailbox both need: add it first.
    Email(String),
    /// No row signs the default home in: adopt it under this label (one the roster does not
    /// list, [`adopt_label`]), with its email, first.
    Adopt(String),
}

impl Setup {
    /// Its first words in the attention entry: short, so the menu row (72 bytes, an ellipsis
    /// where it is cut) still shows the code after it for the label `adopt-default` gives.
    fn short(&self) -> String {
        match self {
            Setup::Window(label) => format!("once: sign {label} in at the desk"),
            Setup::Mailbox(label) => format!("once: {label}'s mailbox in the vault"),
            Setup::Email(label) => format!("once: an email on {label}'s grok row"),
            Setup::Adopt(label) => format!("once: adopt the default home as {label}"),
        }
    }

    /// The command or file the short words name, after the page in the attention entry.
    fn exact(&self) -> String {
        match self {
            Setup::Window(label) => format!("aterm harness accounts browser {label} --cli grok"),
            Setup::Mailbox(_) => "ai-mailbox.toml".to_owned(),
            Setup::Email(_) => "email = \"…\" in Grok Build's roster".to_owned(),
            Setup::Adopt(label) => {
                format!("aterm harness accounts adopt-default {label} --email <you> --cli grok")
            }
        }
    }

    /// Its words in the terminal's `NEEDS YOU:` line.
    fn sentence(&self) -> String {
        match self {
            Setup::Window(label) => format!(
                "run `aterm harness accounts browser {label} --cli grok` at this machine's desk \
                 and sign in there, or put {label}'s mailbox in the vault (ai-mailbox.toml)"
            ),
            Setup::Mailbox(label) => format!(
                "put {label}'s mailbox in the vault (ai-mailbox.toml), or install a Chromium that \
                 opens a window (Google Chrome for Testing or Chromium) and run `aterm harness \
                 accounts browser {label} --cli grok` at this machine's desk"
            ),
            Setup::Email(label) => format!(
                "add `email = \"…\"` to {label}'s row in Grok Build's roster, then run `aterm \
                 harness accounts browser {label} --cli grok` at this machine's desk or put that \
                 mailbox in the vault (ai-mailbox.toml)"
            ),
            Setup::Adopt(label) => format!(
                "run `aterm harness accounts adopt-default {label} --email <your x.ai email> \
                 --cli grok`, then `aterm harness accounts browser {label} --cli grok` at this \
                 machine's desk, or put that mailbox in the vault (ai-mailbox.toml)"
            ),
        }
    }
}

/// `text` cut to at most `max` bytes at a character boundary.
fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// The one need's words (`meta set attention owner=signin:grok kind=ask`): the once-ever setup
/// first, in short words ([`Setup`]), then the code and the page as the immediate alternative,
/// then the setup's exact command or file: `once: sign main in at the desk; or code WMRT-8KQZ at
/// <page> (aterm harness accounts browser main --cli grok)`. Where that passes the attention cap
/// ([`aterm_types::control_verbs::META_ATTENTION_KEYED_MAX`]), the page's host, then without
/// the exact command. An admissible label (64 bytes at most) and a code the grammar admits (16
/// at most) always fit the last; anything longer is cut.
#[must_use]
pub fn need_words(setup: &Setup, url: &str, code: Option<&str>) -> String {
    let cap = aterm_types::control_verbs::META_ATTENTION_KEYED_MAX;
    let code = code.unwrap_or("none");
    let host = format!(
        "https://{}",
        login_drive::https_host(url).unwrap_or_default()
    );
    let (short, exact) = (setup.short(), setup.exact());
    for (page, with_exact) in [(url, true), (host.as_str(), true), (host.as_str(), false)] {
        let text = if with_exact {
            format!("{short}; or code {code} at {page} ({exact})")
        } else {
            format!("{short}; or code {code} at {page}")
        };
        if text.len() <= cap {
            return text;
        }
    }
    cut(format!("{short}; or code {code} at {host}"), cap)
}

/// The terminal's line for the need, a report line aterm reads (`NEEDS YOU: …`): the once-ever
/// setup first, then this sign-in's code and page.
#[must_use]
pub fn needs_you_line(setup: &Setup, url: &str, code: Option<&str>) -> String {
    format!(
        "NEEDS YOU: once, so Grok Build signs in by itself from now on: {}; or approve this \
         sign-in now: code {} at {url}",
        setup.sentence(),
        code.unwrap_or("none")
    )
}

/// The label the once-ever setup adopts the default home under ([`Setup::Adopt`]): the first of
/// [`ADOPT_LABEL`], [`LEDGER_ACCOUNT`] and `grok-home` that `roster` does not list (`accounts
/// adopt-default` refuses a listed one), else `<label>`.
#[must_use]
pub fn adopt_label(roster: &super::accounts::Roster) -> String {
    [ADOPT_LABEL, LEDGER_ACCOUNT, "grok-home"]
        .into_iter()
        .find(|label| roster.row(label).is_none())
        .unwrap_or("<label>")
        .to_owned()
}

/// Which row signs the default home in ([`pick_row`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowFrom {
    /// Grok Build's roster's row whose login is the default home (no `dir` once the renewals'
    /// bindings are applied: `accounts adopt-default --cli grok`).
    Default,
    /// The pool's active row (`accounts use <label> --cli grok`), where no row is the default
    /// home's: the account the person runs on.
    Active,
    /// No row: the default home's own key, [`LEDGER_ACCOUNT`] (its browser profile
    /// `vendor-browsers/grok-default-home`, [`super::headless_oauth::vendor_label_key`]).
    None,
}

/// The row that signs the default home in, from Grok Build's renewal-bound roster
/// ([`super::accounts::Store::read_bound_roster`]: a row a renewal moved off the default home
/// has a `dir` there) and its pool's active label: the row with no `dir`, else the active row,
/// else [`LEDGER_ACCOUNT`] with no email.
#[must_use]
pub fn pick_row(
    roster: &super::accounts::Roster,
    active: Option<&str>,
) -> (String, RowFrom, Option<String>) {
    if let Some(row) = roster.row_for_login(None) {
        return (row.label.clone(), RowFrom::Default, row.email.clone());
    }
    if let Some(row) = active.and_then(|label| roster.row(label)) {
        return (row.label.clone(), RowFrom::Active, row.email.clone());
    }
    (LEDGER_ACCOUNT.to_owned(), RowFrom::None, None)
}

/// The headless browser the automatic rung drives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Browser {
    /// One is installed ([`super::headless_oauth::find_headless_browser`]).
    Found,
    /// None was, and the pinned one was fetched for this sign-in ([`super::headless_fetch`]).
    Fetched,
    /// None is, and why (bounded words, never a URL).
    Missing(String),
}

/// The browser rung: `found` one installed; else, where the row's route waits on a browser
/// alone (`wanted`: it has an email and, for it, a profile or a mailbox), the pinned one
/// `fetch`ed; else none, saying why. A fetch that failed or was stopped installed nothing
/// ([`super::headless_fetch::install`]).
pub fn browser_rung(
    found: bool,
    wanted: bool,
    fetch: impl FnOnce() -> Result<PathBuf, String>,
) -> Browser {
    if found {
        return Browser::Found;
    }
    if !wanted {
        return Browser::Missing("none installed".to_owned());
    }
    match fetch() {
        Ok(_) => Browser::Fetched,
        Err(why) => Browser::Missing(format!("none installed; the pinned fetch failed: {why}")),
    }
}

/// The most bytes of a reason one rung's words keep in the record ([`Ladder::words`],
/// [`ledger_line`]), so the whole line fits [`super::accounts::LEDGER_WHY_MAX`].
const RUNG_WHY_MAX: usize = 96;

/// What the automatic rungs found for the row that signs the default home in, read once the lock
/// is held and grok can run, before it starts ([`Auto::ladder`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ladder {
    /// The row's label (or [`LEDGER_ACCOUNT`]).
    pub label: String,
    /// Where the row came from.
    pub from: RowFrom,
    /// The row's email: typed into the page where a mailbox is configured for it, the
    /// mailbox's target, and checked against the account grok says it signed in. Never written
    /// to the ledger.
    pub email: Option<String>,
    /// The row has an email and its browser profile exists (a row with none has no profile a
    /// person signed in: `accounts browser` refuses it).
    pub profile: bool,
    /// The row has an email and a mailbox is configured for it.
    pub mailbox: bool,
    /// The headless browser.
    pub browser: Browser,
    /// A Chromium that opens a window is installed, so `accounts browser` can run
    /// ([`super::browser_window::find_window_browser`]): the setup names it, else the mailbox.
    pub window: bool,
    /// The label the setup adopts the default home under where no row signs it in
    /// ([`adopt_label`]).
    pub adopt: String,
}

impl Ladder {
    /// The ladder of a machine with nothing ready: no row, no profile, no mailbox, no browser.
    #[must_use]
    pub fn bare(why: &str) -> Ladder {
        Ladder {
            label: LEDGER_ACCOUNT.to_owned(),
            from: RowFrom::None,
            email: None,
            profile: false,
            mailbox: false,
            browser: Browser::Missing(why.to_owned()),
            window: false,
            adopt: ADOPT_LABEL.to_owned(),
        }
    }

    /// Whether the automatic rung can run: a headless browser, and the row's email with its
    /// profile or its mailbox. With no email no route can succeed: the page asks for one.
    #[must_use]
    pub fn ready(&self) -> bool {
        matches!(self.browser, Browser::Found | Browser::Fetched)
            && self.email.is_some()
            && (self.profile || self.mailbox)
    }

    /// The once-ever setup the need leads with.
    #[must_use]
    pub fn setup(&self) -> Setup {
        match (&self.from, &self.email) {
            (RowFrom::None, _) => Setup::Adopt(self.adopt.clone()),
            (_, None) => Setup::Email(self.label.clone()),
            (_, Some(_)) if self.window => Setup::Window(self.label.clone()),
            (_, Some(_)) => Setup::Mailbox(self.label.clone()),
        }
    }

    /// What it found, one line in the ladder's order (never the email):
    /// `row(main, the default home's) profile(none) mailbox(none configured)
    /// headless-browser(none installed)`.
    #[must_use]
    pub fn words(&self) -> String {
        format!(
            "row({}, {}{}) profile({}) mailbox({}) headless-browser({})",
            self.label,
            match self.from {
                RowFrom::Default => "the default home's",
                RowFrom::Active => "the pool's active",
                RowFrom::None => "no grok row",
            },
            if self.email.is_none() && self.from != RowFrom::None {
                ", no email"
            } else {
                ""
            },
            if self.profile { "ready" } else { "none" },
            if self.mailbox {
                "ready"
            } else {
                "none configured"
            },
            match &self.browser {
                Browser::Found => "found".to_owned(),
                Browser::Fetched => "fetched the pinned one".to_owned(),
                Browser::Missing(why) => super::one_line(why, RUNG_WHY_MAX),
            }
        )
    }
}

/// The automatic rung of the gate's ladder, ahead of the person: the row that signs the default
/// home in, what this machine offers it, and the headless approver that takes it. A test's
/// stand-in approves or stops as it is told; [`LiveAuto`] is the machine's.
pub trait Auto {
    /// The ladder for this sign-in, read once the lock is held and grok can run; where `fetch`,
    /// it may fetch the pinned headless browser where a route waits on one alone
    /// ([`browser_rung`]), a download that ends once `stopped` says so.
    fn ladder(&mut self, fetch: bool, stopped: &dyn Fn() -> bool) -> Ladder;
    /// Approves `handoff`'s page as the ladder's row, or says why not; called only where
    /// [`Ladder::ready`], with a deadline [`AUTO_WITHIN`] at most away and a `finished` that is
    /// also true once the caller was asked to stop.
    fn approve(&mut self, handoff: &Handoff<'_>) -> Approval;
}

/// The machine's automatic rung: Grok Build's renewal-bound roster and pool ([`pick_row`]),
/// and the headless browser in the row's profile with its mailbox
/// ([`super::headless_oauth::VendorApprover`]) under the machine's sign-in lock.
pub struct LiveAuto {
    /// Grok Build's roster, where a config directory is known.
    pub roster: Option<PathBuf>,
    /// Grok Build's store, where a home is known (its renewals' bindings, its pool's active
    /// row, the machine's sign-in lock, and Claude Code's store beside it that holds every
    /// vendor's browser profiles).
    pub store: Option<super::accounts::Store>,
    /// The owner's home.
    pub home: Option<PathBuf>,
    /// Unix seconds now, for the store's read.
    pub now: i64,
    /// The approver [`Auto::ladder`] made for the row.
    approver: Option<super::headless_oauth::VendorApprover>,
}

/// What [`LiveAuto::row`] read: the row, its approver, and what it offers before any browser.
struct RowRead {
    label: String,
    from: RowFrom,
    email: Option<String>,
    profile: bool,
    mailbox: bool,
    adopt: String,
    approver: super::headless_oauth::VendorApprover,
}

impl LiveAuto {
    /// The machine's rung over `roster`, `store` and `home`.
    #[must_use]
    pub fn new(
        roster: Option<PathBuf>,
        store: Option<super::accounts::Store>,
        home: Option<PathBuf>,
        now: i64,
    ) -> LiveAuto {
        LiveAuto {
            roster,
            store,
            home,
            now,
            approver: None,
        }
    }

    /// The row that signs the default home in and its approver, with no browser looked for or
    /// fetched: the renewal-bound roster read (an unreadable roster or state reads as none: the
    /// rung is skipped, never guessed); the profile [`super::headless_oauth::row_browser_profile`]
    /// names, the one `accounts browser` opens; and the mailbox configured for its email, which
    /// alone lets the approver type that email into x.ai's page (x.ai mails a code nobody would
    /// read otherwise).
    fn row(&self) -> Option<RowRead> {
        let store = self.store.as_ref()?;
        let roster = self
            .roster
            .as_deref()
            .and_then(|path| store.read_bound_roster(path, self.now).ok())
            .unwrap_or_default();
        let active = store
            .load(self.now)
            .ok()
            .and_then(|(state, _)| state.active);
        let (label, from, email) = pick_row(&roster, active.as_deref());
        let mut approver = super::headless_oauth::VendorApprover::new(
            Cli::Grok,
            &label,
            email.as_deref(),
            &store.shared_dir(),
            self.home.as_deref(),
        );
        let profile = email.is_some() && approver.profile_dir(email.as_deref()).is_dir();
        let mailbox = email.is_some() && approver.mailbox.is_some();
        approver.types_email &= mailbox;
        Some(RowRead {
            label,
            from,
            email,
            profile,
            mailbox,
            adopt: adopt_label(&roster),
            approver,
        })
    }
}

impl Auto for LiveAuto {
    fn ladder(&mut self, fetch: bool, stopped: &dyn Fn() -> bool) -> Ladder {
        let Some(read) = self.row() else {
            return Ladder::bare("no home is known");
        };
        let home = self.home.as_deref();
        let browser = browser_rung(
            super::headless_oauth::find_headless_browser(home).is_some(),
            fetch && read.email.is_some() && (read.profile || read.mailbox),
            || {
                let archive = super::headless_fetch::Archive::host()
                    .ok_or_else(|| "the pin covers no build for this machine".to_owned())?;
                let root = super::headless_fetch::cache_root(home)
                    .ok_or_else(|| "no cache directory is known".to_owned())?;
                super::headless_fetch::install(
                    &root,
                    &archive,
                    &mut super::headless_fetch::Curl,
                    stopped,
                )
            },
        );
        self.approver = Some(read.approver);
        Ladder {
            label: read.label,
            from: read.from,
            email: read.email,
            profile: read.profile,
            mailbox: read.mailbox,
            browser,
            window: super::browser_window::find_window_browser(home).is_some(),
            adopt: read.adopt,
        }
    }

    fn approve(&mut self, handoff: &Handoff<'_>) -> Approval {
        let (Some(store), Some(approver)) = (self.store.as_ref(), self.approver.as_mut()) else {
            return Approval::NeedsYou("no row was read for the automatic sign-in".to_owned());
        };
        // One headless sign-in at a time on this machine, for every CLI (a renewal, `accounts
        // browser`, the upkeep): the row's profile admits one browser.
        let _lock = loop {
            match store.signin_now() {
                Ok(Some(lock)) => break lock,
                Ok(None) => {}
                Err(e) => {
                    return Approval::NeedsYou(format!(
                        "the machine's sign-in lock could not be taken ({e})"
                    ));
                }
            }
            if (handoff.finished)() {
                return Approval::Approved { page: false };
            }
            if Instant::now() >= handoff.deadline {
                return Approval::NeedsYou(format!(
                    "{} for the automatic rung's time",
                    super::accounts::SIGNIN_HELD
                ));
            }
            std::thread::sleep(POLL);
        };
        approver.approve(handoff)
    }
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

/// Signs this machine in to Grok Build once ([`Gate`]): the page approved by `auto` where its
/// ladder is ready, else the one need through `owner`; the end's ledger line through `ledger`
/// ([`ledger_line`], never the page, the code or the email). A person's own launch after a
/// sign-in the automatic rung refused ([`Record::by`]) skips that rung once and asks them, so a
/// profile that keeps refusing never leaves the machine with no way in.
pub fn run(
    gate: &Gate<'_>,
    auto: &mut dyn Auto,
    owner: &mut dyn Owner,
    ledger: &mut dyn FnMut(&str),
) -> Ended {
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
    // After the automatic rung's refusal, a person's own launch asks the person: the rung skipped
    // once, its browser neither looked for nor fetched.
    let skip = gate.ack && last.by && last.outcome == "refused";
    let led = lead(gate, &target, auto, owner, skip);
    drop(leading);
    owner.clear();
    let (word, host) = match &led.lead {
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
            by: led.refused_by_rung,
        },
    );
    ledger(&ledger_line(host.as_deref(), &word, &led.rungs));
    match led.lead {
        Lead::Drove(Drove {
            outcome: Outcome::SignedIn { email, .. },
            ..
        }) => Ended::SignedIn {
            file: target,
            email,
        },
        Lead::Drove(drove) if led.refused_by_rung => Ended::Unsigned {
            why: format!(
                "{}; nothing was placed, and the next launch from a terminal asks you to \
                 approve its code yourself (or sign {}'s browser profile in again as its \
                 account: `aterm harness accounts browser {} --cli grok`)",
                unsigned_why(&drove.outcome),
                led.label,
                led.label
            ),
            word,
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

/// The accounts ledger's line for a sign-in's end: `<host> device <outcome>; <rungs>` (the
/// ladder's words, then `headless(…) person(…)`), within [`super::accounts::LEDGER_WHY_MAX`],
/// which a [`super::accounts::Kind::SignIn`] row keeps whole. Each rung's reason is bounded
/// ([`RUNG_WHY_MAX`]), so even at the longest label the person's rung is never the part cut.
#[must_use]
pub fn ledger_line(host: Option<&str>, word: &str, rungs: &str) -> String {
    super::one_line(
        &format!(
            "{} {} {word}; {rungs}",
            host.unwrap_or("no-page"),
            Flow::Device.as_str()
        ),
        super::accounts::LEDGER_WHY_MAX,
    )
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
                "another session is signing Grok Build in on this machine; waiting for it (aterm \
                 approves its page itself where it can; what it needs from you, if anything, goes \
                 on the menu bar and in that session's terminal)",
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
    /// The caller was stopped (during the browser's fetch, the automatic rung or the need); the
    /// page's host, where grok had named one.
    Stopped(Option<String>),
    /// A login made some other way appeared while the need stood; the page's host.
    Elsewhere(Option<String>, Signed),
    /// It did not run.
    NotRun(String),
}

/// The leader's end with what its rungs did ([`lead`]).
struct Led {
    lead: Lead,
    /// The rungs' results in the ladder's words ([`ledger_line`]).
    rungs: String,
    /// The ladder's row, which the way out of an automatic refusal names.
    label: String,
    /// The automatic rung's refusal ended it ([`Record::by`]).
    refused_by_rung: bool,
}

/// The last rung of the leader's sign-in: it approves nothing. It puts the one need up, the
/// once-ever setup first ([`Setup`]), and waits until grok says it is done (or closes its
/// output), the deadline passes, the caller is stopped, or a login made some other way appears.
/// A caller stopped, or a grok already done, before it is reached raises no need.
struct NeedApprover<'a, 'b> {
    owner: &'a mut dyn Owner,
    gate: &'a Gate<'b>,
    setup: Setup,
    raised: bool,
    stopped: bool,
    elsewhere: Option<Signed>,
}

impl Approver for NeedApprover<'_, '_> {
    fn approve(&mut self, handoff: &Handoff<'_>) -> Approval {
        if (self.gate.stopped)() {
            self.stopped = true;
            return Approval::Refused("stopped".to_owned());
        }
        if (handoff.finished)() {
            return Approval::Approved { page: false };
        }
        if let Some(signed) = signed_in(self.gate.var, self.gate.home) {
            self.elsewhere = Some(signed);
            return Approval::Refused("signed in elsewhere".to_owned());
        }
        self.raised = true;
        self.owner.need(
            &need_words(&self.setup, handoff.url, handoff.code),
            &needs_you_line(&self.setup, handoff.url, handoff.code),
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

/// The leader's approver, the gate's ladder: the automatic rung ([`Auto::approve`]) where
/// [`Ladder::ready`], for at most [`AUTO_WITHIN`], then the person ([`NeedApprover`]) only where
/// it stopped for one (`needs-sign-in`, `needs-you`) or no route was ready. Its refusal ends
/// the sign-in: it never falls through to approving anything else.
struct LadderApprover<'a, 'b> {
    auto: &'a mut dyn Auto,
    ready: bool,
    need: NeedApprover<'a, 'b>,
    /// The automatic rung's result, in the ladder's words.
    headless: Option<String>,
    /// The automatic rung was asked (it may have typed, filled or pressed on the page, whatever
    /// it then said): grok's report of another account than the row's is then refused.
    ran: bool,
    /// The automatic rung refused the page.
    refused: bool,
}

impl Approver for LadderApprover<'_, '_> {
    fn approve(&mut self, handoff: &Handoff<'_>) -> Approval {
        let gate = self.need.gate;
        if self.ready && !(gate.stopped)() {
            // A stop ends the automatic rung as grok's own end does; it is told apart below.
            let finished = || (handoff.finished)() || (gate.stopped)();
            let within = Handoff {
                url: handoff.url,
                code: handoff.code,
                completion: handoff.completion,
                hosts: handoff.hosts,
                email: handoff.email,
                finished: &finished,
                deadline: handoff.deadline.min(Instant::now() + AUTO_WITHIN),
            };
            self.ran = true;
            let said = self.auto.approve(&within);
            if (gate.stopped)() {
                self.need.stopped = true;
                self.headless = Some("stopped".to_owned());
                return Approval::Refused("stopped".to_owned());
            }
            match said {
                Approval::Approved { page } => {
                    self.headless = Some("approved".to_owned());
                    return Approval::Approved { page };
                }
                Approval::Refused(said) => {
                    self.refused = true;
                    self.headless = Some(headless_words("refused", &said));
                    return Approval::Refused(said);
                }
                Approval::NeedsSignIn(said) => {
                    self.headless = Some(headless_words("needs sign-in", &said));
                }
                Approval::NeedsYou(said) => {
                    self.headless = Some(headless_words("needs you", &said));
                }
            }
        }
        self.need.approve(handoff)
    }
}

/// The automatic rung's result in the record: `<what>: <why>`, the reason bounded
/// ([`RUNG_WHY_MAX`]).
fn headless_words(what: &str, why: &str) -> String {
    format!("{what}: {}", super::one_line(why, RUNG_WHY_MAX))
}

/// The rungs' part of the ledger's line ([`ledger_line`]): the ladder's words, then the
/// automatic rung's result and whether the person was asked.
fn rungs_line(ladder: &Ladder, headless: &str, asked: bool) -> String {
    format!(
        "{} headless({headless}) person({})",
        ladder.words(),
        if asked { "asked" } else { "not asked" }
    )
}

/// What a sign-in needs before grok runs ([`preflight`]): grok's device route, its sandbox,
/// its program on a private `PATH`, and a private staging home.
struct Prepared {
    route: &'static crate::ai::connector::Route,
    grammar: &'static crate::ai::connector::Grammar,
    completion: crate::ai::connector::Completion,
    hosts: &'static [&'static str],
    sandbox: Option<PathBuf>,
    program: PathBuf,
    path: OsString,
    /// The opener-free `PATH`'s directory, removed when it drops.
    _opener_free: Option<aterm_tempfile::TempDir>,
    staging: aterm_tempfile::TempDir,
}

/// Everything a sign-in needs before grok runs, or why nothing can run: a missing or refusing
/// sandbox, grok not installed, no private `PATH` or staging home. Nothing of the ladder is read
/// (nor a browser fetched) for a sign-in that cannot run.
fn preflight(gate: &Gate<'_>) -> Result<Prepared, String> {
    let Some(route) = GROK_SIGNIN.route(Some(Flow::Device)) else {
        return Err("this build has no Grok Build device sign-in".to_owned());
    };
    let Drive::Lines {
        grammar,
        completion,
        ..
    } = &route.drive
    else {
        return Err("Grok Build's device sign-in is not a line route".to_owned());
    };
    let Some(hosts) = route.hosts.of(None) else {
        return Err("Grok Build's device sign-in names no hosts".to_owned());
    };
    let sandbox = if GROK_SIGNIN.no_desk.contains(&std::env::consts::OS) {
        let Some(sandbox) = gate.sandbox.filter(|s| s.is_file()) else {
            return Err(format!(
                "Grok Build opens this machine's default browser through the OS, and the sandbox \
                 that stops it ({}) is not here; nothing was run",
                gate.sandbox
                    .unwrap_or(Path::new(login_drive::SANDBOX_EXEC))
                    .display()
            ));
        };
        if let Some(why) = login_drive::sandbox_refusal(sandbox) {
            return Err(format!(
                "Grok Build opens this machine's default browser through the OS, and the sandbox \
                 that stops it cannot run here: {why}; nothing was run"
            ));
        }
        Some(sandbox.to_path_buf())
    } else {
        None
    };
    let (program, path, opener_free) = login_drive::child_program(
        gate.program.0,
        None,
        GROK_SIGNIN.no_opener,
        gate.state,
        gate.var,
    )
    .map_err(|e| format!("no private PATH could be made: {e}"))?;
    if !program.is_file() {
        return Err(format!(
            "`{}` is not installed here (`aterm pkg install grok` installs it)",
            program.display()
        ));
    }
    let staging = aterm_tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(gate.state)
        .map_err(|e| format!("no staging home could be made: {e}"))?;
    Ok(Prepared {
        route,
        grammar,
        completion: *completion,
        hosts,
        sandbox,
        program,
        path,
        _opener_free: opener_free,
        staging,
    })
}

/// The leader's sign-in: once [`preflight`] says it can run, the ladder is read
/// ([`Auto::ladder`], told to the person; with `skip`, after the automatic rung's refusal, no
/// browser is fetched and the rung is skipped), and a stop heard meanwhile ends it before grok
/// starts; then grok's device route runs in its private staging home under the no-desk
/// sandbox, its page handed to the ladder (`auto` as the ladder's row, then the need), its login
/// placed at `target`; with the rungs' results in the ladder's words.
fn lead(
    gate: &Gate<'_>,
    target: &Path,
    auto: &mut dyn Auto,
    owner: &mut dyn Owner,
    skip: bool,
) -> Led {
    let prepared = match preflight(gate) {
        Ok(prepared) => prepared,
        Err(why) => {
            return Led {
                lead: Lead::NotRun(why),
                rungs: String::new(),
                label: String::new(),
                refused_by_rung: false,
            };
        }
    };
    let ladder = auto.ladder(!skip, gate.stopped);
    let ready = ladder.ready() && !skip;
    let skipped = || {
        if skip {
            "skipped: the last automatic try was refused"
        } else if ladder.ready() {
            "not reached"
        } else {
            "not ready"
        }
    };
    if (gate.stopped)() {
        return Led {
            lead: Lead::Stopped(None),
            rungs: rungs_line(&ladder, skipped(), false),
            label: ladder.label.clone(),
            refused_by_rung: false,
        };
    }
    owner.say(&format!(
        "Grok Build sign-in: {}; {}",
        ladder.words(),
        if ready {
            "aterm approves the page itself first"
        } else if skip {
            "the last automatic try was refused, so you will be asked"
        } else {
            "no automatic route is ready, so you will be asked"
        }
    ));
    let mut approver = LadderApprover {
        auto,
        ready,
        need: NeedApprover {
            owner,
            gate,
            setup: ladder.setup(),
            raised: false,
            stopped: false,
            elsewhere: None,
        },
        headless: None,
        ran: false,
        refused: false,
    };
    let ended = lead_with(gate, target, &ladder, &prepared, &mut approver);
    let signed = matches!(
        &ended,
        Lead::Drove(Drove {
            outcome: Outcome::SignedIn { .. },
            ..
        })
    );
    let headless = match approver.headless.as_deref() {
        // The rung says approved once grok said done or closed its output; one that ended
        // without a login was not approved by it.
        Some("approved") if !signed => "grok ended while it ran".to_owned(),
        Some(said) => said.to_owned(),
        None => skipped().to_owned(),
    };
    let refused_by_rung = approver.refused
        || (approver.ran
            && matches!(
                &ended,
                Lead::Drove(Drove {
                    outcome: Outcome::Refused(_),
                    ..
                })
            ));
    Led {
        rungs: rungs_line(&ladder, &headless, approver.need.raised),
        label: ladder.label.clone(),
        refused_by_rung,
        lead: ended,
    }
}

/// [`lead`]'s run of grok, once prepared and its approver made.
fn lead_with(
    gate: &Gate<'_>,
    target: &Path,
    ladder: &Ladder,
    prepared: &Prepared,
    approver: &mut LadderApprover<'_, '_>,
) -> Lead {
    let lead_args = gate.program.1;
    let sandboxed: Vec<OsString>;
    let (program, lead_args): (&Path, &[OsString]) = match &prepared.sandbox {
        Some(sandbox) => {
            sandboxed = login_drive::no_desk_argv(&prepared.program, lead_args);
            (sandbox.as_path(), sandboxed.as_slice())
        }
        None => (prepared.program.as_path(), lead_args),
    };
    let staging = prepared.staging.path();
    let env = login_drive::child_env(Cli::Grok, &GROK_SIGNIN, staging, &prepared.path, gate.var);
    let args = login_drive::child_args(prepared.route, None);
    let drove = login_drive::drive(
        &login_drive::Spec {
            program,
            lead: lead_args,
            args: &args,
            env: &env,
            cwd: staging,
            grammar: prepared.grammar,
            completion: prepared.completion,
            hosts: prepared.hosts,
            email: ladder.email.as_deref(),
            hang: gate.hang,
        },
        approver,
    );
    if approver.need.stopped {
        return Lead::Stopped(drove.host);
    }
    if let Some(signed) = approver.need.elsewhere.take() {
        return Lead::Elsewhere(drove.host, signed);
    }
    let Outcome::SignedIn { email: said, .. } = &drove.outcome else {
        return Lead::Drove(drove);
    };
    // Once aterm's rung acted on the page (whatever it then said: it may have pressed confirm
    // and run out of time before the page said so), the login is the row's account only: grok's
    // report of another one is a refusal, and its login is not placed. Where the rung never ran,
    // the approval was a person's own, and is theirs.
    if approver.ran
        && let (Some(row), Some(said)) = (ladder.email.as_deref(), said.as_deref())
        && !super::accounts::email_matches(row, said)
    {
        return Lead::Drove(Drove {
            outcome: Outcome::Refused(format!(
                "grok signed in another account than the one {}'s row names, so its login was \
                 not placed",
                ladder.label
            )),
            host: drove.host,
        });
    }
    match place(&staging.join("auth.json"), target) {
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
            "waiting for the approval until the code expires ({} min from the sign-in's start); \
             aterm opens no page (Ctrl-C stops)",
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
