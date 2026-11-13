// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one gate every read of agy passes (owner rulings 2026-11-09: "if you're not logged in,
//! then you can't return this value, and some other hook should handle this"; "failing auth
//! should update the known local and fleet state of that account").
//!
//! A read (`agy -p /usage`, `agy -p /model`) is a process, and a signed-out agy answers one by
//! opening its Google sign-in page. So no read starts an agy the account state does not already
//! know signed in:
//!
//! - [`Gate::admit`] reads the agy account's `auth` in [`accounts::Store`] before anything runs.
//!   Only a login on record as working ([`Auth::Ok`]), or one nothing is on record of whose
//!   grant file is here ([`Proof::Grant`], agy's file backend), is given a [`Pass`]; every other
//!   answer is "not signed in", at once and with no process.
//! - The account is the one whose login the read's home is ([`seat`]): a pool row's directory,
//!   or the person's own home (the row with no directory, or this machine's own login where no
//!   roster lists the pool). A home no account answers to (a direct `harness agy login`
//!   profile) passes only on its grant file, and a prompt it meets has no account to be
//!   recorded on: the quota store's backoff holds it, as before.
//! - [`Gate::read`] is the only code that starts agy for a read. It takes the [`Pass`], runs agy
//!   where its own browser open fails ([`off_screen`]), and where agy still asks for a sign-in (a
//!   grant that ended since it was proven) records [`Auth::SignedOut`] through the store's
//!   transaction, so the ledger row reaches the journal as `signed-out` and the gate then holds.
//! - [`Gate::served`] is the hook that opens it again: a turn the person's own agy served in a
//!   tab is the evidence, never a probe.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::accounts::{self, Auth, Cli, Event, Logins, Roster, Row, State};
use super::agy_quota_store::{SIGN_IN_PROMPT, SIGNED_OUT};
use super::align::{Captured, Ended};

/// The word a refusal carries where nothing is on record of the login: it is not known to be
/// signed in, and agy is not run to find out.
pub const NOT_KNOWN: &str = "not-signed-in";

/// What the read's own files say of its sign-in, known without running agy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proof {
    /// Nothing local says: agy keeps its grant in the OS keyring, which nothing here may ask,
    /// or another credential or a moved config stands beside it.
    None,
    /// agy's file backend, and its grant file is a usable file.
    Grant,
    /// agy's file backend, and no usable grant file is in this directory.
    NoGrant(PathBuf),
}

impl Proof {
    /// The proof `grant` gives: [`Proof::None`] where the grant file is not the whole answer
    /// (`None`), else whether it is a usable file.
    #[must_use]
    pub fn of_grant(grant: Option<PathBuf>) -> Self {
        match grant {
            None => Self::None,
            Some(path) if super::agy_quota_store::usable_grant(&path) => Self::Grant,
            Some(path) => Self::NoGrant(path.parent().unwrap_or(&path).to_path_buf()),
        }
    }
}

/// Leave to start one agy process for a read at [`Pass::at`]: made only by [`Gate::admit`], and
/// spent by the one process it admits.
#[derive(Debug)]
pub struct Pass {
    at: i64,
    name: String,
    granted: bool,
}

impl Pass {
    /// The time the gate was passed, Unix seconds.
    #[must_use]
    pub const fn at(&self) -> i64 {
        self.at
    }

    /// A pass no gate gave, for a test source that runs no agy.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn fixture(at: i64) -> Self {
        Self {
            at,
            name: String::from("fixture"),
            granted: false,
        }
    }
}

/// Why a read was not admitted. Its words say which, and name the account by its email where a
/// row or the read names one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The login is on record as signed out since `since`.
    SignedOut {
        /// The account's shown name.
        name: String,
        /// When that was recorded, Unix seconds.
        since: Option<i64>,
    },
    /// agy's file backend holds no grant here.
    NoGrant {
        /// The account's shown name.
        name: String,
        /// The directory the grant file is missing from.
        dir: PathBuf,
    },
    /// Nothing is on record of the login, and nothing local proves it signed in.
    NotKnown {
        /// The account's shown name.
        name: String,
    },
    /// No account of the agy pool answers to the home the read would run in.
    Unlisted,
    /// The account records could not be read.
    Unreadable(String),
}

impl Refusal {
    /// Whether the login is known signed out (on record, or by its missing grant file), as
    /// against merely not known signed in.
    #[must_use]
    pub const fn signed_out(&self) -> bool {
        matches!(self, Self::SignedOut { .. } | Self::NoGrant { .. })
    }

    /// The refusal as one line: [`SIGNED_OUT`] leads a login known signed out, [`NOT_KNOWN`] any
    /// other, and each says what signs it in.
    #[must_use]
    pub fn words(&self) -> String {
        const HOW: &str = "sign in to agy in a tab (a turn it serves is noted), or `aterm harness \
                           agy login <account>`";
        match self {
            Self::SignedOut { name, since } => format!(
                "{SIGNED_OUT}: agy's login {name} is on record as signed out{}, so agy is not \
                 run (a read never starts a sign-in); {HOW}",
                since.map_or_else(String::new, |at| format!(
                    " since {}",
                    super::usage::rfc3339_utc(at)
                ))
            ),
            Self::NoGrant { name, dir } => format!(
                "{SIGNED_OUT}: agy's login {name} is signed out on this machine (no usable {} in \
                 {}); {HOW}",
                super::upgrade_agy::GRANT_FILE,
                dir.display()
            ),
            Self::NotKnown { name } => format!(
                "{NOT_KNOWN}: agy's login {name} is not known to be signed in on this machine, \
                 so agy is not run to find out (a read never starts a sign-in); {HOW}"
            ),
            Self::Unlisted => format!(
                "{NOT_KNOWN}: no agy account on record answers to this home, so agy is not run \
                 to find out (a read never starts a sign-in); {HOW}"
            ),
            Self::Unreadable(why) => format!(
                "{NOT_KNOWN}: the agy account records could not be read ({why}), so agy is not \
                 run"
            ),
        }
    }
}

/// How the login a read would run on stands in the account state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// On record as working, or proven by its grant file with nothing on record.
    SignedIn {
        /// The account's shown name: its email where a row names one.
        name: String,
    },
    /// Not admitted, and why.
    Out(Refusal),
}

/// What a home no account answers to is called in a read's words.
const UNLISTED: &str = "in this home";

/// [`Standing`] and whether an account answers to the read's home (so there is one to record on).
type Judged = (Standing, bool);

/// The agy account a read's home answers to, in the account state: the pool's store, the roster
/// beside the config, and the login directory the read runs in (`None`: the person's own home).
#[derive(Debug, Clone)]
pub struct Gate {
    store: accounts::Store,
    roster: Option<PathBuf>,
    login: Option<PathBuf>,
}

/// The account the login directory `login` answers to in `rows` (the effective roster): the row
/// whose directory it is, or for the person's own home the row with none. With no rows at all the
/// person's own home is this machine's own login ([`Roster::own`]), as every pool names it
/// before a roster lists it.
fn seat(rows: &Roster, login: Option<&Path>) -> Option<Row> {
    match login {
        Some(dir) => rows
            .rows
            .iter()
            .find(|row| row.dir.as_deref().is_some_and(|d| Path::new(d) == dir))
            .cloned(),
        None if rows.rows.is_empty() => Roster::own().rows.into_iter().next(),
        None => rows.row_for_login(None).cloned(),
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// Negative control, on this thread: a read's agy runs as any child does, its browser open
    /// free to succeed ([`off_screen`]).
    pub(super) static ON_DESK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Negative control, on this thread: a sign-in prompt a read meets is not recorded
    /// ([`Gate::read`]).
    pub(super) static PROMPT_UNRECORDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, debug_assertions))]
fn on_desk() -> bool {
    ON_DESK.get()
}

/// A release build has no negative control.
#[cfg(not(any(test, debug_assertions)))]
const fn on_desk() -> bool {
    false
}

#[cfg(any(test, debug_assertions))]
fn prompt_unrecorded() -> bool {
    PROMPT_UNRECORDED.get()
}

/// A release build has no negative control.
#[cfg(not(any(test, debug_assertions)))]
const fn prompt_unrecorded() -> bool {
    false
}

/// What keeps a read's agy off the screen for as long as its command lives (a private `PATH`'s
/// directory, removed when it drops).
pub(super) struct OffScreen {
    _held: Option<aterm_tempfile::TempDir>,
}

/// The command that runs `program` with `args` where its own browser open fails, so a read can
/// put nothing on the screen whatever agy decides to do:
///
/// - macOS: under [`super::login_drive::NO_DESK`] (LaunchServices, Apple events, `open`,
///   `osascript` and the launch of an installed app denied), the sandbox vendor renewals run
///   under. `PATH` is left alone: agy still reads its grant from the keyring. Where
///   `sandbox-exec` cannot apply its profile (this process already runs under a sandbox) it
///   exits before its child starts, so agy does not run at all.
/// - Other Unix: an opener-free `PATH` ([`super::login_drive::child_program`]), under the
///   capture's cleared environment (no display, no `BROWSER`).
/// - Elsewhere nothing here keeps a browser closed, so no command is made and agy is not run.
///
/// # Errors
///
/// The private `PATH` cannot be made, or this system has no way to keep agy off the screen.
pub(super) fn off_screen(
    program: &OsStr,
    args: &[OsString],
    scratch: &Path,
) -> Result<(aterm_uds::spawnfd::Command, OffScreen), String> {
    if on_desk() {
        let mut cmd = aterm_uds::spawnfd::Command::new(program);
        cmd.args(args);
        return Ok((cmd, OffScreen { _held: None }));
    }
    #[cfg(target_os = "macos")]
    {
        let _ = scratch;
        let mut cmd = aterm_uds::spawnfd::Command::new(super::login_drive::SANDBOX_EXEC);
        cmd.args(super::login_drive::no_desk_argv(Path::new(program), args));
        Ok((cmd, OffScreen { _held: None }))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let (program, path, held) =
            super::login_drive::child_program(Path::new(program), None, true, scratch, &|name| {
                std::env::var_os(name)
            })
            .map_err(|e| {
                format!("agy's opener-free PATH could not be made ({e}); it is not run")
            })?;
        let mut cmd = aterm_uds::spawnfd::Command::new(program);
        cmd.args(args).env("PATH", path);
        Ok((cmd, OffScreen { _held: held }))
    }
    #[cfg(not(unix))]
    {
        let _ = (program, args, scratch);
        Err(String::from(
            "agy is not run for a read on this system: nothing here keeps its sign-in page off \
             the screen",
        ))
    }
}

impl Gate {
    /// The gate of the agy pool kept under the harness state `state`, its roster beside `config`,
    /// for a read that runs in the login directory `login` (`None`: the person's own home).
    #[must_use]
    pub fn at(state: &Path, user_home: &Path, config: Option<&Path>, login: Option<&Path>) -> Self {
        Self {
            store: accounts::Store::at_for(state, Cli::Agy, user_home),
            roster: config.and_then(|config| accounts::roster_beside_for(config, Cli::Agy)),
            // The person's own home named outright is still the person's own home.
            login: login
                .filter(|login| *login != user_home)
                .map(Path::to_path_buf),
        }
    }

    /// [`Gate::at`] for this machine's harness state, this process's home and the active config;
    /// `None` where no state directory or home is known.
    #[must_use]
    pub fn of_machine(login: Option<&Path>) -> Option<Self> {
        let state = super::agy_quota_store::machine_state()?;
        let home = aterm_types::dirs::home_dir()?;
        Some(Self::at(
            &state,
            &home,
            aterm_types::dirs::aterm_config_path().as_deref(),
            login,
        ))
    }

    /// The pool's account store.
    #[must_use]
    pub const fn store(&self) -> &accounts::Store {
        &self.store
    }

    fn roster_file(&self) -> Option<&Path> {
        self.roster.as_deref().filter(|path| path.is_file())
    }

    /// How the read's login stands, from one read of the account records and `proof`: nothing is
    /// run and nothing is written.
    #[must_use]
    pub fn standing(&self, proof: &Proof, now: i64) -> Standing {
        self.judge(proof, now).0
    }

    fn judge(&self, proof: &Proof, now: i64) -> Judged {
        let records = match self.store.read_account_records(self.roster_file(), now) {
            Ok(records) => records,
            Err(
                accounts::RosterReadError::Roster(why) | accounts::RosterReadError::Records(why),
            ) => return (Standing::Out(Refusal::Unreadable(why)), false),
        };
        let Some(row) = seat(&records.roster, self.login.as_deref()) else {
            // No account to read or to record on: the grant file alone speaks.
            let standing = match proof {
                Proof::Grant => Standing::SignedIn {
                    name: UNLISTED.to_owned(),
                },
                Proof::NoGrant(dir) => Standing::Out(Refusal::NoGrant {
                    name: UNLISTED.to_owned(),
                    dir: dir.clone(),
                }),
                Proof::None => Standing::Out(Refusal::Unlisted),
            };
            return (standing, false);
        };
        let name = row.shown_name().to_owned();
        let account = records.state.accounts.get(&row.label);
        let auth = account.map_or(Auth::Unseen, |account| account.current_auth(&row));
        let standing = match (auth, proof) {
            (Auth::SignedOut, _) => Standing::Out(Refusal::SignedOut {
                name,
                since: account.and_then(|account| account.auth_at),
            }),
            (_, Proof::NoGrant(dir)) => Standing::Out(Refusal::NoGrant {
                name,
                dir: dir.clone(),
            }),
            (Auth::Ok, Proof::None | Proof::Grant) | (Auth::Unseen, Proof::Grant) => {
                Standing::SignedIn { name }
            }
            (Auth::Unseen, Proof::None) => Standing::Out(Refusal::NotKnown { name }),
        };
        (standing, true)
    }

    /// Leave to start one agy process at `now`, or why not. A login on record as working whose
    /// grant file is gone is recorded signed out here: the file is the fact.
    ///
    /// # Errors
    ///
    /// The login is not known signed in ([`Refusal`]).
    pub fn admit(&self, proof: &Proof, now: i64) -> Result<Pass, Refusal> {
        match self.judge(proof, now) {
            (Standing::SignedIn { name }, _) => Ok(Pass {
                at: now,
                name,
                granted: *proof == Proof::Grant,
            }),
            (Standing::Out(refusal), seated) => {
                if seated && matches!(refusal, Refusal::NoGrant { .. }) {
                    // Recorded only where it changes the word on record; a held lock loses
                    // nothing, since the missing file refuses the next read too.
                    let _ = self.signed_out(None, now);
                }
                Err(refusal)
            }
        }
    }

    /// One read of agy under `pass`: `program` with `args` and the pairs `env`, off the screen
    /// ([`off_screen`]), within `budget`, stopped the moment it prints its sign-in prompt
    /// ([`SIGN_IN_PROMPT`]). A prompt is recorded on the account ([`Gate::signed_out`], naming
    /// `email` where the read knows the login's address) before the failure is answered.
    ///
    /// # Errors
    ///
    /// agy could not be kept off the screen or started, ran past `budget`, or asked for a
    /// sign-in (the text then leads with [`SIGNED_OUT`]).
    pub(super) fn read(
        &self,
        pass: Pass,
        program: &OsStr,
        args: &[OsString],
        env: &[(String, String)],
        (budget, max_stdout): (Duration, usize),
        email: Option<&str>,
    ) -> Result<Captured, String> {
        let (mut cmd, _held) = off_screen(program, args, &self.store.dir)?;
        for (name, value) in env {
            cmd.env(name, value);
        }
        match super::align::capture_bounded_until(cmd, budget, max_stdout, SIGN_IN_PROMPT)? {
            Ended::Exited(got) => Ok(got),
            Ended::Stopped => {
                if !prompt_unrecorded() {
                    // A lock held past its ladder, or a home no account answers to, leaves no
                    // record: the caller's backoff holds the next read, which runs off the
                    // screen as this one did.
                    let _ = self.signed_out(email, pass.at);
                }
                Err(if pass.granted {
                    format!(
                        "{SIGNED_OUT} (expired): agy asked for a Google sign-in for its login {} \
                         although its grant file is here, so the grant is expired or revoked; \
                         `aterm harness agy login <account>` signs it in again",
                        pass.name
                    )
                } else {
                    format!(
                        "{SIGNED_OUT}: agy asked for a Google sign-in for its login {} (it holds \
                         no usable grant here); signing in to agy in a tab or `aterm harness agy \
                         login <account>` signs it in",
                        pass.name
                    )
                })
            }
        }
    }

    /// One transaction on the read's account: `edit` gets its row, and the events it returns are
    /// committed with the state (and so journalled, [`super::journal::observe`]). Where a roster
    /// lists the pool the transaction is bound to it and learns its rows' keys; with none the
    /// account is this machine's own login, keyed by `email` where the caller observed its
    /// address, so its events reach the journal too.
    fn record(
        &self,
        ladder: &[Duration],
        now: i64,
        email: Option<&str>,
        edit: impl FnOnce(&Row, &mut State, &mut Logins) -> Vec<Event>,
    ) -> Result<Vec<Event>, String> {
        let login = self.login.as_deref();
        let key = |row: &Row, state: &mut State| {
            let observed = email
                .or(row.expected_identity_email())
                .and_then(|email| super::journal::realm_key("google", email));
            if let (Some(key), Some(account)) = (observed, state.accounts.get_mut(&row.label))
                && account.pk.is_none()
            {
                account.pk = Some(key.as_str().to_owned());
            }
        };
        match self.roster_file() {
            Some(roster) => self.store.transact_pool_logins_within(
                ladder,
                roster,
                now,
                &mut None,
                |raw, state, logins| {
                    let row = seat(&accounts::bound(raw, logins), login)
                        .ok_or("no agy account answers to this home; nothing recorded")?;
                    let events = edit(&row, state, logins);
                    key(&row, state);
                    Ok(events)
                },
            ),
            None => self
                .store
                .transact_logins_within(ladder, now, &mut None, |state, logins| {
                    let row = seat(&Roster::default(), login)
                        .ok_or("no agy account answers to this home; nothing recorded")?;
                    let events = edit(&row, state, logins);
                    key(&row, state);
                    Ok(events)
                }),
        }
    }

    /// Record that the read's login is signed out, as a login wall seen on it is recorded
    /// ([`accounts::note_auth`]): a change of word is a ledger row, which the journal carries as
    /// `signed-out`.
    ///
    /// # Errors
    ///
    /// The store's writer stayed held or the transaction failed; nothing was recorded.
    pub fn signed_out(&self, email: Option<&str>, now: i64) -> Result<Vec<Event>, String> {
        self.record(&accounts::LOCK_LADDER, now, email, |row, state, _| {
            accounts::note_auth(state, &row.label, Auth::SignedOut, now)
        })
    }

    /// The hook that marks the login working from evidence aterm already holds: a turn the
    /// person's own agy served at `at` (its transcript's newest row is the model's answer,
    /// [`super::profile::agy_served_at`]), on the login `email` where its own log names one.
    /// Only a turn served after the word on record changes it: a login on record as working is
    /// left as it is, and one signed out or unseen is recorded signed in
    /// ([`accounts::note_checked_sign_in`]), a `signed-in` on the journal. The store's writer is
    /// tried once: this runs on every footer pass, and the next pass tries again.
    ///
    /// Returns whether the login was newly recorded as working.
    pub fn served(&self, email: Option<&str>, at: i64, now: i64) -> bool {
        let at = at.min(now);
        // The cheap read first: most passes find the turn already on record.
        let Ok(records) = self.store.read_account_records(self.roster_file(), now) else {
            return false;
        };
        let Some(row) = seat(&records.roster, self.login.as_deref()) else {
            return false;
        };
        let news = |state: &State, row: &Row| {
            state.accounts.get(&row.label).is_none_or(|account| {
                account.current_auth(row) != Auth::Ok
                    && account.auth_at.is_none_or(|held| at > held)
            })
        };
        if !news(&records.state, &row) {
            return false;
        }
        self.record(&[], now, email, |row, state, logins| {
            if news(state, row) {
                accounts::note_checked_sign_in(state, logins, &row.label, at)
            } else {
                Vec::new()
            }
        })
        .is_ok_and(|events| !events.is_empty())
    }
}

#[cfg(test)]
#[path = "agy_gate_tests.rs"]
mod tests;
