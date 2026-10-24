// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A pooled CLI's accounts as a launch or a running session of it meets them, for every CLI the
//! connector table pools ([`Cli`]): the login a launch aterm makes starts under
//! ([`launch_env`]), after each of its rows' usage is read in that row's own home
//! ([`record_usage`]), and the advice a running session gets from its own CLI's roster and store,
//! with the relaunch line a move writes ([`session_move`]). Claude Code's sessions keep the
//! host's own flow (its transcripts and `/usage` reads); these read the pool's files (its
//! roster as `logins.json` binds it, [`accounts::bound`]), the session's environment, the usage
//! sample its caller read and, for Codex, its app server's own report of each row's usage.

use std::io;
use std::path::Path;
use std::time::Duration;

use super::accounts::{
    self, Advice, Cli, Mode, Point, Roster, Row, Sample, StayWhy, Store, Supervisor,
};
use super::codex_app_login::Usage;
use super::relaunch::{self, PoolRelaunch};
use crate::ai::connector::Seam;

/// How old a row's usage sample may be before a launch reads it again ([`record_usage`]).
pub const USAGE_FRESH_S: i64 = 300;

/// How long one read of a row's usage is waited for ([`live_usage`]).
pub const USAGE_READ_WAIT: Duration = Duration::from_secs(10);

/// A reader of one login's usage: `cli`'s login in the home `home`, at `now`
/// ([`live_usage`] is the product's; a test passes its own).
pub type ReadUsage<'a> = &'a (dyn Fn(Cli, &Path, i64) -> Option<Usage> + Sync);

/// Where this machine keeps the pools.
#[derive(Debug, Clone, Copy)]
pub struct Seat<'a> {
    /// The folder `aterm.toml` and each CLI's roster are in.
    pub config_dir: &'a Path,
    /// The harness state root the stores are under ([`Store::at_for`]).
    pub state: &'a Path,
    /// The user's home: the CLIs' default homes, and the root of their logins.
    pub home: &'a Path,
    /// Now, unix seconds.
    pub now: i64,
}

impl Seat<'_> {
    /// `cli`'s roster: an empty one where it has no file.
    ///
    /// # Errors
    ///
    /// Why a roster file there does not parse.
    pub fn roster(&self, cli: Cli) -> Result<Roster, String> {
        let path = self.config_dir.join(cli.accounts().roster_file);
        if !path.exists() {
            return Ok(Roster::default());
        }
        Roster::load_for(cli, &path)
    }

    /// `cli`'s store.
    #[must_use]
    pub fn store(&self, cli: Cli) -> Store {
        Store::at_for(self.state, cli, self.home)
    }

    /// `cli`'s roster with each row's login as `logins.json` binds it ([`accounts::bound`]): a
    /// row `accounts renew --cli` signed in to a staging home names that home, as `list`, `use`
    /// and the Account menu read it.
    ///
    /// # Errors
    ///
    /// Why the roster or the bindings do not read.
    pub fn bound_roster(&self, cli: Cli) -> Result<Roster, String> {
        let roster = self.roster(cli)?;
        let (logins, _) = self
            .store(cli)
            .load_logins(self.now)
            .map_err(|e| format!("{e:?}"))?;
        Ok(accounts::bound(&roster, &logins))
    }

    /// `[harness] accounts =` in the `aterm.toml` beside the rosters: the one rotation switch of
    /// every CLI whose roster this machine keeps.
    #[must_use]
    pub fn mode(&self) -> Mode {
        crate::supervise::config::SupervisorConfig::from_path(Some(
            &self.config_dir.join("aterm.toml"),
        ))
        .0
        .accounts
    }

    /// Whether `cli` keeps its login where no row's own login file is read (Codex's
    /// `cli_auth_credentials_store` in its default home's `config.toml`, which every overlay
    /// links: [`crate::ai::facts::codex_keyring`]).
    #[must_use]
    pub fn keyring_held(&self, cli: Cli) -> bool {
        cli == Cli::Codex
            && crate::ai::facts::codex_keyring(&self.home.join(cli.accounts().default_home))
    }
}

/// What a launch of a pooled CLI under one row sets in the program's environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginEnv {
    /// The row's label.
    pub label: String,
    /// The seam's variable.
    pub var: &'static str,
    /// Its value, or `None` for the machine's default login, whose launch unsets it.
    pub value: Option<String>,
}

/// [`accounts::login_env_for`] for `row`, once `cli`'s overlay home for it links the shared
/// entries to the CLI's default home under `home` ([`super::accounts_overlay::ensure`]), where
/// its seam is an overlay.
///
/// # Errors
///
/// Why the overlay home could not be made, or that it holds a shared entry of its own
/// ([`super::accounts_overlay::Drift`]: a session there would run apart from the default home's
/// sessions, history and databases), which is left as it is.
pub fn login_env(cli: Cli, row: &Row, home: &Path) -> io::Result<LoginEnv> {
    let seam = cli.accounts().seam;
    if let (Seam::Overlay { .. }, Some(dir)) = (seam, row.dir.as_deref().filter(|d| !d.is_empty()))
    {
        let ensured = super::accounts_overlay::ensure(
            seam,
            Path::new(dir),
            &home.join(cli.accounts().default_home),
        )?;
        if let Some(drift) = ensured.drift.first() {
            return Err(io::Error::other(format!(
                "{dir} holds its own {} ({} shared entr{} not linked to the default home), so \
                 nothing starts there",
                drift.name,
                ensured.drift.len(),
                if ensured.drift.len() == 1 { "y" } else { "ies" }
            )));
        }
    }
    let (var, value) = accounts::login_env_for(cli, row);
    Ok(LoginEnv {
        label: row.label.clone(),
        var,
        value,
    })
}

/// The home `row`'s login of `cli` runs in: its own directory (an overlay made ready by
/// [`login_env`]), else the CLI's default home under `home`; `None` where its overlay cannot be
/// used, or the seam is no home of its own.
fn row_home(cli: Cli, row: &Row, home: &Path) -> Option<std::path::PathBuf> {
    match row.dir.as_deref().filter(|d| !d.is_empty()) {
        None => Some(home.join(cli.accounts().default_home)),
        Some(dir) if matches!(cli.accounts().seam, Seam::Overlay { .. }) => {
            login_env(cli, row, home).ok()?;
            Some(std::path::PathBuf::from(dir))
        }
        Some(_) => None,
    }
}

/// Reads the usage of each of `roster`'s rows of `cli` whose login is here (`enrolled`, or the
/// default row) and whose sample is older than [`USAGE_FRESH_S`], each in its own home and at
/// once, with `read`; records each reading under its row in `cli`'s store (its sample, and the
/// wall it names) in one transaction. The labels recorded.
pub fn record_usage(
    seat: &Seat<'_>,
    cli: Cli,
    roster: &Roster,
    enrolled: &std::collections::BTreeSet<String>,
    read: ReadUsage<'_>,
) -> Vec<String> {
    let store = seat.store(cli);
    let Ok((state, _)) = store.load(seat.now) else {
        return Vec::new();
    };
    let due: Vec<(&Row, std::path::PathBuf)> = roster
        .rows
        .iter()
        .filter(|row| row.dir.is_none() || enrolled.contains(&row.label))
        .filter(|row| {
            state
                .accounts
                .get(&row.label)
                .and_then(|a| a.sample.as_ref())
                .is_none_or(|s| seat.now.saturating_sub(s.at) >= USAGE_FRESH_S)
        })
        .filter_map(|row| Some((row, row_home(cli, row, seat.home)?)))
        .collect();
    if due.is_empty() {
        return Vec::new();
    }
    let readings: Vec<(String, Usage)> = std::thread::scope(|scope| {
        let reads: Vec<_> = due
            .iter()
            .map(|(row, home)| {
                let label = row.label.clone();
                scope.spawn(move || read(cli, home, seat.now).map(|usage| (label, usage)))
            })
            .collect();
        reads
            .into_iter()
            .filter_map(|r| r.join().ok().flatten())
            .collect()
    });
    if readings.is_empty() {
        return Vec::new();
    }
    let mut note = None;
    let recorded = store.transact_within(&accounts::LOCK_LADDER, seat.now, &mut note, |state| {
        let mut events = Vec::new();
        for (label, usage) in &readings {
            events.extend(accounts::note_sample(
                state,
                label,
                usage.sample.clone(),
                seat.now,
            ));
            if let Some((window, until)) = usage.wall {
                events.extend(accounts::note_wall(
                    state, label, window, until, seat.now, seat.now,
                ));
            }
        }
        Ok(events)
    });
    match recorded {
        Ok(_) => readings.into_iter().map(|(label, _)| label).collect(),
        Err(_) => Vec::new(),
    }
}

/// The product's [`ReadUsage`]: Codex's app server started in `home`, asked
/// `account/rateLimits/read` within [`USAGE_READ_WAIT`], then ended
/// ([`super::codex_app_login::run_app_server`], [`super::codex_app_login::read_usage`]); `None`
/// for any other CLI, which has no usage report aterm reads.
#[must_use]
pub fn live_usage(cli: Cli, home: &Path, now: i64) -> Option<Usage> {
    super::codex_app_login::run_app_server(cli, home, USAGE_READ_WAIT, |server, deadline| {
        super::codex_app_login::read_usage(server, deadline, now)
    })
    .flatten()
}

/// Whether `roster` holds two rows whose logins are here: one row is nothing to pick between.
fn pooled(roster: &Roster, enrolled: &std::collections::BTreeSet<String>) -> bool {
    let here = |row: &&Row| row.dir.is_none() || enrolled.contains(&row.label);
    roster.rows.iter().filter(here).count() >= 2
}

/// The login a launch aterm makes of `cli` starts under ([`accounts::launch_row`] over the bound
/// roster, once [`record_usage`] read each row's usage with `read`): `None` with fewer than two
/// rows here, with no row usable, for a CLI whose seam no lab pass measured
/// ([`crate::ai::connector::Accounts::measured`]), where the CLI keeps its login in the OS
/// keyring ([`Seat::keyring_held`]), or where the pick's overlay home cannot be used
/// ([`login_env`]). Under `[harness] accounts = "off"` only the owner's pin is honoured.
#[must_use]
pub fn launch_env(seat: &Seat<'_>, cli: Cli, read: ReadUsage<'_>) -> Option<LoginEnv> {
    if !cli.accounts().measured || seat.keyring_held(cli) {
        return None;
    }
    let roster = seat.bound_roster(cli).ok()?;
    let enrolled = accounts::enrolled_for(cli, &roster);
    if !pooled(&roster, &enrolled) {
        return None;
    }
    let store = seat.store(cli);
    let row = if seat.mode() == Mode::Off {
        let (state, _) = store.load(seat.now).ok()?;
        let pinned = state
            .active
            .as_deref()
            .filter(|_| state.pinned)
            .and_then(|label| roster.row(label))?;
        let cond = accounts::condition(
            pinned,
            &state,
            pinned.dir.is_none() || enrolled.contains(&pinned.label),
            seat.now,
        );
        if matches!(
            cond,
            accounts::Condition::NotEnrolled | accounts::Condition::SignedOut
        ) {
            return None;
        }
        pinned
    } else {
        record_usage(seat, cli, &roster, &enrolled, read);
        let (state, _) = store.load(seat.now).ok()?;
        accounts::launch_row(&roster, &state, &enrolled, seat.now)?
    };
    login_env(cli, row, seat.home).ok()
}

/// A running session of a pooled CLI, as [`session_move`] asks about it.
#[derive(Debug, Clone)]
pub struct Session<'a> {
    /// Its CLI.
    pub cli: Cli,
    /// Its tab's session id (the canary a rotation names).
    pub sid: &'a str,
    /// Who answers its walls.
    pub supervisor: Supervisor,
    /// Its process's environment, as `NAME`/value pairs.
    pub env: &'a [(&'a str, &'a str)],
    /// The usage its CLI's own records said last (codex's rollout `rate_limits`), where read.
    pub sample: Option<Sample>,
    /// Where it is.
    pub point: Point,
    /// `accounts =` in `aterm.toml`.
    pub mode: Mode,
    /// How its relaunch line is spelled.
    pub relaunch: PoolRelaunch<'a>,
}

/// What [`session_move`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    /// The advice.
    pub advice: Advice,
    /// The relaunch line a move writes; `None` for every other advice.
    pub line: Option<String>,
}

impl Moved {
    fn stay(why: StayWhy) -> Moved {
        Moved {
            advice: Advice::Stay(why),
            line: None,
        }
    }
}

/// The advice for session `s` from its own CLI's roster (as `logins.json` binds it) and store
/// ([`accounts::advise_session`]), and the relaunch line a move writes ([`relaunch::pool_line`]).
/// The usage sample and a wall the session stands at are recorded in the store under every
/// mode, `off` included, before the advice is read, and a turn end proves the session's row
/// where it is the rotation's unproven `active` ([`accounts::prove`]). A move's line is written
/// before anything is committed ([`StayWhy::NoLine`] where it cannot be); a move that rotates
/// then commits its rotation ([`accounts::begin_rotation`]), so the breaker counts it in that
/// CLI's store alone. A CLI that keeps its login in the OS keyring stays
/// ([`StayWhy::KeyringStore`]).
#[must_use]
pub fn session_move(seat: &Seat<'_>, s: &Session<'_>) -> Moved {
    if s.supervisor == Supervisor::Own {
        return Moved::stay(StayWhy::OtherSupervisor);
    }
    let Ok(roster) = seat.bound_roster(s.cli) else {
        return Moved::stay(StayWhy::NoRoster);
    };
    let store = seat.store(s.cli);
    let who = accounts::ProcessLogin::of_env_for(s.cli, s.env.iter().copied());
    let me = who.me(&roster);
    if let Some(label) = me.label().filter(|_| me.default_home && me.stored_login) {
        let mut note = None;
        let recorded =
            store.transact_within(&accounts::LOCK_LADDER, seat.now, &mut note, |state| {
                let mut events = Vec::new();
                if s.point == Point::TurnEnd {
                    // A served turn on the rotation's new row proves it, as Claude Code's
                    // host's `account_served` does, so the canary it held does not hold
                    // the pool's early moves for good.
                    events.extend(accounts::prove(state, label, seat.now));
                    events.extend(accounts::note_session(state, s.sid, label, None, seat.now));
                }
                if let Some(sample) = s.sample.clone() {
                    events.extend(accounts::note_sample(state, label, sample, seat.now));
                }
                if let Point::Wall { window, at } = &s.point {
                    events.extend(accounts::note_wall(
                        state,
                        label,
                        window,
                        None,
                        at.unwrap_or(seat.now),
                        seat.now,
                    ));
                }
                Ok(events)
            });
        if recorded.is_err() {
            return Moved::stay(StayWhy::NothingDue);
        }
    }
    if seat.keyring_held(s.cli) {
        return Moved::stay(StayWhy::KeyringStore);
    }
    let Ok((state, _)) = store.load(seat.now) else {
        return Moved::stay(StayWhy::NothingDue);
    };
    let enrolled = accounts::enrolled_for(s.cli, &roster);
    let advice = accounts::advise_session(
        s.cli,
        s.supervisor,
        &roster,
        &state,
        &enrolled,
        &me,
        &s.point,
        s.mode,
        seat.now,
    );
    let Advice::Move { to, rotates, .. } = &advice else {
        return Moved { advice, line: None };
    };
    let Some(row) = roster.row(to) else {
        return Moved::stay(StayWhy::NoCandidate);
    };
    let Some(line) = login_env(s.cli, row, seat.home)
        .ok()
        .and_then(|env| relaunch::pool_line(&s.relaunch, s.cli, &env).ok())
    else {
        return Moved::stay(StayWhy::NoLine);
    };
    if *rotates {
        let Some(from) = me.label() else {
            return Moved::stay(StayWhy::NotListed);
        };
        let expect = state.active.clone();
        let mut note = None;
        let committed =
            store.transact_within(&accounts::LOCK_LADDER, seat.now, &mut note, |held| {
                let mover = accounts::Mover {
                    sid: s.sid,
                    account: from,
                };
                accounts::begin_rotation(
                    held,
                    &roster,
                    &enrolled,
                    expect.as_deref(),
                    mover,
                    to,
                    seat.now,
                )
                .map(|event| vec![event])
                .map_err(|lost| format!("{lost:?}"))
            });
        if committed.is_err() {
            return Moved::stay(StayWhy::NothingDue);
        }
    }
    Moved {
        advice,
        line: Some(line),
    }
}

#[cfg(test)]
#[path = "accounts_pool_tests.rs"]
mod tests;
