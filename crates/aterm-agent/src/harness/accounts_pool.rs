// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A pooled CLI's accounts as a launch or a running session of it meets them, for every CLI the
//! connector table pools ([`Cli`]): the login a launch aterm makes starts under
//! ([`launch_env`]), after each of its rows' usage is read in that row's own home
//! ([`record_usage`]), and the advice a running session gets from its own CLI's roster and store,
//! with the relaunch line a move writes ([`session_move`]). Claude Code's sessions keep the
//! host's own flow (its transcripts and `/usage` reads); these read the pool's files (its
//! roster as `logins.json` binds it, [`accounts::bound`]), the session's environment, the usage
//! sample its caller read and, for Codex, OpenAI's `/wham/usage` endpoint for each row's login.

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
    /// This observes policy and bindings without loading unrelated budget state. An
    /// interrupted commit is still recovered as a whole before the observation.
    ///
    /// # Errors
    ///
    /// Why the roster or bindings cannot be read, the bounded account writer lock cannot be
    /// taken, or an interrupted commit (including its budget image) cannot be recovered.
    pub fn bound_roster(&self, cli: Cli) -> Result<Roster, String> {
        pool_read_using(
            self,
            cli,
            || self.roster(cli),
            std::convert::identity,
            |store, held| policy_held(self, cli, store, held).map(|(roster, _)| roster),
        )
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
    /// `cli_auth_credentials_store` in its default home's `config.toml`, which every row's launch
    /// reads directly: [`crate::ai::facts::codex_keyring`]).
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

/// The directory `row`'s login of `cli` lives in: its own directory (an overlay made ready by
/// [`login_env`], or a login directory/file seam's folder), else the CLI's default home under
/// `home`; `None` where its overlay cannot be used.
fn row_home(cli: Cli, row: &Row, home: &Path) -> Option<std::path::PathBuf> {
    match row.dir.as_deref().filter(|d| !d.is_empty()) {
        None => Some(home.join(cli.accounts().default_home)),
        Some(dir) if matches!(cli.accounts().seam, Seam::Overlay { .. }) => {
            login_env(cli, row, home).ok()?;
            Some(std::path::PathBuf::from(dir))
        }
        Some(dir) => Some(std::path::PathBuf::from(dir)),
    }
}

/// A managed pool's policy, bindings and budget state captured under the account writer lock.
struct PoolSnapshot {
    roster: Roster,
    logins: accounts::Logins,
    state: accounts::State,
}

impl PoolSnapshot {
    fn require_policy(&self, raw: &Roster, logins: &accounts::Logins) -> Result<(), String> {
        if *logins != self.logins || accounts::bound(raw, logins) != self.roster {
            return Err("account policy or login changed before commit; nothing recorded".into());
        }
        Ok(())
    }
}

/// Fingerprint the endpoint and the managed login generation, not the credentials. Tagged,
/// length-delimited fields distinguish absent and empty values, even on non-UTF-8 homes.
/// Serving a turn does not change this key; renewing even in the same directory does.
fn sample_context(
    seat: &Seat<'_>,
    cli: Cli,
    row: &Row,
    logins: &accounts::Logins,
) -> accounts::SampleContext {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-pool-sample-context-v2");
    let mut field = |value: Option<&[u8]>| match value {
        None => hash.update([0]),
        Some(value) => {
            hash.update([1]);
            hash.update((value.len() as u64).to_be_bytes());
            hash.update(value);
        }
    };
    field(Some(cli.id().as_bytes()));
    field(Some(seat.home.as_os_str().as_encoded_bytes()));
    field(Some(row.label.as_bytes()));
    field(row.dir.as_deref().map(str::as_bytes));
    field(row.email.as_deref().map(str::as_bytes));
    field(row.identity_email.as_deref().map(str::as_bytes));
    field(row.vendor.region.map(|region| region.as_str().as_bytes()));
    field(row.flow(cli).map(|flow| flow.as_str().as_bytes()));
    let binding = logins.labels.get(&row.label);
    field(
        binding
            .and_then(|binding| binding.dir.as_deref())
            .map(str::as_bytes),
    );
    field(
        binding
            .and_then(|binding| binding.renewed_at)
            .map(|at| at.to_be_bytes())
            .as_ref()
            .map(|bytes| bytes.as_slice()),
    );
    field(
        binding
            .map(|binding| binding.renewal_generation.to_be_bytes())
            .as_ref()
            .map(|bytes| bytes.as_slice()),
    );
    field(
        binding
            .and_then(|binding| binding.proved_by)
            .map(|proof| proof.as_str().as_bytes()),
    );
    field(
        binding
            .and_then(|binding| binding.proved_email.as_deref())
            .map(str::as_bytes),
    );
    field(
        binding
            .and_then(|binding| binding.proved_dir.as_deref())
            .map(str::as_bytes),
    );
    accounts::SampleContext(aterm_codec::hex::encode_lower(&hash.finalize()))
}

fn policy_held(
    seat: &Seat<'_>,
    cli: Cli,
    store: &Store,
    held: &accounts::Lock,
) -> Result<(Roster, accounts::Logins), String> {
    let path = seat.config_dir.join(cli.accounts().roster_file);
    let (target, _) = super::cli::roster_target(&path)?;
    store.recover_sync_held(held, &target)?;
    let raw = seat.roster(cli)?;
    let (logins, _) = store.load_logins(seat.now)?;
    Ok((accounts::bound(&raw, &logins), logins))
}

fn snapshot_held(
    seat: &Seat<'_>,
    cli: Cli,
    store: &Store,
    held: &accounts::Lock,
) -> Result<PoolSnapshot, String> {
    let (roster, logins) = policy_held(seat, cli, store, held)?;
    let (state, _) = store.load(seat.now)?;
    Ok(PoolSnapshot {
        roster,
        logins,
        state,
    })
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolRecovery",
        action = "Observe",
        project = "aterm_agent::harness::accounts_pool::tests::recovery::recovery_transition"
    )
)]
fn pool_snapshot(seat: &Seat<'_>, cli: Cli) -> Result<PoolSnapshot, String> {
    pool_snapshot_using(seat, cli, || seat.roster(cli))
}

fn pool_snapshot_using(
    seat: &Seat<'_>,
    cli: Cli,
    read_roster: impl FnOnce() -> Result<Roster, String>,
) -> Result<PoolSnapshot, String> {
    pool_read_using(
        seat,
        cli,
        read_roster,
        |roster| PoolSnapshot {
            roster,
            logins: accounts::Logins::default(),
            state: accounts::State::default(),
        },
        |store, held| snapshot_held(seat, cli, store, held),
    )
}

fn pool_read_using<T>(
    seat: &Seat<'_>,
    cli: Cli,
    read_roster: impl FnOnce() -> Result<Roster, String>,
    from_empty: impl FnOnce(Roster) -> T,
    read_held: impl FnOnce(&Store, &accounts::Lock) -> Result<T, String>,
) -> Result<T, String> {
    let store = seat.store(cli);
    let path = seat.config_dir.join(cli.accounts().roster_file);
    store.recover_sync_within(&accounts::LOCK_LADDER, &path)?;
    // Even an empty roster may be an image of an interrupted pool creation. Keep this
    // no-lock fast path only across one completed receipt; a healthy empty pool still
    // needs no state directory, snapshot reads or usage polling.
    let empty = store.sync_read_receipt().and_then(|receipt| {
        let raw = read_roster()?;
        store.check_sync_read_receipt(&receipt)?;
        Ok(raw.rows.is_empty().then_some(raw))
    });
    // A concurrent commit can invalidate the bracket. Resolve it once under the
    // writer lock; an invalid receipt still refuses there before any pool use.
    if let Ok(Some(raw)) = empty {
        return Ok(from_empty(raw));
    }
    let _held = store
        .acquire_within(&accounts::LOCK_LADDER)
        .map_err(|why| why.to_string())?
        .ok_or_else(|| accounts::LOCK_HELD.to_owned())?;
    read_held(&store, &_held)
}

/// Reads the usage of each of `roster`'s rows of `cli` whose login is here (`enrolled`, or the
/// default row) and whose sample is older than [`USAGE_FRESH_S`] or lacks the current login
/// context, each in its own home and at
/// once, with `read`; records each reading under its row in `cli`'s store (its sample, and the
/// wall it names) in one transaction. The labels recorded. When an overlay CLI's default row is
/// due, its default home is read before linking and reading the overlay homes so any shared
/// suffix file the CLI creates at startup exists there first.
/// Samples from the future or before the last managed renewal are ignored. A context mismatch
/// never clears stored usage or limits; only an accepted new sample receives the current tag.
/// # Errors
/// Account state, bindings or current policy could not be read, the policy changed during
/// polling, or fresh usage could not be recorded. A caller must not select from the old
/// state after a failed write.
pub fn record_usage(
    seat: &Seat<'_>,
    cli: Cli,
    roster: &Roster,
    enrolled: &std::collections::BTreeSet<String>,
    read: ReadUsage<'_>,
) -> Result<Vec<String>, String> {
    let before = pool_snapshot(seat, cli)?;
    if before.roster != *roster {
        return Err("account policy or login changed before usage read".into());
    }
    poll_usage(seat, cli, &before, enrolled, read).map(|(_, labels)| labels)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSampleContext",
        action = "Poll",
        project = "aterm_agent::harness::accounts_pool::tests::sample_context::context_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolCommit",
        action = "Usage",
        project = "aterm_agent::harness::accounts_pool::tests::commit::commit_transition"
    )
)]
fn poll_usage(
    seat: &Seat<'_>,
    cli: Cli,
    before: &PoolSnapshot,
    enrolled: &std::collections::BTreeSet<String>,
    read: ReadUsage<'_>,
) -> Result<(accounts::State, Vec<String>), String> {
    let store = seat.store(cli);
    let due_rows: Vec<&Row> = before
        .roster
        .rows
        .iter()
        .filter(|row| row.dir.is_none() || enrolled.contains(&row.label))
        .filter(|row| {
            before.state.accounts.get(&row.label).is_none_or(|account| {
                account.sample_context.as_ref()
                    != Some(&sample_context(seat, cli, row, &before.logins))
                    || account.sample.as_ref().is_none_or(|sample| {
                        sample.at > seat.now || seat.now.saturating_sub(sample.at) >= USAGE_FRESH_S
                    })
            })
        })
        .collect();
    if due_rows.is_empty() {
        return Ok((before.state.clone(), Vec::new()));
    }
    let is_overlay = matches!(cli.accounts().seam, Seam::Overlay { .. });
    let mut readings: Vec<(String, Usage)> = Vec::new();
    if is_overlay {
        for row in due_rows.iter().copied().filter(|row| row.dir.is_none()) {
            if let Some(home) = row_home(cli, row, seat.home)
                && let Some(usage) = read(cli, &home, seat.now)
            {
                readings.push((row.label.clone(), usage));
            }
        }
    }
    let parallel_due: Vec<(&Row, std::path::PathBuf)> = due_rows
        .into_iter()
        .filter(|row| !is_overlay || row.dir.is_some())
        .filter_map(|row| Some((row, row_home(cli, row, seat.home)?)))
        .collect();
    if !parallel_due.is_empty() {
        readings.extend(std::thread::scope(|scope| {
            let reads: Vec<_> = parallel_due
                .iter()
                .map(|(row, home)| {
                    let label = row.label.clone();
                    scope.spawn(move || read(cli, home, seat.now).map(|usage| (label, usage)))
                })
                .collect();
            reads
                .into_iter()
                .filter_map(|r| r.join().ok().flatten())
                .collect::<Vec<_>>()
        }));
    }
    let held = store
        .acquire_within(&accounts::LOCK_LADDER)
        .map_err(|why| why.to_string())?
        .ok_or_else(|| accounts::LOCK_HELD.to_owned())?;
    let current = snapshot_held(seat, cli, &store, &held)?;
    if current.roster != before.roster || current.logins != before.logins {
        return Err("account policy or login changed during usage read; nothing recorded".into());
    }
    readings.retain(|(label, usage)| {
        usage.sample.at <= seat.now
            && before
                .logins
                .labels
                .get(label)
                .and_then(|binding| binding.renewed_at)
                .is_none_or(|renewed| usage.sample.at >= renewed)
    });
    if readings.is_empty() {
        return Ok((current.state, Vec::new()));
    }
    let mut state_after = None;
    let path = seat.config_dir.join(cli.accounts().roster_file);
    let recorded =
        store.transact_pool_logins_held(&held, &path, seat.now, &mut None, |raw, state, logins| {
            current.require_policy(raw, logins)?;
            let mut events = Vec::new();
            for (label, usage) in &readings {
                let sample_events =
                    accounts::note_sample(state, label, usage.sample.clone(), seat.now);
                if !sample_events.is_empty()
                    && let Some(row) = current.roster.row(label)
                    && let Some(account) = state.accounts.get_mut(label)
                {
                    account.sample_context = Some(sample_context(seat, cli, row, &current.logins));
                }
                events.extend(sample_events);
                if let Some((window, until)) = usage.wall {
                    events.extend(accounts::note_wall(
                        state, label, window, until, seat.now, seat.now,
                    ));
                }
            }
            state_after = Some(state.clone());
            Ok(events)
        });
    recorded.map_err(|why| {
        format!(
            "{}: could not record fresh usage: {why}",
            store.state_file().display()
        )
    })?;
    Ok((
        state_after.ok_or("usage transaction supplied no state")?,
        readings.into_iter().map(|(label, _)| label).collect(),
    ))
}

/// The product's [`ReadUsage`]: Codex's native `/wham/usage` read in `home` within
/// [`USAGE_READ_WAIT`], refreshing `<home>/auth.json` in place when expired
/// ([`super::codex_app_login::read_usage_in_home`]); `None` for any other CLI, which has no usage
/// report aterm reads.
#[must_use]
pub fn live_usage(cli: Cli, home: &Path, now: i64) -> Option<Usage> {
    if cli != Cli::Codex {
        return None;
    }
    let user_home = aterm_types::dirs::home_dir()?;
    super::codex_app_login::read_usage_in_home(home, &user_home, USAGE_READ_WAIT, now)
}

/// The login a launch aterm makes of `cli` starts under ([`accounts::launch_row`] over the bound
/// roster, once [`record_usage`] read each row's usage with `read`): `None` with an unpinned lone
/// default-home row, with an empty roster, for a CLI whose seam no lab pass measured
/// ([`crate::ai::connector::Accounts::measured`]), or where the CLI keeps its login in the OS
/// keyring ([`Seat::keyring_held`]). Under `[harness] accounts = "off"` only the owner's pin
/// is honoured.
/// # Errors
/// The pool's records cannot be read, its effective roster changes during polling, no managed
/// row is usable, the chosen login's overlay cannot be prepared, or its binding, routing or
/// selection changes during preparation. A caller must hold the launch on these failures.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolLaunch",
        action = "Choose",
        project = "aterm_agent::harness::accounts_pool::tests::launch_admission::launch_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolUsagePolicy",
        action = "PollAndChoose",
        project = "aterm_agent::harness::accounts_pool::tests::usage_policy::usage_transition"
    )
)]
pub fn launch_env(
    seat: &Seat<'_>,
    cli: Cli,
    read: ReadUsage<'_>,
) -> Result<Option<LoginEnv>, String> {
    launch_env_using(seat, cli, read, |row| login_env(cli, row, seat.home))
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolLaunchPolicy",
        action = "Finish",
        project = "aterm_agent::harness::accounts_pool::tests::launch_policy::launch_transition"
    )
)]
fn launch_env_using(
    seat: &Seat<'_>,
    cli: Cli,
    read: ReadUsage<'_>,
    mut prepare: impl FnMut(&Row) -> io::Result<LoginEnv>,
) -> Result<Option<LoginEnv>, String> {
    if !cli.accounts().measured || seat.keyring_held(cli) {
        return Ok(None);
    }
    let snapshot = pool_snapshot(seat, cli)?;
    let roster = &snapshot.roster;
    if roster.rows.is_empty() {
        return Ok(None);
    }
    let enrolled = accounts::enrolled_for(cli, roster);
    let configured_mode = seat.mode();
    let pinned = checked_launch_pin(seat, cli, roster, &snapshot.state, &enrolled)?;
    if configured_mode == Mode::Off {
        let Some(pinned) = pinned else {
            return Ok(None);
        };
        let env = prepare(pinned).map_err(|why| why.to_string())?;
        finish_launch(
            seat,
            cli,
            &snapshot,
            pinned,
            &Default::default(),
            configured_mode,
        )?;
        return Ok(Some(env));
    }
    if let [only] = roster.rows.as_slice()
        && only.dir.is_none()
        && pinned.is_none()
    {
        return Ok(None);
    }
    let (state, _) = poll_usage(seat, cli, &snapshot, &enrolled, read)?;
    checked_launch_pin(seat, cli, roster, &state, &enrolled)?;
    let mut candidate_enrolled = enrolled.clone();
    let mut last_overlay_error = None;
    loop {
        let row = accounts::launch_row(roster, &state, &candidate_enrolled, seat.now)
            .ok_or_else(|| last_overlay_error.take().unwrap_or_else(|| format!("{} account pool has no usable account; inspect `aterm harness accounts --cli {}`", cli.id(), cli.id())))?;
        let why = match prepare(row) {
            Ok(env) => {
                let excluded = enrolled.difference(&candidate_enrolled).cloned().collect();
                finish_launch(seat, cli, &snapshot, row, &excluded, configured_mode)?;
                return Ok(Some(env));
            }
            Err(why) => why,
        };
        if (state.pinned && state.active.as_deref() == Some(row.label.as_str()))
            || !candidate_enrolled.remove(&row.label)
        {
            return Err(why.to_string());
        }
        last_overlay_error = Some(why.to_string());
    }
}

fn checked_launch_pin<'a>(
    seat: &Seat<'_>,
    cli: Cli,
    roster: &'a Roster,
    state: &accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
) -> Result<Option<&'a Row>, String> {
    if !state.pinned {
        return Ok(None);
    }
    let pinned = state
        .active
        .as_deref()
        .and_then(|label| roster.row(label))
        .ok_or_else(|| format!("{} account pin names no roster row", cli.id()))?;
    let cond = accounts::condition(
        pinned,
        state,
        pinned.dir.is_none() || enrolled.contains(&pinned.label),
        seat.now,
    );
    if matches!(
        cond,
        accounts::Condition::NotEnrolled | accounts::Condition::SignedOut
    ) {
        return Err(format!(
            "{} account {} is not signed in",
            cli.id(),
            pinned.label
        ));
    }
    Ok(Some(pinned))
}

/// Observe current admission after preparing an environment, completing any earlier admitted
/// commit first. This records no new account decision and does not freeze credential contents,
/// raw roster writers or the caller's later process spawn.
fn finish_launch(
    seat: &Seat<'_>,
    cli: Cli,
    before: &PoolSnapshot,
    row: &Row,
    excluded: &std::collections::BTreeSet<String>,
    configured_mode: Mode,
) -> Result<(), String> {
    let store = seat.store(cli);
    let _held = store
        .acquire_within(&accounts::LOCK_LADDER)
        .map_err(|why| why.to_string())?
        .ok_or(accounts::LOCK_HELD)?;
    let current = snapshot_held(seat, cli, &store, &_held)?;
    if current.roster.row(&row.label) != Some(row)
        || !accounts::RenewalParent::capture(&before.logins, &row.label)
            .matches(&current.logins, &row.label)
    {
        return Err("account policy or login changed while preparing a launch".into());
    }
    if seat.keyring_held(cli) || seat.mode() != configured_mode {
        return Err("account routing changed while preparing a launch".into());
    }
    let mut enrolled = accounts::enrolled_for(cli, &current.roster);
    enrolled.retain(|label| !excluded.contains(label));
    let pinned = checked_launch_pin(seat, cli, &current.roster, &current.state, &enrolled)?;
    let selected = if configured_mode == Mode::Off {
        pinned
    } else {
        accounts::launch_row(&current.roster, &current.state, &enrolled, seat.now)
    };
    if selected.map(|current| &current.label) != Some(&row.label) {
        return Err("account selection changed while preparing a launch".into());
    }
    Ok(())
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolLaunchPolicy",
        action = "AnotherProvider",
        reason = "Model bookkeeping: select the Codex, Grok or Kimi launch fixture"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolLaunchPolicy",
        action = "OffWithPin",
        reason = "Model bookkeeping: select automatic admission or off mode with an explicit pin"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolLaunchPolicy",
        action = "AnotherCase",
        reason = "Model bookkeeping: select the change injected during environment preparation"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolLaunchPolicy",
        action = "Prepare",
        reason = "Model bookkeeping: freeze the native fixture before Finish"
    )
)]
#[doc(hidden)]
pub fn account_pool_launch_policy_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolRecovery",
        action = "AnotherProvider",
        reason = "Select a native Codex, Grok or Kimi recovery fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolRecovery",
        action = "AnotherCase",
        reason = "Select a native interruption or conflict fixture."
    )
)]
#[doc(hidden)]
pub fn account_pool_recovery_environment() {}

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
    /// The caller's `accounts =` snapshot from `aterm.toml`; a move requires it to
    /// agree with the current file before preparing a command.
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
/// mode, `off` included, before the advice is read. A turn end on the row's current login
/// proves the rotation's unproven `active` ([`accounts::prove`]); a superseded login only
/// contributes usage evidence. A move's line is written
/// before anything is committed ([`StayWhy::NoLine`] where it cannot be). A stale caller's
/// policy snapshot is refused before preparing a line. After preparation, both endpoint rows
/// and login generations are checked and advice is recomputed under the
/// account lock. A move that rotates then commits its rotation ([`accounts::begin_rotation`]), so the breaker counts it in that
/// CLI's store alone. A CLI that keeps its login in the OS keyring stays
/// ([`StayWhy::KeyringStore`]).
#[must_use]
pub fn session_move(seat: &Seat<'_>, s: &Session<'_>) -> Moved {
    session_move_using(seat, s, |row| {
        login_env(s.cli, row, seat.home)
            .ok()
            .and_then(|env| relaunch::pool_line(&s.relaunch, s.cli, &env).ok())
    })
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolMovePolicy",
        action = "Finish",
        project = "aterm_agent::harness::accounts_pool::tests::move_policy::move_transition"
    )
)]
fn session_move_using(
    seat: &Seat<'_>,
    s: &Session<'_>,
    mut prepare: impl FnMut(&Row) -> Option<String>,
) -> Moved {
    if s.supervisor == Supervisor::Own {
        return Moved::stay(StayWhy::OtherSupervisor);
    }
    let Ok(mut roster) = seat.bound_roster(s.cli) else {
        return Moved::stay(StayWhy::NoRoster);
    };
    let who = accounts::ProcessLogin::of_env_for(s.cli, s.env.iter().copied());
    let me = who.me(&roster);
    if let Some(label) = me.label().filter(|_| me.default_home && me.stored_login) {
        let recorded = roster
            .row(label)
            .ok_or_else(|| "observed row is missing".into())
            .and_then(|row| record_session(seat, s, &who, row));
        roster = match recorded {
            Ok(current) => current,
            Err(_) => return Moved::stay(StayWhy::NothingDue),
        };
    }
    if seat.keyring_held(s.cli) {
        return Moved::stay(StayWhy::KeyringStore);
    }
    let Ok(snapshot) = pool_snapshot(seat, s.cli) else {
        return Moved::stay(StayWhy::NothingDue);
    };
    if snapshot.roster != roster {
        return Moved::stay(StayWhy::NothingDue);
    }
    let roster = &snapshot.roster;
    let state = &snapshot.state;
    let me = who.me(roster);
    let configured_mode = seat.mode();
    if s.mode != configured_mode {
        return Moved::stay(StayWhy::NothingDue);
    }
    let enrolled = accounts::enrolled_for(s.cli, roster);
    let mut candidate_enrolled = enrolled.clone();
    let mut saw_line_failure = false;
    let (advice, line) = loop {
        let advice = accounts::advise_session(
            s.cli,
            s.supervisor,
            roster,
            state,
            &candidate_enrolled,
            &me,
            &s.point,
            s.mode,
            seat.now,
        );
        let Advice::Move { to, .. } = &advice else {
            return if saw_line_failure {
                Moved::stay(StayWhy::NoLine)
            } else {
                Moved { advice, line: None }
            };
        };
        let Some(row) = roster.row(to) else {
            return Moved::stay(if saw_line_failure {
                StayWhy::NoLine
            } else {
                StayWhy::NoCandidate
            });
        };
        if let Some(line) = prepare(row) {
            break (advice, line);
        }
        saw_line_failure = true;
        if (state.pinned && state.active.as_deref() == Some(to.as_str()))
            || !candidate_enrolled.remove(to)
        {
            return Moved::stay(StayWhy::NoLine);
        }
    };
    let excluded = enrolled.difference(&candidate_enrolled).cloned().collect();
    if finish_move(
        seat,
        s,
        &snapshot,
        &who,
        &advice,
        &excluded,
        configured_mode,
    )
    .is_err()
    {
        return Moved::stay(StayWhy::NothingDue);
    }
    Moved {
        advice,
        line: Some(line),
    }
}

/// Validate a prepared line at one current policy observation, after completing any earlier
/// admitted commit. This does not freeze external roster writers or a later process spawn.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolCommit",
        action = "Rotate",
        project = "aterm_agent::harness::accounts_pool::tests::commit::commit_transition"
    )
)]
fn finish_move(
    seat: &Seat<'_>,
    session: &Session<'_>,
    before: &PoolSnapshot,
    who: &accounts::ProcessLogin,
    advice: &Advice,
    excluded: &std::collections::BTreeSet<String>,
    configured_mode: Mode,
) -> Result<(), String> {
    let Advice::Move { to, rotates, .. } = advice else {
        return Err("no prepared account move".into());
    };
    let from = who
        .me(&before.roster)
        .label()
        .ok_or("source account disappeared")?;
    let store = seat.store(session.cli);
    let held = store
        .acquire_within(&accounts::LOCK_LADDER)
        .map_err(|why| why.to_string())?
        .ok_or(accounts::LOCK_HELD)?;
    let current = snapshot_held(seat, session.cli, &store, &held)?;
    for label in [from, to.as_str()] {
        if before.roster.row(label) != current.roster.row(label)
            || !accounts::RenewalParent::capture(&before.logins, label)
                .matches(&current.logins, label)
        {
            return Err("account policy or login changed while preparing a move".into());
        }
    }
    if seat.keyring_held(session.cli)
        || session.mode != configured_mode
        || seat.mode() != configured_mode
    {
        return Err("account routing changed while preparing a move".into());
    }
    let mut enrolled = accounts::enrolled_for(session.cli, &current.roster);
    enrolled.retain(|label| !excluded.contains(label));
    let me = who.me(&current.roster);
    let fresh = accounts::advise_session(
        session.cli,
        session.supervisor,
        &current.roster,
        &current.state,
        &enrolled,
        &me,
        &session.point,
        session.mode,
        seat.now,
    );
    if fresh != *advice {
        return Err("account advice changed while preparing a move".into());
    }
    if *rotates {
        let path = seat.config_dir.join(session.cli.accounts().roster_file);
        store.transact_pool_logins_held(
            &held,
            &path,
            seat.now,
            &mut None,
            |raw, state, logins| {
                current.require_policy(raw, logins)?;
                accounts::begin_rotation(
                    state,
                    &current.roster,
                    &enrolled,
                    current.state.active.as_deref(),
                    accounts::Mover {
                        sid: session.sid,
                        account: from,
                    },
                    to,
                    seat.now,
                )
                .map(|event| vec![event])
                .map_err(|lost| format!("{lost:?}"))
            },
        )?;
    }
    Ok(())
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolMovePolicy",
        action = "AnotherProvider",
        reason = "Model bookkeeping: select the native Codex, Grok or Kimi fixture"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolMovePolicy",
        action = "FollowActive",
        reason = "Model bookkeeping: select rotation or following an already active account"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolMovePolicy",
        action = "AnotherCase",
        reason = "Model bookkeeping: select the change injected during command preparation"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolMovePolicy",
        action = "Choose",
        reason = "Model bookkeeping: freeze the native fixture before Finish"
    )
)]
#[doc(hidden)]
pub fn account_pool_move_environment() {}

/// Re-resolve attribution inside the recording lock: a concurrent renewal can turn a
/// current login into a historical one. Keep its usage, but only an unchanged current
/// login can prove the canary. An unrecognised login supplies neither kind of evidence.
/// Return the effective roster read under the lock for the following advice step.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolCommit",
        action = "Observe",
        project = "aterm_agent::harness::accounts_pool::tests::commit::commit_transition"
    )
)]
fn record_session(
    seat: &Seat<'_>,
    session: &Session<'_>,
    who: &accounts::ProcessLogin,
    row: &accounts::Row,
) -> Result<Roster, String> {
    let path = seat.config_dir.join(session.cli.accounts().roster_file);
    let observation = accounts::LoginObservation {
        roster_path: &path,
        row,
        who,
    };
    let mut recorded = None;
    seat.store(session.cli).transact_pool_logins_within(
        &accounts::LOCK_LADDER,
        &path,
        seat.now,
        &mut None,
        |raw, state, logins| {
            let current = accounts::bound(raw, logins);
            let me = who.me(&current);
            if !me.default_home || !me.stored_login || me.label() != Some(row.label.as_str()) {
                return Err("session login changed; usage attribution is unknown".into());
            }
            let mut events = Vec::new();
            if session.point == Point::TurnEnd {
                if observation.matches_current(&current) {
                    events.extend(accounts::prove(state, &row.label, seat.now));
                }
                events.extend(accounts::note_session(
                    state,
                    session.sid,
                    &row.label,
                    None,
                    seat.now,
                ));
            }
            if let Some(sample) = session.sample.clone() {
                events.extend(accounts::note_sample(state, &row.label, sample, seat.now));
            }
            if let Point::Wall { window, at } = &session.point {
                events.extend(accounts::note_wall(
                    state,
                    &row.label,
                    window,
                    None,
                    at.unwrap_or(seat.now),
                    seat.now,
                ));
            }
            recorded = Some(current);
            Ok(events)
        },
    )?;
    recorded.ok_or_else(|| "session observation was not recorded".into())
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolCommit",
        action = "AnotherOperation",
        reason = "Select rotation or a provider-specific usage or session fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolCommit",
        action = "AnotherFault",
        reason = "Select success, a pre-admission refusal, or an interrupted admitted write."
    )
)]
#[doc(hidden)]
pub fn account_pool_commit_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolLaunch",
        action = "Unmanaged",
        reason = "Bounded environment scenario: the conformance test stages a store holding only an unrelated old cache, which a launch leaves alone before the product's Choose"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolLaunch",
        action = "Ready",
        reason = "Bounded environment scenario: the conformance test stages two managed logins, the active one at its usage wall before the product's Choose"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolLaunch",
        action = "StateUnread",
        reason = "Bounded environment scenario: the conformance test stages an unreadable pool state file before the product's Choose"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolLaunch",
        action = "BindingsUnread",
        reason = "Bounded environment scenario: the conformance test stages unreadable login bindings before the product's Choose"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolLaunch",
        action = "Exhausted",
        reason = "Bounded environment scenario: the conformance test stages every managed login at its usage wall before the product's Choose"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolLaunch",
        action = "Drifted",
        reason = "Bounded environment scenario: the conformance test stages a login holding its own copy of a state file the shared home also holds before the product's Choose"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolLaunch",
        action = "RosterUnread",
        reason = "Bounded environment scenario: the conformance test stages an unreadable account roster before the product's Choose"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolLaunch",
        action = "NoLocalLogin",
        reason = "Bounded environment scenario: the conformance test stages a roster row whose directory holds no login before the product's Choose"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolLaunch",
        action = "InvalidPin",
        reason = "Bounded environment scenario: the conformance test stages a pin to a login the roster does not hold before the product's Choose"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolLaunch",
        action = "UsageWriteFails",
        reason = "Bounded environment scenario: the conformance test stages a held pool lock, so fresh usage cannot be recorded before the product's Choose"
    )
)]
#[doc(hidden)]
pub fn account_pool_launch_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolUsagePolicy",
        action = "AnotherCase",
        reason = "Model bookkeeping: the model steps through its fixed cases, and the conformance test stages each case's fixture for PollAndChoose"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolUsagePolicy",
        action = "Choose",
        reason = "Model bookkeeping: the model fixes the case it checks before the bound PollAndChoose"
    )
)]
#[doc(hidden)]
pub fn account_pool_usage_policy_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSampleContext",
        action = "AnotherCase",
        reason = "Model bookkeeping: the model steps through its fixed cases, and the conformance test stages each case's fixture for the bound Poll"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSampleContext",
        action = "Choose",
        reason = "Model bookkeeping: the model fixes the case it checks before the bound Poll"
    )
)]
#[doc(hidden)]
pub fn account_sample_context_environment() {}

#[cfg(test)]
#[path = "accounts_pool_tests.rs"]
mod tests;
