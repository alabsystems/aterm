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
pub use super::codex_app_login::Reading;
use super::codex_app_login::Usage;
use super::relaunch::{self, PoolRelaunch};
use crate::ai::connector::Seam;

/// How old a row's usage sample may be before a launch reads it again ([`record_usage`]).
pub const USAGE_FRESH_S: i64 = 300;

/// How long one read of a row's usage is waited for ([`live_usage`]).
pub const USAGE_READ_WAIT: Duration = Duration::from_secs(10);

/// A reader of one login's usage ([`ReadUsage`]): [`LiveUsage`] is the product's; a test
/// passes a closure answering `Option<Usage>`, whose `None` is [`Reading::Unread`].
pub trait UsageReader: Sync {
    /// What `cli`'s login in the home `home` reads at `now`, for a row whose provider email is
    /// `expected_email`. The reader must reject a known identity mismatch in the credential
    /// snapshot it actually uses.
    fn read(&self, cli: Cli, home: &Path, expected_email: Option<&str>, now: i64) -> Reading;
}

impl<F> UsageReader for F
where
    F: Fn(Cli, &Path, Option<&str>, i64) -> Option<Usage> + Sync,
{
    fn read(&self, cli: Cli, home: &Path, expected_email: Option<&str>, now: i64) -> Reading {
        self(cli, home, expected_email, now).into()
    }
}

/// The product's [`UsageReader`]: Codex's native `/wham/usage` read in `home` within
/// [`USAGE_READ_WAIT`], refreshing `<home>/auth.json` in place when expired
/// ([`super::codex_app_login::read_login_in_home`]), a login the provider refused read as
/// signed out; [`Reading::Unread`] for any other CLI, which has no usage report aterm reads.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiveUsage;

impl UsageReader for LiveUsage {
    fn read(&self, cli: Cli, home: &Path, expected_email: Option<&str>, now: i64) -> Reading {
        if cli != Cli::Codex {
            return Reading::Unread;
        }
        let Some(user_home) = aterm_types::dirs::home_dir() else {
            return Reading::Unread;
        };
        super::codex_app_login::read_login_in_home(
            home,
            &user_home,
            USAGE_READ_WAIT,
            now,
            expected_email,
        )
    }
}

/// A reader of one login's usage: `cli`'s login in the home `home`, at `now`, with the row's
/// expected provider email.
pub type ReadUsage<'a> = &'a dyn UsageReader;

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

    /// Claim or look at `cli`'s accounts for this machine ([`super::claims::touch`]):
    /// a launch claims the row it starts on before its first usage reading.
    pub fn claim(&self, cli: Cli, act: super::claims::Act<'_>) -> super::claims::Touched {
        let roster = self.config_dir.join(cli.accounts().roster_file);
        super::claims::touch(self.home, self.state, Some(&roster), cli, act, self.now)
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
        keyring_held_in(cli, self.home)
    }
}

/// [`Seat::keyring_held`] under the user's home `home`.
#[must_use]
pub fn keyring_held_in(cli: Cli, home: &Path) -> bool {
    cli == Cli::Codex && crate::ai::facts::codex_keyring(&home.join(cli.accounts().default_home))
}

/// Whether a launch of `cli` takes no row of its pool, the owner's pin included, whatever the
/// pool says: no lab pass measured its seam ([`crate::ai::connector::Accounts::measured`]), or
/// `keyring`, its login is kept in the OS keyring ([`Seat::keyring_held`]). The accounts report's
/// `next:` reads it too, under every rotation mode, so it names no row where a launch takes none.
#[must_use]
pub const fn takes_no_row(cli: Cli, keyring: bool) -> bool {
    !cli.accounts().measured || keyring
}

/// How a launch's refusal on a Codex program that keeps no row's login its own ends
/// ([`launch_now`]); a refusal read for something other than a worker drops it.
pub const NO_WORKER: &str = "; no worker was started";

/// The command that ends the owner's pin on `cli`'s pool: a launch then takes the pool's pick, or
/// under rotation off no row.
#[must_use]
pub fn unpin_line(cli: Cli) -> String {
    format!("aterm harness accounts use --auto --cli {}", cli.id())
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

/// Check the stored identity evidence for this row without modifying its login.
/// A dedicated Claude login retains its own sign-in profile; the default login
/// uses the user's profile and legacy path. Missing claims are not a contradiction.
///
/// # Errors
/// A known stored email differs from the row's expected identity, or Codex's
/// stored tokens name different known users. This does not authenticate credentials.
pub fn check_login_identity(cli: Cli, row: &Row, home: &Path) -> io::Result<()> {
    match cli {
        Cli::Claude => {
            if let Some(email) = row.expected_identity_email() {
                let matches = match row.dir.as_deref().filter(|dir| !dir.is_empty()) {
                    Some(dir) => {
                        super::profile::identity_in_home(Path::new(dir), email).email_matches
                    }
                    None => super::profile::answers_to(&[], home, email),
                };
                if matches == Some(false) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Claude login does not match the expected account; sign in again",
                    ));
                }
            }
        }
        Cli::Codex => {
            let (_, value) = accounts::login_env_for(cli, row);
            let path = value
                .as_deref()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| home.join(cli.accounts().default_home));
            super::codex_app_login::check_login_identity(&path, row.expected_identity_email())?;
        }
        _ => {}
    }
    Ok(())
}

/// [`accounts::login_env_for`] for `row`, once `cli`'s overlay home for it links the shared
/// entries to the CLI's default home under `home` ([`super::accounts_overlay::ensure`]), where
/// its seam is an overlay.
///
/// # Errors
///
/// Why the overlay home could not be made, or that it holds a shared entry of its own
/// ([`super::accounts_overlay::Drift`]: a session there would run apart from the default home's
/// sessions, history and databases), which is left as it is. A Codex credential file naming
/// different known token users or an email other than the row's expected identity,
/// or a Claude profile naming an email other than expected, is refused before
/// preparing a managed launch.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolIdentity",
        action = "Prepare",
        project = "aterm_agent::harness::accounts_pool::tests::identity::identity_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CodexRowIdentity",
        action = "Prepare",
        project = "aterm_agent::harness::codex_app_login::tests::row_identity_tests::row_identity_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CodexStoredIdentity",
        action = "Read",
        project = "aterm_agent::harness::codex_app_login::tests::stored_identity_tests::stored_identity_transition"
    )
)]
pub fn login_env(cli: Cli, row: &Row, home: &Path) -> io::Result<LoginEnv> {
    let seam = cli.accounts().seam;
    let (var, value) = accounts::login_env_for(cli, row);
    check_login_identity(cli, row, home)?;
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
    if cli == Cli::Kimi {
        let default_home = home.join(cli.accounts().default_home);
        let login_home = row
            .dir
            .as_deref()
            .filter(|dir| !dir.is_empty())
            .map(Path::new)
            .unwrap_or(&default_home);
        super::accounts_overlay::disable_feedback_survey(login_home)?;
    }
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
    if cli == Cli::Codex {
        login_env(cli, row, home).ok()?;
    }
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
#[derive(Debug)]
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
/// context, each in its own home and at once, with `read`; records each reading under its row in
/// `cli`'s store (its sample and the wall it names; a login the provider refused as signed out,
/// and a signed-out one that reads again as signed in) in one transaction. The labels recorded.
/// When an overlay CLI's default row is due, its default home is read before linking and reading
/// the overlay homes so any shared suffix file the CLI creates at startup exists there first.
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
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CodexUsageWalls",
        action = "Record",
        project = "aterm_agent::harness::accounts_pool::tests::quota::quota_transition"
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
    let mut readings: Vec<(String, Reading)> = Vec::new();
    if is_overlay {
        for row in due_rows.iter().copied().filter(|row| row.dir.is_none()) {
            if let Some(home) = row_home(cli, row, seat.home) {
                let reading = read.read(cli, &home, row.expected_identity_email(), seat.now);
                readings.push((row.label.clone(), reading));
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
                    scope.spawn(move || {
                        (
                            label,
                            read.read(cli, home, row.expected_identity_email(), seat.now),
                        )
                    })
                })
                .collect();
            reads
                .into_iter()
                .filter_map(|r| r.join().ok())
                .collect::<Vec<_>>()
        }));
    }
    readings.retain(|(_, reading)| *reading != Reading::Unread);
    let held = store
        .acquire_within(&accounts::LOCK_LADDER)
        .map_err(|why| why.to_string())?
        .ok_or_else(|| accounts::LOCK_HELD.to_owned())?;
    let current = snapshot_held(seat, cli, &store, &held)?;
    if current.roster != before.roster || current.logins != before.logins {
        return Err("account policy or login changed during usage read; nothing recorded".into());
    }
    readings.retain(|(label, reading)| match reading {
        Reading::Usage(usage) => {
            usage.sample.at <= seat.now
                && before
                    .logins
                    .labels
                    .get(label)
                    .and_then(|binding| binding.renewed_at)
                    .is_none_or(|renewed| usage.sample.at >= renewed)
        }
        // Read under the bindings checked unchanged above: no renewal came between.
        Reading::SignedOut => true,
        Reading::Unread => false,
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
            for (label, reading) in &readings {
                let usage = match reading {
                    Reading::Usage(usage) => usage,
                    Reading::SignedOut => {
                        events.extend(accounts::note_auth(
                            state,
                            label,
                            accounts::Auth::SignedOut,
                            seat.now,
                        ));
                        continue;
                    }
                    Reading::Unread => continue,
                };
                // A login read as signed out that reads its usage again is signed in again
                // (a sign-in by hand in its home), so launches choose it once more.
                if state
                    .accounts
                    .get(label)
                    .is_some_and(|account| account.auth == accounts::Auth::SignedOut)
                {
                    events.extend(accounts::note_auth(
                        state,
                        label,
                        accounts::Auth::Ok,
                        seat.now,
                    ));
                }
                let sample_events =
                    accounts::note_sample(state, label, usage.sample.clone(), seat.now);
                if !sample_events.is_empty()
                    && let Some(row) = current.roster.row(label)
                    && let Some(account) = state.accounts.get_mut(label)
                {
                    account.sample_context = Some(sample_context(seat, cli, row, &current.logins));
                }
                events.extend(sample_events);
                for &(window, until) in &usage.walls {
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

/// [`LiveUsage`]'s usage, where it read one.
#[must_use]
pub fn live_usage(cli: Cli, home: &Path, expected_email: Option<&str>, now: i64) -> Option<Usage> {
    LiveUsage.read(cli, home, expected_email, now).usage()
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
    launch_env_for(seat, cli, read, None)
}

/// [`launch_env`] for a launch that runs `program` (`None` where it is not known): where a Codex
/// program keeps no row's login its own ([`super::codex_alab::auth_path_unread`]), the launch
/// may run only under the roster's row on this machine's own login ([`default_serving`]), and
/// the refusal when that row cannot serve says why from its own condition
/// ([`super::codex_alab::only_default`]), so the pool never picks a row its program would run on
/// another login, and never offers a pin its next usage read ends. Every other CLI, and a
/// program that keeps `CODEX_AUTH_PATH`'s login its own, keeps [`accounts::launch_row`]'s pick.
///
/// # Errors
///
/// As [`launch_env`], and that refusal.
pub fn launch_env_for(
    seat: &Seat<'_>,
    cli: Cli,
    read: ReadUsage<'_>,
    program: Option<&Path>,
) -> Result<Option<LoginEnv>, String> {
    launch_env_using(seat, cli, read, program, |row| {
        login_env(cli, row, seat.home)
    })
}

/// The roster's row on this machine's own login (the one with no `dir` once bound) as the only
/// row a launch on a Codex program that keeps no row's login its own may take: it serves while
/// the owner's pin holds it (a row that is not signed out) or while it runs (usable, unseen or
/// low), as [`accounts::launch_row`] takes a row; otherwise the reason, from its own condition.
/// `enrolled` is the set of rows whose login is here ([`accounts::enrolled_in`]): a default row
/// outside it, whose default home holds no login file, is not signed in here.
///
/// # Errors
///
/// Why it cannot serve ([`super::codex_alab::DefaultRow`]): the roster lists none (or a sign-in
/// moved the row that was on that login onto a login of its own), a wall stands on it (its limit
/// and reset), no login of it here is current, another Mac holds it (its warm set or a claim),
/// or this Mac cannot record its claims and is not the row's home.
pub fn default_serving<'a>(
    roster: &'a Roster,
    state: &accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    now: i64,
) -> Result<&'a Row, super::codex_alab::DefaultRow> {
    use super::codex_alab::DefaultRow;
    let Some(row) = roster.row_for_login(None) else {
        return Err(
            match roster
                .rows
                .iter()
                .find(|row| row.was.iter().any(Option::is_none))
            {
                Some(moved) => DefaultRow::Moved(moved.label.clone()),
                None => DefaultRow::Unlisted,
            },
        );
    };
    let label = row.label.clone();
    if row.held_by.is_some() {
        return Err(DefaultRow::Other {
            label,
            why: "held for another Mac's warm set".to_string(),
        });
    }
    if let Err(why) = state.lease_verdict(&row.label, now) {
        return Err(DefaultRow::Other {
            label,
            why: match why {
                accounts::WhyNot::LeasedElsewhere { until } => format!(
                    "leased elsewhere until {}",
                    super::usage::rfc3339_utc(until)
                ),
                accounts::WhyNot::Unclaimed => {
                    "this Mac cannot record its claims, and is not the row's home".to_string()
                }
                other => other.as_str().to_string(),
            },
        });
    }
    let pinned = state.pinned && state.active.as_deref() == Some(row.label.as_str());
    match accounts::condition(row, state, enrolled.contains(&row.label), now) {
        cond @ (accounts::Condition::NotEnrolled | accounts::Condition::SignedOut) => {
            Err(DefaultRow::SignedOut {
                label,
                stored: cond == accounts::Condition::SignedOut,
            })
        }
        accounts::Condition::Limited { .. } if pinned => Ok(row),
        accounts::Condition::Limited { until } => {
            let wall = state.accounts.get(&row.label).and_then(|account| {
                account
                    .walls
                    .iter()
                    .filter_map(|(window, wall)| Some((window, wall, wall.stands_until()?)))
                    .filter(|(_, _, end)| now < *end)
                    .max_by_key(|(_, _, end)| *end)
            });
            Err(DefaultRow::Walled {
                label,
                window: wall.map_or_else(
                    || accounts::UNNAMED_WINDOW.to_string(),
                    |(window, _, _)| window.clone(),
                ),
                reset: wall.and_then(|(_, wall, _)| wall.until),
                until,
            })
        }
        accounts::Condition::Low | accounts::Condition::Usable | accounts::Condition::Unseen => {
            Ok(row)
        }
    }
}

/// [`default_serving`] on `cli`'s records as they stand ([`Seat::bound_roster`], the store's
/// state), for a launch the pool picks nothing for (rotation off with no pin, or Codex's login
/// kept in the OS keyring, [`Seat::keyring_held`], whatever the pin) whose starter carried a row
/// of its own: [`super::codex_alab::DefaultRow::Serves`] where it can serve.
///
/// # Errors
///
/// Why the roster or the store's state cannot be read.
pub fn default_row_now(seat: &Seat<'_>, cli: Cli) -> Result<super::codex_alab::DefaultRow, String> {
    let roster = seat.bound_roster(cli)?;
    let (state, _) = seat
        .store(cli)
        .load(seat.now)
        .map_err(|why| why.to_string())?;
    let enrolled = accounts::enrolled_in(cli, &roster, seat.home);
    Ok(
        match default_serving(&roster, &state, &enrolled, seat.now) {
            Ok(row) => super::codex_alab::DefaultRow::Serves(row.label.clone()),
            Err(blocked) => blocked,
        },
    )
}

/// Why `program` keeps no row of `cli`'s roster its own login, where that narrows a launch to
/// [`default_serving`]: a Codex program [`super::codex_alab::auth_path_unread`] refuses, read
/// only where the roster has a row of its own to refuse.
fn default_only(
    cli: Cli,
    roster: &Roster,
    program: Option<&Path>,
) -> Option<super::codex_alab::AuthPathUnread> {
    if cli != Cli::Codex || roster.rows.iter().all(|row| row.dir.is_none()) {
        return None;
    }
    super::codex_alab::auth_path_unread(program?)
}

/// What a launch of `cli` running `program` may take from `state` as it stands, where the
/// program keeps no row of the roster its own login
/// ([`super::codex_alab::auth_path_unread`]); read by [`launch_opening`] and by the accounts
/// report's `next:`, so the report says what a launch would do.
#[derive(Debug)]
pub struct DefaultOnly<'a> {
    /// Why the program keeps no row's login its own.
    pub why: super::codex_alab::AuthPathUnread,
    /// The row of its own the owner's pin holds, where it holds one: a launch under it is
    /// refused while it holds ([`launch_env_for`]).
    pub pinned_own: Option<&'a Row>,
    /// The row on this machine's own login where it can serve, else why not
    /// ([`default_serving`]).
    pub default: Result<&'a Row, super::codex_alab::DefaultRow>,
}

/// [`DefaultOnly`] for a launch of `cli` running `program` (`None` where it is not known) on
/// `state` (with the rows whose login is here, `enrolled`): `None` where the launch keeps
/// [`accounts::launch_row`]'s pick (every CLI but Codex, a roster with no row of its own, a
/// program not known, or one that keeps `CODEX_AUTH_PATH`'s login its own). Reads no usage.
#[must_use]
pub fn default_only_now<'a>(
    cli: Cli,
    roster: &'a Roster,
    state: &accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    program: Option<&Path>,
    now: i64,
) -> Option<DefaultOnly<'a>> {
    let why = default_only(cli, roster, program)?;
    Some(DefaultOnly {
        why,
        pinned_own: pinned_own(roster, state),
        default: default_serving(roster, state, enrolled, now),
    })
}

/// The row a launch of `cli` running `program` (`None` where it is not known) takes from
/// `state` as it stands under `[harness] accounts = "off"` (with the rows whose login is here,
/// `enrolled`), which reads no usage: the row the owner's pin holds, or `None` where no pin
/// stands, when the launch takes no row and runs on the login its starter carries (a plain
/// starter's, this machine's own). `None` too, the pin included, where the CLI's seam is not
/// measured or `keyring` says its login is kept in the OS keyring ([`keyring_held_in`]), as
/// [`launch_env_for`] takes no row then in every mode. [`launch_env_for`] takes it so where its
/// roster lists a row; the accounts report's `next:` reads it too, so the report says what a
/// launch does.
///
/// # Errors
///
/// Why the launch is refused: the pin names no row of the roster, its row is not signed in here,
/// on a Codex program that keeps no row's login its own
/// ([`super::codex_alab::auth_path_unread`]) it is a row of its own, or the recorded claims keep
/// its row from this Mac (another Mac's senior live claim, or a claim this Mac cannot record on a
/// row that is not its home), as [`launch_env_for`] refuses it.
pub fn off_launch<'a>(
    (cli, keyring): (Cli, bool),
    roster: &'a Roster,
    state: &accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    program: Option<&Path>,
    now: i64,
) -> Result<Option<&'a Row>, String> {
    launch_now(
        (cli, keyring, Mode::Off),
        roster,
        state,
        enrolled,
        program,
        now,
    )
}

/// The row a launch of `cli` running `program` (`None` where it is not known) takes under
/// rotation `mode` from `state` as it stands (with the rows whose login is here, `enrolled`),
/// reading no usage, recording no claim and preparing no login: [`launch_env_for`]'s pick and
/// refusal on records its usage read leaves as they are. `None` where the launch takes no row
/// and runs on the login its starter carries (this machine's own for a plain one): the CLI's
/// seam is not measured or `keyring` says its login is kept in the OS keyring
/// ([`keyring_held_in`]), the roster lists no row, rotation off holds no pin, or the roster's one
/// row is this machine's own login and no pin stands. `aterm ai review` holds its reviewer to it
/// ([`crate::ai::facts::CliAccounts::launch_refused`]), as [`off_launch`] holds the accounts
/// report's `next:` under rotation off.
///
/// # Errors
///
/// Why the launch is refused, in [`launch_env_for`]'s words: the pin names no row of the
/// roster, or its row is not signed in here; on a Codex program that keeps no row's login its
/// own ([`super::codex_alab::auth_path_unread`]), the pin is on a row of its own, or the row on
/// this machine's own login cannot serve; under rotation off, the recorded claims keep the pin's
/// row from this Mac ([`off_launch`]); or no row is usable.
pub fn launch_now<'a>(
    (cli, keyring, mode): (Cli, bool, Mode),
    roster: &'a Roster,
    state: &accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    program: Option<&Path>,
    now: i64,
) -> Result<Option<&'a Row>, String> {
    if takes_no_row(cli, keyring) || roster.rows.is_empty() {
        return Ok(None);
    }
    let unread = default_only(cli, roster, program);
    let unread = unread.as_ref();
    if mode == Mode::Off {
        return off_pick(cli, roster, state, enrolled, unread, now);
    }
    if let Some(why) = unread
        && let Some(pinned) = pinned_own(roster, state)
    {
        return Err(refused_pin(roster, state, enrolled, pinned, why, now));
    }
    let pinned = launch_pin(cli, roster, state, enrolled, now)?;
    if lone_default(roster, pinned) {
        return Ok(None);
    }
    pick_row(cli, roster, state, enrolled, unread, now).map(Some)
}

/// The row of its own (one with a `dir`) the owner's pin in `state` holds, where it holds one. On
/// a program that keeps no row's login its own ([`default_only`]) a launch under that pin is
/// refused for it ([`refused_pin`]) before the row's sign-in is judged ([`launch_pin`]): that
/// build would not run the row signed in either, so its sign-in is no way back.
fn pinned_own<'a>(roster: &'a Roster, state: &accounts::State) -> Option<&'a Row> {
    state
        .active
        .as_deref()
        .filter(|_| state.pinned)
        .and_then(|label| roster.row(label))
        .filter(|row| row.dir.is_some())
}

/// Whether a launch under rotation takes no row because the roster's one row is this machine's
/// own login and the owner's pin (`pinned`) holds none: it runs on that login as it is.
fn lone_default(roster: &Roster, pinned: Option<&Row>) -> bool {
    matches!(roster.rows.as_slice(), [only] if only.dir.is_none()) && pinned.is_none()
}

/// The row a launch under rotation picks from `candidates` (the pool's state less the rows whose
/// preparation failed): on a Codex program that keeps no row's login its own (`unread`) the row
/// on this machine's own login ([`default_serving`]), else [`accounts::launch_row`]'s.
///
/// # Errors
///
/// Why no row serves: that row's own condition ([`super::codex_alab::only_default`]), or no row
/// is usable ([`unusable`]).
fn pick_row<'a>(
    cli: Cli,
    roster: &'a Roster,
    candidates: &accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    unread: Option<&super::codex_alab::AuthPathUnread>,
    now: i64,
) -> Result<&'a Row, String> {
    match unread {
        Some(why) => default_serving(roster, candidates, enrolled, now).map_err(|blocked| {
            format!(
                "{}{NO_WORKER}",
                super::codex_alab::only_default(why, None, &blocked)
            )
        }),
        None => {
            accounts::launch_row(roster, candidates, enrolled, now).ok_or_else(|| unusable(cli))
        }
    }
}

/// A launch's refusal where `cli`'s pool has no usable row.
fn unusable(cli: Cli) -> String {
    format!(
        "{} account pool has no usable account; inspect `aterm harness accounts --cli {}`",
        cli.id(),
        cli.id()
    )
}

/// [`off_launch`] where `unread` says why the program keeps no row's login its own, if it keeps
/// none ([`default_only`]).
fn off_pick<'a>(
    cli: Cli,
    roster: &'a Roster,
    state: &accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    unread: Option<&super::codex_alab::AuthPathUnread>,
    now: i64,
) -> Result<Option<&'a Row>, String> {
    // Rotation off reads no usage: the records as they stand are all there is to judge by.
    if let Some(why) = unread
        && let Some(pinned) = pinned_own(roster, state)
    {
        return Err(refused_pin(roster, state, enrolled, pinned, why, now));
    }
    let Some(pinned) = launch_pin(cli, roster, state, enrolled, now)? else {
        return Ok(None);
    };
    // Nor does it start on a pin the recorded claims keep from this Mac: another Mac's senior
    // live claim, or a claim this Mac cannot record on a row that is not its home.
    state
        .lease_verdict(&pinned.label, now)
        .map_err(|why| off_claim_refused(cli, &pinned.label, why))?;
    Ok(Some(pinned))
}

/// The refusal of a rotation-off launch whose owner's pin names `label`, a row the account claims
/// keep from this Mac (`why`: another Mac's senior live claim, or a claim this Mac could not record
/// on a row that is not its home), with the way back: the pin's end ([`unpin_line`]), after which
/// a launch under rotation off takes no row.
fn off_claim_refused(cli: Cli, label: &str, why: accounts::WhyNot) -> String {
    let because = match why {
        accounts::WhyNot::LeasedElsewhere { until } => format!(
            "another Mac's claim holds it until {}",
            super::usage::rfc3339_utc(until)
        ),
        accounts::WhyNot::Unclaimed => {
            "this Mac cannot record a claim on it, and it is not this Mac's home account".to_owned()
        }
        other => other.as_str().to_owned(),
    };
    format!(
        "{} account `{label}`, the owner's pin under `accounts = \"off\"`, is not this Mac's to \
         use now: {because}; `{}` ends the pin, after which a launch takes no row and runs on the \
         login its starter carries{NO_WORKER}",
        cli.id(),
        unpin_line(cli)
    )
}

/// The refusal of a launch whose owner's pin names `pinned`, a row of its own, on a program that
/// keeps no row's login its own (`why`), from the default row's condition in `state` (with the
/// rows whose login is here, `enrolled`): a launch that reads usage judges it only after the read,
/// so the refusal offers a pin of a row that read shows walled only for once its wall ends
/// ([`super::codex_alab::only_default_pinned`]).
fn refused_pin(
    roster: &Roster,
    state: &accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    pinned: &Row,
    why: &super::codex_alab::AuthPathUnread,
    now: i64,
) -> String {
    let unpinned = accounts::State {
        pinned: false,
        ..state.clone()
    };
    let default = match default_serving(roster, &unpinned, enrolled, now) {
        Ok(row) => super::codex_alab::DefaultRow::Serves(row.label.clone()),
        Err(blocked) => blocked,
    };
    format!(
        "{}{NO_WORKER}",
        super::codex_alab::only_default_pinned(why, &pinned.label, &default)
    )
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolLaunchPolicy",
        action = "Finish",
        project = "aterm_agent::harness::accounts_pool::tests::launch_policy::launch_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolPreparation",
        action = "FailFirst",
        project = "aterm_agent::harness::accounts_pool::tests::preparation::preparation_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolPreparation",
        action = "TryAlternative",
        project = "aterm_agent::harness::accounts_pool::tests::preparation::preparation_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolPreparation",
        action = "Finish",
        project = "aterm_agent::harness::accounts_pool::tests::preparation::preparation_transition"
    )
)]
fn launch_env_using(
    seat: &Seat<'_>,
    cli: Cli,
    read: ReadUsage<'_>,
    program: Option<&Path>,
    mut prepare: impl FnMut(&Row) -> io::Result<LoginEnv>,
) -> Result<Option<LoginEnv>, String> {
    // The gate [`off_launch`] holds rotation off's pick to too.
    if takes_no_row(cli, seat.keyring_held(cli)) {
        return Ok(None);
    }
    // Who holds each account, as the claims read now, before a row is picked.
    seat.claim(cli, super::claims::Act::Look);
    let mut snapshot = pool_snapshot(seat, cli)?;
    let roster = &snapshot.roster;
    if roster.rows.is_empty() {
        return Ok(None);
    }
    let enrolled = accounts::enrolled_in(cli, roster, seat.home);
    let configured_mode = seat.mode();
    let unread = default_only(cli, roster, program);
    let unread = unread.as_ref();
    if configured_mode == Mode::Off {
        let Some(pinned) = off_pick(cli, roster, &snapshot.state, &enrolled, unread, seat.now)?
        else {
            return Ok(None);
        };
        // Rotation off moves nothing, and still never starts on another Mac's account: the pin's
        // row is refused where the claims read it held elsewhere, and claimed before its session
        // starts, as a rotating launch's is (docs/DESIGN-fluid-swarm-2026-10-28.md F19).
        let held = |why: accounts::WhyNot| off_claim_refused(cli, &pinned.label, why);
        snapshot
            .state
            .lease_verdict(&pinned.label, seat.now)
            .map_err(held)?;
        let env = prepare(pinned).map_err(|why| why.to_string())?;
        match seat.claim(cli, super::claims::Act::Claim(&pinned.label)) {
            super::claims::Touched::HeldElsewhere(holder) => {
                return Err(held(accounts::WhyNot::LeasedElsewhere {
                    until: holder.until,
                }));
            }
            super::claims::Touched::Unrecorded(_)
                if !snapshot.state.home_accounts.contains(&pinned.label) =>
            {
                return Err(held(accounts::WhyNot::Unclaimed));
            }
            _ => {}
        }
        finish_launch(
            seat,
            cli,
            &snapshot,
            pinned,
            &Default::default(),
            configured_mode,
            unread.is_some(),
        )?;
        return Ok(Some(env));
    }
    // [`launch_now`] holds a launch that reads no usage to the same steps. A pin on a row of its
    // own on a program that keeps no row's login its own is refused for that once the usage read
    // below is in, whatever that row's sign-in.
    let pinned = match unread.and(pinned_own(roster, &snapshot.state)) {
        Some(own) => Some(own),
        None => checked_launch_pin(seat, cli, roster, &snapshot.state, &enrolled)?,
    };
    if lone_default(roster, pinned) {
        return Ok(None);
    }
    let (mut state, _) = poll_usage(seat, cli, &snapshot, &enrolled, read)?;
    snapshot.state = state.clone();
    if let Some(why) = unread
        && let Some(pinned) = pinned_own(roster, &state)
    {
        return Err(refused_pin(
            roster, &state, &enrolled, pinned, why, seat.now,
        ));
    }
    checked_launch_pin(seat, cli, roster, &state, &enrolled)?;
    let mut excluded = std::collections::BTreeSet::new();
    let mut last_overlay_error = None;
    loop {
        let candidates = without_failed_preparations(&state, &excluded);
        let picked = pick_row(cli, roster, &candidates, &enrolled, unread, seat.now);
        let row = picked.map_err(|why| last_overlay_error.take().unwrap_or(why))?;
        let why = match prepare(row) {
            Ok(env) => {
                // The row's claim is recorded before its session starts. Where another Mac's
                // claim reached this one since the look, or this Mac could not record its claim
                // on a row that is not its home, the launch picks again.
                let claim = seat.claim(cli, super::claims::Act::Claim(&row.label));
                let refused = match claim {
                    super::claims::Touched::HeldElsewhere(_) => true,
                    super::claims::Touched::Unrecorded(_) => {
                        state.unrecorded = true;
                        !state.home_accounts.contains(&row.label)
                    }
                    _ => false,
                };
                if refused {
                    if !excluded.insert(row.label.clone()) {
                        return Err(unusable(cli));
                    }
                    continue;
                }
                finish_launch(
                    seat,
                    cli,
                    &snapshot,
                    row,
                    &excluded,
                    configured_mode,
                    unread.is_some(),
                )?;
                return Ok(Some(env));
            }
            Err(why) => why,
        };
        if (state.pinned && state.active.as_deref() == Some(row.label.as_str()))
            || !excluded.insert(row.label.clone())
        {
            return Err(why.to_string());
        }
        last_overlay_error = Some(why.to_string());
    }
}

/// When a launch of `cli` running `program` (`None` where it is not known) that
/// [`launch_env_for`] refused may start: the instant the first of its rows opens
/// ([`accounts::next_opening`]) where no row here is usable now and one of them is at a limit
/// that ends. On a Codex program that keeps no row's login its own
/// ([`super::codex_alab::auth_path_unread`]), where the roster has a row of its own, only the
/// row on this machine's own login can serve, so it is the instant that row's wall ends
/// ([`default_serving`]'s [`super::codex_alab::DefaultRow::Walled`]), whatever the other rows
/// are. `None` where a launch would not wait on the pool ([`launch_env_for`] chooses no row:
/// rotation is off, the CLI's seam is unmeasured or its login is in the keyring, or the roster
/// is empty or a lone unpinned default row), a row it may take is usable, none of them will
/// open by waiting (signed out, not signed in here, no wall ahead, or on such a Codex program
/// the owner's pin on a row of its own, or a default row that is unlisted or held elsewhere),
/// or the pool cannot be read. Reads no usage.
#[must_use]
pub fn launch_opening(seat: &Seat<'_>, cli: Cli, program: Option<&Path>) -> Option<i64> {
    if !cli.accounts().measured || seat.keyring_held(cli) || seat.mode() == Mode::Off {
        return None;
    }
    let snapshot = pool_snapshot(seat, cli).ok()?;
    let roster = &snapshot.roster;
    if let [only] = roster.rows.as_slice()
        && only.dir.is_none()
        && !snapshot.state.pinned
    {
        return None;
    }
    let enrolled = accounts::enrolled_in(cli, roster, seat.home);
    if let Some(rule) = default_only_now(cli, roster, &snapshot.state, &enrolled, program, seat.now)
    {
        return match rule.default {
            Err(super::codex_alab::DefaultRow::Walled { until, .. })
                if rule.pinned_own.is_none() =>
            {
                Some(until)
            }
            _ => None,
        };
    }
    if accounts::launch_row(roster, &snapshot.state, &enrolled, seat.now).is_some() {
        return None;
    }
    accounts::next_opening(roster, &snapshot.state, &enrolled, seat.now)
}

/// The login a cold relaunch of a `cli` conversation that runs `program` (`None` where it is
/// not known) starts under, where the pool row it ran on (named by `env`, the seam variables its
/// launch carried) is now limited, signed out, not signed in here, or held by another Mac's
/// account claim (or unclaimed where this Mac cannot record its claim on a row that is not its
/// home): the serving row's ([`launch_env_for`], so on a Codex program that keeps no row's login
/// its own only the row on this machine's own login). `None` keeps the row it ran on (having
/// claimed it where claims apply): that row still serves, it ran on no row of the bound roster (a
/// home or credential of its own), the pool has fewer than two rows, no row the program may run
/// serves (the launch then fails as it would have, and a conversation on this machine's own login
/// relaunches on it), or `[harness] accounts = "off"` moves no session.
#[must_use]
pub fn cold_launch_env(
    seat: &Seat<'_>,
    cli: Cli,
    env: &[(&str, &str)],
    read: ReadUsage<'_>,
    program: Option<&Path>,
) -> Option<LoginEnv> {
    if !cli.accounts().measured || seat.keyring_held(cli) || seat.mode() == Mode::Off {
        return None;
    }
    seat.claim(cli, super::claims::Act::Look);
    let snapshot = pool_snapshot(seat, cli).ok()?;
    if snapshot.roster.rows.len() < 2 {
        return None;
    }
    let who = accounts::ProcessLogin::of_env_for(cli, env.iter().copied());
    let ran = snapshot
        .roster
        .row(accounts::judged_label(&snapshot.roster, &who)?)?;
    let enrolled = accounts::enrolled_in(cli, &snapshot.roster, seat.home);
    let cond = accounts::condition(
        ran,
        &snapshot.state,
        enrolled.contains(&ran.label),
        seat.now,
    );
    let usable = !matches!(
        cond,
        accounts::Condition::Limited { .. }
            | accounts::Condition::SignedOut
            | accounts::Condition::NotEnrolled
    ) && snapshot.state.lease_verdict(&ran.label, seat.now).is_ok();
    if usable {
        let claim = seat.claim(cli, super::claims::Act::Claim(&ran.label));
        let refused = match claim {
            super::claims::Touched::HeldElsewhere(_) => true,
            super::claims::Touched::Unrecorded(_) => {
                !snapshot.state.home_accounts.contains(&ran.label)
            }
            _ => false,
        };
        if !refused {
            return None;
        }
    }
    launch_env_for(seat, cli, read, program)
        .ok()
        .flatten()
        .filter(|pick| pick.label != ran.label)
}

/// The login a resume line for a held `cli` conversation names, from
/// the pool's records as they stand (taking no lock, writing nothing, and reading no usage), where
/// the pool row `env` names is recorded as limited, signed out, or not signed in here: the serving
/// row's, once its stored login identity agrees (`None` keeps `env`). A Codex program that cannot
/// isolate named logins may move only onto the serving default row, even when the recorded named
/// row still serves. Call this off the main thread: checking `program` can read its binary.
///
/// # Errors
///
/// The effective named login cannot be isolated by `program`. No resume line may be offered
/// from that result. This is advice, not an execution-time guarantee about a later binary.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ColdResumeCapability",
        action = "NextCase",
        reason = "Select the recorded login, executable, pin and availability fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ColdResumeCapability",
        action = "Advise",
        project = "aterm_agent::harness::accounts_pool::tests::cold_resume_capability::capability_transition"
    )
)]
pub fn cold_resume_env(
    seat: &Seat<'_>,
    cli: Cli,
    env: &[(&str, &str)],
    program: &Path,
) -> Result<Option<LoginEnv>, super::codex_alab::AuthPathUnread> {
    let restriction = (cli == Cli::Codex)
        .then(|| super::codex_alab::auth_path_unread(program))
        .flatten();
    let pick = cold_resume_pick(seat, cli, env, restriction.is_some());
    let named = pick.as_ref().map_or_else(
        || {
            env.iter()
                .any(|(key, value)| *key == super::codex_alab::AUTH_PATH_ENV && !value.is_empty())
        },
        |pick| pick.value.is_some(),
    );
    if named && let Some(why) = restriction {
        return Err(why);
    }
    Ok(pick)
}

fn cold_resume_pick(
    seat: &Seat<'_>,
    cli: Cli,
    env: &[(&str, &str)],
    default_only: bool,
) -> Option<LoginEnv> {
    if !cli.accounts().measured || seat.keyring_held(cli) || seat.mode() == Mode::Off {
        return None;
    }
    let store = seat.store(cli);
    let receipt = store.sync_read_receipt().ok()?;
    let raw = seat.roster(cli).ok()?;
    if raw.rows.len() < 2 {
        return None;
    }
    let (logins, _) = store.load_logins(seat.now).ok()?;
    let (state, _) = store.load(seat.now).ok()?;
    store.check_sync_read_receipt(&receipt).ok()?;
    let roster = accounts::bound(&raw, &logins);
    let who = accounts::ProcessLogin::of_env_for(cli, env.iter().copied());
    let ran = roster.row(accounts::judged_label(&roster, &who)?)?;
    let enrolled = accounts::enrolled_in(cli, &roster, seat.home);
    let ready = |row: &Row| -> Option<LoginEnv> {
        check_login_identity(cli, row, seat.home).ok()?;
        let (var, value) = accounts::login_env_for(cli, row);
        Some(LoginEnv {
            label: row.label.clone(),
            var,
            value,
        })
    };
    if default_only {
        if ran.dir.is_none()
            || checked_launch_pin(seat, cli, &roster, &state, &enrolled)
                .ok()?
                .is_some_and(|row| row.dir.is_some())
        {
            return None;
        }
        return ready(default_serving(&roster, &state, &enrolled, seat.now).ok()?);
    }
    let cond = accounts::condition(ran, &state, enrolled.contains(&ran.label), seat.now);
    if !matches!(
        cond,
        accounts::Condition::Limited { .. }
            | accounts::Condition::SignedOut
            | accounts::Condition::NotEnrolled
    ) {
        return None;
    }
    if let Some(pinned) = checked_launch_pin(seat, cli, &roster, &state, &enrolled).ok()? {
        return ready(pinned).filter(|pick| pick.label != ran.label);
    }
    let mut excluded = std::collections::BTreeSet::new();
    loop {
        let candidates = without_failed_preparations(&state, &excluded);
        let row = accounts::launch_row(&roster, &candidates, &enrolled, seat.now)?;
        if let Some(pick) = ready(row) {
            return (pick.label != ran.label).then_some(pick);
        }
        if (state.pinned && state.active.as_deref() == Some(row.label.as_str()))
            || !excluded.insert(row.label.clone())
        {
            return None;
        }
    }
}

/// Make failed preparations ineligible in a temporary selection snapshot. Keep
/// the full roster, including its circuit-breaker bound and active-account row.
fn without_failed_preparations(
    state: &accounts::State,
    excluded: &std::collections::BTreeSet<String>,
) -> accounts::State {
    let mut candidates = state.clone();
    for label in excluded {
        candidates.accounts.entry(label.clone()).or_default().auth = accounts::Auth::SignedOut;
    }
    candidates
}

fn check_failed_preparations(
    before: &PoolSnapshot,
    current: &PoolSnapshot,
    excluded: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    for label in excluded {
        if before.roster.row(label) != current.roster.row(label)
            || !accounts::RenewalParent::capture(&before.logins, label)
                .matches(&current.logins, label)
            || before
                .state
                .accounts
                .get(label)
                .map_or((accounts::Auth::Unseen, None), |a| (a.auth, a.auth_at))
                != current
                    .state
                    .accounts
                    .get(label)
                    .map_or((accounts::Auth::Unseen, None), |a| (a.auth, a.auth_at))
            || before
                .logins
                .get(label)
                .map(|b| (b.renewed_at, b.served_at))
                != current
                    .logins
                    .get(label)
                    .map(|b| (b.renewed_at, b.served_at))
        {
            return Err("failed account policy or login changed while preparing a fallback".into());
        }
    }
    Ok(())
}

fn checked_launch_pin<'a>(
    seat: &Seat<'_>,
    cli: Cli,
    roster: &'a Roster,
    state: &accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
) -> Result<Option<&'a Row>, String> {
    launch_pin(cli, roster, state, enrolled, seat.now)
}

/// Why the owner's pin in `state` refuses every launch of `cli` under rotation, in
/// [`launch_env_for`]'s words, where it does: it names no row of `roster`, or its row is not
/// signed in here (`enrolled` holds the rows whose login is here) or is known signed out. `None`
/// where no pin stands or its row can be taken. The accounts report's `next:` reads it, so it
/// names no row while the pin refuses every launch.
#[must_use]
pub fn pin_refusal(
    cli: Cli,
    roster: &Roster,
    state: &accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    now: i64,
) -> Option<String> {
    launch_pin(cli, roster, state, enrolled, now).err()
}

/// The row the owner's pin in `state` holds a launch of `cli` to, `None` where no pin stands.
///
/// # Errors
///
/// The pin names no row of `roster`, or its row is not signed in here (`enrolled` holds the
/// rows whose login is here) or is known signed out.
fn launch_pin<'a>(
    cli: Cli,
    roster: &'a Roster,
    state: &accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    now: i64,
) -> Result<Option<&'a Row>, String> {
    if !state.pinned {
        return Ok(None);
    }
    let pinned = state
        .active
        .as_deref()
        .and_then(|label| roster.row(label))
        .ok_or_else(|| format!("{} account pin names no roster row", cli.id()))?;
    let cond = accounts::condition(pinned, state, enrolled.contains(&pinned.label), now);
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
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolIdentity",
        action = "Finish",
        project = "aterm_agent::harness::accounts_pool::tests::identity::identity_transition"
    )
)]
fn finish_launch(
    seat: &Seat<'_>,
    cli: Cli,
    before: &PoolSnapshot,
    row: &Row,
    excluded: &std::collections::BTreeSet<String>,
    configured_mode: Mode,
    default_only: bool,
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
    check_failed_preparations(before, &current, excluded)?;
    if current
        .state
        .active
        .as_ref()
        .is_some_and(|label| excluded.contains(label))
        && (current.state.pinned
            || current.state.canary_holds(seat.now)
            || current.state.active != before.state.active
            || current.state.proven != before.state.proven
            || current.state.canary != before.state.canary
            || current.state.rotations != before.state.rotations)
    {
        return Err("active account changed to a failed preparation".into());
    }
    let candidates = without_failed_preparations(&current.state, excluded);
    let enrolled = accounts::enrolled_in(cli, &current.roster, seat.home);
    let pinned = checked_launch_pin(seat, cli, &current.roster, &candidates, &enrolled)?;
    if default_only && pinned.is_some_and(|pinned| pinned.dir.is_some()) {
        return Err("account selection changed while preparing a launch".into());
    }
    let selected = if configured_mode == Mode::Off {
        pinned
    } else if default_only {
        default_serving(&current.roster, &candidates, &enrolled, seat.now).ok()
    } else {
        accounts::launch_row(&current.roster, &candidates, &enrolled, seat.now)
    };
    if selected.map(|current| &current.label) != Some(&row.label) {
        return Err("account selection changed while preparing a launch".into());
    }
    check_login_identity(cli, row, seat.home).map_err(|why| why.to_string())?;
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
        machine = "AccountPoolPreparation",
        action = "ChooseMove",
        reason = "Select launch or running-session preparation in the native fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolPreparation",
        action = "FailedDefault",
        reason = "Select default or dedicated login preparation failure."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolPreparation",
        action = "AnotherCase",
        reason = "Select alternative availability and pin changes in the native fixture."
    )
)]
#[doc(hidden)]
pub fn account_pool_preparation_environment() {}

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

/// One session attribution inside the pool's writer transaction. The caller may drop
/// it when its local process observation is stale. Recording changes the row and its
/// audit events together, without doing I/O or acquiring another account lock.
#[derive(Debug)]
pub struct SessionNote<'a> {
    state: &'a mut accounts::State,
    events: &'a mut Vec<accounts::Event>,
    sid: &'a str,
    account: &'a str,
    now: i64,
}

impl SessionNote<'_> {
    /// Record the session's observed account and clear its previous hold reason.
    /// Call only while the captured local process context is still current.
    pub fn record(self) {
        self.events.extend(accounts::note_session(
            self.state,
            self.sid,
            self.account,
            None,
            self.now,
        ));
    }
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

/// A pooled session's advice and prepared command, before any rotation is admitted.
/// Dropping the plan retains usage evidence but does not reserve a canary or spend
/// the rotation breaker. An executor consumes it only after accepting the handoff.
pub struct SessionPlan {
    advice: Advice,
    prepared: Option<PreparedMove>,
}

impl std::fmt::Debug for SessionPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionPlan")
            .field("advice", &self.advice)
            .field("prepared", &self.prepared.is_some())
            .finish()
    }
}

#[derive(Debug)]
struct MoveContext {
    cli: Cli,
    sid: String,
    supervisor: Supervisor,
    point: Point,
    mode: Mode,
}

#[derive(Debug)]
struct MoveLocation {
    config_dir: std::path::PathBuf,
    state: std::path::PathBuf,
    home: std::path::PathBuf,
}

impl MoveLocation {
    fn capture(seat: &Seat<'_>) -> io::Result<Self> {
        Ok(Self {
            config_dir: std::path::absolute(seat.config_dir)?,
            state: std::path::absolute(seat.state)?,
            home: std::path::absolute(seat.home)?,
        })
    }

    fn at(&self, now: i64) -> Seat<'_> {
        Seat {
            config_dir: &self.config_dir,
            state: &self.state,
            home: &self.home,
            now,
        }
    }
}

#[derive(Debug)]
struct PreparedMove {
    location: MoveLocation,
    source: MoveContext,
    before: PoolSnapshot,
    who: accounts::ProcessLogin,
    excluded: std::collections::BTreeSet<String>,
    configured_mode: Mode,
    line: String,
}

impl SessionPlan {
    fn stay(why: StayWhy) -> Self {
        Self {
            advice: Advice::Stay(why),
            prepared: None,
        }
    }

    /// The advice calculated from the observed policy and usage.
    #[must_use]
    pub fn advice(&self) -> &Advice {
        &self.advice
    }

    /// Admit a prepared move at the executor's current time. Policy, both login
    /// generations, identity, budget and advice are checked again under the writer.
    /// A refusal returns no command and makes no new rotation. This does not lock
    /// the external process; its executor must still validate its local handoff.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountPoolPlanning",
            action = "Finish",
            project = "aterm_agent::harness::accounts_pool::tests::planning::planning_transition"
        )
    )]
    pub fn commit(self, now: i64) -> Moved {
        let Self { advice, prepared } = self;
        let Some(prepared) = prepared else {
            return Moved { advice, line: None };
        };
        let seat = prepared.location.at(now);
        if finish_move(
            &seat,
            &prepared.source,
            &prepared.before,
            &prepared.who,
            &advice,
            &prepared.excluded,
            prepared.configured_mode,
        )
        .is_err()
        {
            return Moved::stay(StayWhy::NothingDue);
        }
        Moved {
            advice,
            line: Some(prepared.line),
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
/// Taking an expired canary records its new holder before returning the line,
/// without charging another rotation. A replaced canary invalidates the plan.
/// The `note` callback receives the session attribution after the transaction's
/// identity checks. Consume it under the caller's local process guard, or drop it
/// to preserve a newer session row. Dropping it still records valid account-wide
/// proof and usage. The callback runs under the account writer and must do no I/O.
/// This synchronous convenience consumes the plan immediately. Call
/// [`session_plan`] when execution must be admitted separately from advice.
#[must_use]
pub fn session_move(seat: &Seat<'_>, s: &Session<'_>, note: impl FnOnce(SessionNote<'_>)) -> Moved {
    let Ok(location) = MoveLocation::capture(seat) else {
        return Moved::stay(StayWhy::NothingDue);
    };
    let absolute = location.at(seat.now);
    session_move_using(
        &absolute,
        s,
        |row| prepare_session_line(&absolute, s, row),
        note,
    )
}

/// Record a session's admitted usage and prepare advice without committing a
/// rotation. Preparation may create login overlays. Its usage observation may
/// prove or roll back an existing canary, but allocates no new rotation or canary.
/// The executor consumes the returned plan with [`SessionPlan::commit`] only when
/// it can accept the move.
#[must_use]
pub fn session_plan(
    seat: &Seat<'_>,
    s: &Session<'_>,
    note: impl FnOnce(SessionNote<'_>),
) -> SessionPlan {
    let Ok(location) = MoveLocation::capture(seat) else {
        return SessionPlan::stay(StayWhy::NothingDue);
    };
    let owned_seat = location.at(seat.now);
    let seat = &owned_seat;
    session_plan_using(seat, s, |row| prepare_session_line(seat, s, row), note)
}

fn prepare_session_line(seat: &Seat<'_>, s: &Session<'_>, row: &Row) -> Option<String> {
    login_env(s.cli, row, seat.home)
        .ok()
        .and_then(|env| relaunch::pool_line(&s.relaunch, s.cli, &env).ok())
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolMovePolicy",
        action = "Finish",
        project = "aterm_agent::harness::accounts_pool::tests::move_policy::move_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolPreparation",
        action = "Finish",
        project = "aterm_agent::harness::accounts_pool::tests::preparation::preparation_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolPreparation",
        action = "FailFirst",
        project = "aterm_agent::harness::accounts_pool::tests::preparation::preparation_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolPreparation",
        action = "TryAlternative",
        project = "aterm_agent::harness::accounts_pool::tests::preparation::preparation_transition"
    )
)]
fn session_move_using(
    seat: &Seat<'_>,
    s: &Session<'_>,
    prepare: impl FnMut(&Row) -> Option<String>,
    note: impl FnOnce(SessionNote<'_>),
) -> Moved {
    session_plan_using(seat, s, prepare, note).commit(seat.now)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolPlanning",
        action = "Plan",
        project = "aterm_agent::harness::accounts_pool::tests::planning::planning_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolCanary",
        action = "Plan",
        project = "aterm_agent::harness::accounts_pool::tests::canary::canary_transition"
    )
)]
fn session_plan_using(
    seat: &Seat<'_>,
    s: &Session<'_>,
    mut prepare: impl FnMut(&Row) -> Option<String>,
    note: impl FnOnce(SessionNote<'_>),
) -> SessionPlan {
    let Ok(location) = MoveLocation::capture(seat) else {
        return SessionPlan::stay(StayWhy::NothingDue);
    };
    let owned_seat = location.at(seat.now);
    let seat = &owned_seat;
    if s.supervisor == Supervisor::Own {
        return SessionPlan::stay(StayWhy::OtherSupervisor);
    }
    let Ok(mut roster) = seat.bound_roster(s.cli) else {
        return SessionPlan::stay(StayWhy::NoRoster);
    };
    let who = accounts::ProcessLogin::of_env_for(s.cli, s.env.iter().copied());
    let me = who.me(&roster);
    if let Some(label) = me.label().filter(|_| me.default_home && me.stored_login) {
        let recorded = roster
            .row(label)
            .ok_or_else(|| "observed row is missing".into())
            .and_then(|row| record_session(seat, s, &who, row, note));
        roster = match recorded {
            Ok(current) => current,
            Err(_) => return SessionPlan::stay(StayWhy::NothingDue),
        };
    }
    if seat.keyring_held(s.cli) {
        return SessionPlan::stay(StayWhy::KeyringStore);
    }
    let Ok(snapshot) = pool_snapshot(seat, s.cli) else {
        return SessionPlan::stay(StayWhy::NothingDue);
    };
    if snapshot.roster != roster {
        return SessionPlan::stay(StayWhy::NothingDue);
    }
    let roster = &snapshot.roster;
    let state = &snapshot.state;
    let me = who.me(roster);
    let configured_mode = seat.mode();
    if s.mode != configured_mode {
        return SessionPlan::stay(StayWhy::NothingDue);
    }
    let enrolled = accounts::enrolled_in(s.cli, roster, seat.home);
    let mut excluded = std::collections::BTreeSet::new();
    let mut saw_line_failure = false;
    let (advice, line) = loop {
        let candidates = without_failed_preparations(state, &excluded);
        let advice = accounts::advise_session(
            s.cli,
            s.supervisor,
            roster,
            &candidates,
            &enrolled,
            &me,
            &s.point,
            s.mode,
            seat.now,
        );
        let Advice::Move { to, .. } = &advice else {
            return if saw_line_failure {
                SessionPlan::stay(StayWhy::NoLine)
            } else {
                SessionPlan {
                    advice,
                    prepared: None,
                }
            };
        };
        let Some(row) = roster.row(to) else {
            return SessionPlan::stay(if saw_line_failure {
                StayWhy::NoLine
            } else {
                StayWhy::NoCandidate
            });
        };
        if let Some(line) = prepare(row) {
            break (advice, line);
        }
        saw_line_failure = true;
        let own_proven = me.label() == Some(to.as_str()) && state.proven && !state.pinned;
        if (state.active.as_deref() == Some(to.as_str()) && !own_proven)
            || !excluded.insert(to.clone())
        {
            return SessionPlan::stay(StayWhy::NoLine);
        }
    };
    SessionPlan {
        advice,
        prepared: Some(PreparedMove {
            location,
            source: MoveContext {
                cli: s.cli,
                sid: s.sid.to_owned(),
                supervisor: s.supervisor,
                point: s.point.clone(),
                mode: s.mode,
            },
            before: snapshot,
            who,
            excluded,
            configured_mode,
            line,
        }),
    }
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolPlanning",
        action = "AnotherCase",
        reason = "Choose the native delayed-execution fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolPlanning",
        action = "Change",
        reason = "The fixture changes policy, login generation, execution time or a competing plan between preparation and commit."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolPlanning",
        action = "Drop",
        reason = "The executor abandons its plan without calling commit; dropping owned plan data has no store effect."
    )
)]
#[doc(hidden)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolCanary",
        action = "AnotherCase",
        reason = "Select an expired, absent, live, proven or replaced canary fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolCanary",
        action = "Drop",
        reason = "The caller abandons a prepared plan without committing it."
    )
)]
pub fn account_pool_planning_environment() {}

/// Validate a prepared line at one current policy observation, after completing any earlier
/// admitted commit. This does not freeze external roster writers or a later process spawn.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolIdentity",
        action = "Finish",
        project = "aterm_agent::harness::accounts_pool::tests::identity::identity_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolCommit",
        action = "Rotate",
        project = "aterm_agent::harness::accounts_pool::tests::commit::commit_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolCommit",
        action = "Take",
        project = "aterm_agent::harness::accounts_pool::tests::commit::commit_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolCanary",
        action = "First",
        project = "aterm_agent::harness::accounts_pool::tests::canary::canary_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolCanary",
        action = "Second",
        project = "aterm_agent::harness::accounts_pool::tests::canary::canary_transition"
    )
)]
fn finish_move(
    seat: &Seat<'_>,
    session: &MoveContext,
    before: &PoolSnapshot,
    who: &accounts::ProcessLogin,
    advice: &Advice,
    excluded: &std::collections::BTreeSet<String>,
    configured_mode: Mode,
) -> Result<(), String> {
    let Advice::Move {
        to,
        rotates,
        takes_canary,
        ..
    } = advice
    else {
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
    if *takes_canary
        && (before.state.active != current.state.active
            || before.state.canary != current.state.canary)
    {
        return Err("canary changed while preparing a move".into());
    }
    if seat.keyring_held(session.cli)
        || session.mode != configured_mode
        || seat.mode() != configured_mode
    {
        return Err("account routing changed while preparing a move".into());
    }
    check_failed_preparations(before, &current, excluded)?;
    let own_proven = current.state.active.as_deref() == Some(from)
        && current.state.proven
        && !current.state.pinned
        && before.state.active == current.state.active
        && before.state.rotations == current.state.rotations
        && before.state.canary == current.state.canary;
    if !own_proven
        && current
            .state
            .active
            .as_ref()
            .is_some_and(|label| excluded.contains(label))
    {
        return Err("active account changed to a failed preparation".into());
    }
    let candidates = without_failed_preparations(&current.state, excluded);
    let enrolled = accounts::enrolled_in(session.cli, &current.roster, seat.home);
    let me = who.me(&current.roster);
    let fresh = accounts::advise_session(
        session.cli,
        session.supervisor,
        &current.roster,
        &candidates,
        &enrolled,
        &me,
        &session.point,
        session.mode,
        seat.now,
    );
    if fresh != *advice {
        return Err("account advice changed while preparing a move".into());
    }
    check_login_identity(
        session.cli,
        current.roster.row(to).ok_or("target account disappeared")?,
        seat.home,
    )
    .map_err(|why| why.to_string())?;
    if *rotates || *takes_canary {
        let path = seat.config_dir.join(session.cli.accounts().roster_file);
        store.transact_pool_logins_held(
            &held,
            &path,
            seat.now,
            &mut None,
            |raw, state, logins| {
                current.require_policy(raw, logins)?;
                let event = if *rotates {
                    accounts::begin_rotation(
                        state,
                        &current.roster,
                        &enrolled,
                        current.state.active.as_deref(),
                        accounts::Mover {
                            sid: &session.sid,
                            account: from,
                        },
                        to,
                        seat.now,
                    )
                    .map_err(|lost| format!("{lost:?}"))?
                } else {
                    if state.active.as_deref() != Some(to.as_str()) {
                        return Err("canary account changed before admission".into());
                    }
                    accounts::take_canary(
                        state,
                        current.state.canary.as_ref(),
                        &session.sid,
                        seat.now,
                    )
                    .ok_or_else(|| "canary changed before admission".to_owned())?
                };
                Ok(vec![event])
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

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolIdentity",
        action = "Provider",
        reason = "The native fixture selects provider, routing, identity evidence and a later profile replacement."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolIdentity",
        action = "Route",
        reason = "The native fixture selects provider, routing, identity evidence and a later profile replacement."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolIdentity",
        action = "Default",
        reason = "The native fixture selects provider, routing, identity evidence and a later profile replacement."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolIdentity",
        action = "Expected",
        reason = "The native fixture selects provider, routing, identity evidence and a later profile replacement."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolIdentity",
        action = "Credential",
        reason = "The native fixture selects provider, routing, identity evidence and a later profile replacement."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolIdentity",
        action = "Change",
        reason = "The native fixture selects provider, routing, identity evidence and a later profile replacement."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolIdentity",
        action = "Unknown",
        reason = "The native fixture selects provider, routing, identity evidence and a later profile replacement."
    )
)]
#[doc(hidden)]
pub fn account_pool_identity_environment() {}

/// Re-resolve attribution inside the recording lock: a concurrent renewal can turn a
/// current login into a historical one. Check the process's own login directory against
/// current identity policy before recording anything. Keep a historical login's usage,
/// but only an unchanged current login can prove the canary. An unrecognised login or
/// known identity contradiction supplies neither kind of evidence.
/// Return the effective roster read under the lock for the following advice step.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPoolObservationIdentity",
        action = "Observe",
        project = "aterm_agent::harness::accounts_pool::tests::identity::observations::observation_transition"
    )
)]
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
    note: impl FnOnce(SessionNote<'_>),
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
            let mut observed = current
                .row(&row.label)
                .ok_or("observed account disappeared")?
                .clone();
            // A retained process uses its old directory, including the default home.
            observed.dir.clone_from(&who.login);
            check_login_identity(session.cli, &observed, seat.home)
                .map_err(|why| why.to_string())?;
            let mut events = Vec::new();
            if session.point == Point::TurnEnd {
                if observation.matches_current(&current) {
                    events.extend(accounts::prove(state, &row.label, seat.now));
                }
                note(SessionNote {
                    state,
                    events: &mut events,
                    sid: session.sid,
                    account: &row.label,
                    now: seat.now,
                });
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

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolObservationIdentity",
        action = "Provider",
        reason = "The native fixture selects provider, login directory, identity evidence, binding and observation kind."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolObservationIdentity",
        action = "Default",
        reason = "The native fixture selects provider, login directory, identity evidence, binding and observation kind."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolObservationIdentity",
        action = "Expected",
        reason = "The native fixture selects provider, login directory, identity evidence, binding and observation kind."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolObservationIdentity",
        action = "Credential",
        reason = "The native fixture selects provider, login directory, identity evidence, binding and observation kind."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolObservationIdentity",
        action = "Binding",
        reason = "The native fixture selects provider, login directory, identity evidence, binding and observation kind."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPoolObservationIdentity",
        action = "Point",
        reason = "The native fixture selects provider, login directory, identity evidence, binding and observation kind."
    )
)]
#[doc(hidden)]
pub fn account_pool_observation_environment() {}

#[cfg(test)]
#[path = "accounts_pool_tests.rs"]
mod tests;
