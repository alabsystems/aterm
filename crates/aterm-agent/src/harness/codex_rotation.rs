// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Propose an account change from authenticated usage and the current pool.
//! The native host must separately prove the failed turn and authorize its
//! restart. A proposal neither commits pool selection nor changes a process.

use super::accounts::{self, Cli, Point, Row, Supervisor};
use super::accounts_pool::{self, Seat, Session, SessionPlan};
use super::cli::Env;
use super::codex_app_login::{self, IdentifiedRead, IdentifiedUsage};
use super::codex_renewal::{self as renewal, Identity, Receipt, ReceiptWriter, Successor};
use super::codex_rotation_pace::OwnRead;
pub use super::codex_rotation_pace::{KeptPace, Paces, ReadPace};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(super) struct LaunchUse(());

pub(super) struct WallUse(super::provider_read::Account);

impl super::provider_read::WallDoor for WallUse {
    fn account(self) -> super::provider_read::Account {
        self.0
    }
}

pub trait UsageReader {
    fn read_on(
        &self,
        seat: &Seat<'_>,
        home: &Path,
        expected_email: Option<&str>,
        now: i64,
        wait: Duration,
        use_token: super::provider_read::Use,
    ) -> IdentifiedRead {
        let _ = seat;
        self.read(home, expected_email, now, wait, use_token)
    }
    fn read(
        &self,
        home: &Path,
        expected_email: Option<&str>,
        now: i64,
        wait: Duration,
        use_token: super::provider_read::Use,
    ) -> IdentifiedRead;
}

/// Whether this build's host can run a usage-limit handoff at all: the sealed renewal service's
/// native host and provider pair is qualified on Apple Silicon macOS only.
pub const QUOTA_ROTATION_QUALIFIED: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));

/// What a usage-limit handoff keeps of its ask's deadline to record its reads, past the provider
/// reads themselves: the first steps of [`accounts::LOCK_LADDER`], so a read the deadline cut
/// short is still on record while another session holds the store's lock briefly.
pub(super) const RECORD_WITHIN: Duration = Duration::from_millis(300);

/// Whether aterm.toml (`policy`, as it read with `warnings`) lets a usage-limit handoff run: a
/// handoff needs rotation on, relaunch and limit resumption on, and a file with nothing to warn
/// of. What [`with_prepared_writer`] refuses and the accounts report's `at a usage limit:` line
/// leaves out, read from the one place.
#[must_use]
pub fn policy_allows_handoff(
    policy: &crate::supervise::SupervisorConfig,
    warnings: &[String],
) -> bool {
    warnings.is_empty()
        && policy.accounts != accounts::Mode::Off
        && policy.relaunch
        && policy.resume_limits
}

pub struct LiveUsage<'a> {
    pub user_home: &'a Path,
    pub config_home: &'a Path,
}

impl UsageReader for LiveUsage<'_> {
    fn read_on(
        &self,
        seat: &Seat<'_>,
        home: &Path,
        expected_email: Option<&str>,
        now: i64,
        wait: Duration,
        use_token: super::provider_read::Use,
    ) -> IdentifiedRead {
        let Some(directory) = home.parent() else {
            return IdentifiedRead::Unread;
        };
        super::cli::cleanup_codex_enrollments(
            &seat.store(Cli::Codex),
            &seat.config_dir.join(Cli::Codex.accounts().roster_file),
            now,
            directory,
        );
        self.read(home, expected_email, now, wait, use_token)
    }
    fn read(
        &self,
        home: &Path,
        expected_email: Option<&str>,
        now: i64,
        wait: Duration,
        use_token: super::provider_read::Use,
    ) -> IdentifiedRead {
        codex_app_login::read_identified_login_with_config(
            home,
            self.config_home,
            self.user_home,
            wait,
            now,
            expected_email,
            use_token,
        )
    }
}

#[derive(Clone, Debug)]
pub struct Proposal<T = String> {
    pub plan: SessionPlan<T>,
    pub source: Row,
    pub destination: Row,
    pub source_identity: Identity,
    pub destination_identity: Identity,
    pub old_auth: PathBuf,
    pub new_auth: PathBuf,
}

impl<T: Clone> Proposal<T> {
    pub fn successor(&self) -> Option<Successor> {
        Some(Successor {
            row: self.destination.clone(),
            old_auth: self.old_auth.clone(),
            new_auth: self.new_auth.clone(),
            generation: self.plan.selected_generation()?,
            identity: self.destination_identity.clone(),
        })
    }

    /// Replaying this proposal never bypasses the pool's current policy,
    /// generation, selection or receipt checks.
    pub fn commit(&self, receipt: &Receipt, now: i64) -> Result<(), &'static str> {
        let next = self.successor().ok_or("selection-missing")?;
        if !receipt.intent.purpose.pool_scoped()
            || receipt.intent.identity != self.source_identity
            || !receipt.matches_successor(&next)
        {
            return Err("selection-changed");
        }
        let binding = receipt_binding(receipt)?;
        self.plan
            .clone()
            .commit_native(now, &receipt.id, &binding)
            .line
            .ok_or("pool-commit-refused")?;
        Ok(())
    }
}

/// Immutable handoff metadata only; no credential bytes enter this digest.
pub fn receipt_binding(receipt: &Receipt) -> Result<String, &'static str> {
    if !receipt.intent.purpose.pool_scoped() || receipt.target_identity().is_none() {
        return Err("not-quota-receipt");
    }
    let fields = aterm_json::to_string(&(
        &receipt.id,
        &receipt.intent,
        &receipt.destination,
        &receipt.sid,
        receipt.old_pid,
        receipt.old_birth,
        receipt.claim_revision,
        receipt.human_seq,
        &receipt.old_auth,
        &receipt.new_auth,
        receipt.generation,
        &receipt.row_stamp,
        &receipt.native_prepare,
    ))
    .map_err(|_| "receipt-binding")?;
    Ok(aterm_codec::hex::encode_lower(
        &aterm_digest::Sha256::digest(format!("aterm/codex-quota-receipt/v1\0{fields}").as_bytes()),
    ))
}

/// Validate the selected account while holding the same writer that publishes
/// native receipt transitions. No network call may run inside the callback.
pub fn with_selected_writer<T>(
    env: &Env,
    receipt: &Receipt,
    use_selected: impl FnOnce(&Successor, &ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    with_selection_writer(
        env,
        receipt,
        SelectionScope::Pool,
        SelectionPhase::Forward,
        Takes::Waiting,
        use_selected,
    )
}

/// [`with_selected_writer`] for a lost login's move ([`renewal::Purpose::LoginRotation`]):
/// the session's own selection, judged of the records alone, with no pool record behind it
/// ([`SelectionScope::Session`]). No network call may run inside the callback.
pub fn with_session_selected_writer<T>(
    env: &Env,
    receipt: &Receipt,
    use_selected: impl FnOnce(&Successor, &ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    with_selection_writer(
        env,
        receipt,
        SelectionScope::Session,
        SelectionPhase::Forward,
        Takes::Waiting,
        use_selected,
    )
}

/// Publish an exec attempt only after the selected account's writer admits it.
/// Refusal before the callback preserves the receipt. Once writing starts,
/// even an error retains the prospective attempt because its bytes may exist.
/// This publishes metadata; the native host must independently authorize Go.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ExecPublication",
        action = "PriorAttempt",
        reason = "The fixture starts with a retained earlier attempt."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ExecPublication",
        action = "NextFault",
        reason = "The fixture chooses writer, policy and real-file failure conditions."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ExecPublication",
        action = "Publish",
        project = "aterm_agent::harness::accounts_pool::tests::quota_proposals::publication::publication_transition"
    )
)]
pub fn publish_selected_committing(
    env: &Env,
    receipt: &mut Receipt,
) -> Result<Option<()>, &'static str> {
    publish_selected_committing_using(env, receipt, renewal::write_receipt)
}

pub(super) fn publish_selected_committing_using(
    env: &Env,
    receipt: &mut Receipt,
    write: impl FnOnce(&Env, &Receipt) -> Result<(), &'static str>,
) -> Result<Option<()>, &'static str> {
    if !receipt.intent.purpose.pool_scoped() || receipt.target_identity().is_none() {
        return Err("not-quota-receipt");
    }
    if receipt.revoked {
        return Err("receipt-revoked");
    }
    let first = receipt.phase == renewal::Phase::Ready && !receipt.launch_attempted;
    let retry = receipt.phase == renewal::Phase::Committing && receipt.launch_attempted;
    if !(first || retry)
        || receipt.accepted_turn.is_some()
        || receipt.successor_pid.is_some()
        || receipt.successor_birth.is_some()
        || receipt.candidate.is_some()
        || receipt.current_attempt.is_some()
    {
        return Err("not-committing");
    }
    let mut committing = receipt.clone();
    committing.phase = renewal::Phase::Committing;
    committing.launch_attempted = true;
    with_selected_writer(env, &committing, |_, _| {
        receipt.clone_from(&committing);
        write(env, receipt)
    })
}

/// Validate data for a Ready receipt before the pool move has been committed.
/// This is not execution authority; native admission uses `with_selected_writer`.
///
/// The machine's sign-in turn and `accounts.lock` are each taken on the store's ladder;
/// [`with_prepared_writer_now`] tries each once instead.
pub fn with_prepared_writer<T>(
    env: &Env,
    receipt: &Receipt,
    use_prepared: impl FnOnce(&Successor, &ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    prepared_writer(env, receipt, Takes::Waiting, use_prepared)
}

/// [`with_prepared_writer`] with the machine's sign-in turn and `accounts.lock` each tried once,
/// never waited for: a turn another sign-in holds answers `signin-busy` at once, and a lock another
/// writer holds `Ok(None)`. Its callers are an exec handoff's `arm` and `reserve`: they ask it
/// while they hold their session's own locks, which the window's thread takes too, and their
/// session asks again shortly. Every other caller of a prepared selection's writer (a TUI
/// session's quota handoff, as it publishes and as it commits, and its lost-login move, as it
/// publishes) waits ([`with_prepared_writer`]).
pub fn with_prepared_writer_now<T>(
    env: &Env,
    receipt: &Receipt,
    use_prepared: impl FnOnce(&Successor, &ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    prepared_writer(env, receipt, Takes::Once, use_prepared)
}

/// The writer of a Ready receipt's selection, taking its two locks as `takes` says.
fn prepared_writer<T>(
    env: &Env,
    receipt: &Receipt,
    takes: Takes,
    use_prepared: impl FnOnce(&Successor, &ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    if receipt.phase != renewal::Phase::Ready
        || receipt.revoked
        || !receipt.has_no_execution_evidence()
    {
        return Err("not-ready");
    }
    let scope = SelectionScope::of(receipt.intent.purpose)?;
    with_selection_writer(
        env,
        receipt,
        scope,
        SelectionPhase::Prepared,
        takes,
        use_prepared,
    )
}

/// Validate a consumed receipt after its pool deduplication record was retired.
/// This validates metadata only. The native caller must separately prove that
/// the same admitted process completed its original turn successfully. The
/// destination must still run, a row read low since included ([`target_stands`]).
pub fn with_completed_writer<T>(
    env: &Env,
    receipt: &Receipt,
    use_completed: impl FnOnce(&Successor, &ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    if receipt.phase != renewal::Phase::Consumed
        || receipt.accepted_turn.as_deref() != Some(receipt.intent.wall.as_str())
        || renewal::read_receipt(env, &receipt.id).as_ref() != Some(receipt)
    {
        return Err("not-consumed");
    }
    with_selection_writer(
        env,
        receipt,
        SelectionScope::Pool,
        SelectionPhase::Completed,
        Takes::Waiting,
        use_completed,
    )
}

/// Revalidate a consumed tree's own account for another authenticated member.
/// The pool's next-launch selection may have changed while this tree remained
/// alive. Native code must separately bind the member to the admitted image.
pub fn with_consumed_member_writer<T>(
    env: &Env,
    receipt: &Receipt,
    use_member: impl FnOnce(&Successor, &ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    if receipt.phase != renewal::Phase::Consumed
        || receipt.accepted_turn.as_deref() != Some(receipt.intent.wall.as_str())
        || renewal::read_receipt(env, &receipt.id).as_ref() != Some(receipt)
    {
        return Err("not-consumed");
    }
    with_selection_writer(
        env,
        receipt,
        SelectionScope::Pool,
        SelectionPhase::Member,
        Takes::Waiting,
        use_member,
    )
}

/// Validate compensation to the original login, without requiring the failed
/// destination's credentials or remaining quota. The native host must still
/// authorize the sealed Rollback command and prove the forward child has ended.
pub fn with_rollback_writer<T>(
    env: &Env,
    receipt: &Receipt,
    restore_source: impl FnOnce(&ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    if receipt.phase != renewal::Phase::Committing
        || !receipt.launch_attempted
        || receipt.revoked
        || receipt.accepted_turn.is_some()
    {
        return Err("not-rollback");
    }
    let scope = SelectionScope::of(receipt.intent.purpose)?;
    with_selection_writer(
        env,
        receipt,
        scope,
        SelectionPhase::Rollback,
        Takes::Waiting,
        |_, writer| restore_source(writer),
    )
}

/// How a selection's writer takes the machine's sign-in turn and `accounts.lock`, the two locks
/// it judges under.
#[derive(Clone, Copy)]
enum Takes {
    /// Each on its store's ladder ([`accounts::Store::signin_within`],
    /// [`accounts::Store::acquire_bounded`]).
    Waiting,
    /// Each tried once, never waited for: a caller that holds locks of its own while it asks, and
    /// asks again shortly, waits for neither under them.
    Once,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectionPhase {
    Prepared,
    Forward,
    Rollback,
    Completed,
    Member,
}

/// Whose selection a move's writer judges: the pool's, or the session's alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionScope {
    /// A usage-limit handoff's ([`renewal::Purpose::WeeklyQuota`]): the pool moved its
    /// selection to the destination, so its deduplication record (`native_rotations`) binds the
    /// receipt and, from its commit on, the destination is the pool's `active` row.
    Pool,
    /// A lost login's move ([`renewal::Purpose::LoginRotation`]): only this session moves, so
    /// no pool record binds it and the pool's `active` row is not the destination's to be. In
    /// its place, until the session is moved, the records must still show what moves it: the
    /// source signed out, the refusal of the token its login file holds settled as an ordinary
    /// end ([`codex_app_login::RefusalReason::ends_ordinarily`]), and the destination one this
    /// Mac may use ([`accounts::State::lease_verdict`]), as a pool move's `begin_rotation` holds
    /// it to.
    Session,
}

impl SelectionScope {
    /// The scope of a receipt of `purpose`; a login renewal moves no account and has none.
    fn of(purpose: renewal::Purpose) -> Result<Self, &'static str> {
        match purpose {
            renewal::Purpose::WeeklyQuota => Ok(Self::Pool),
            renewal::Purpose::LoginRotation => Ok(Self::Session),
            renewal::Purpose::Login => Err("not-moving-receipt"),
        }
    }
}

fn with_selection_writer<T>(
    env: &Env,
    receipt: &Receipt,
    scope: SelectionScope,
    phase: SelectionPhase,
    takes: Takes,
    use_selected: impl FnOnce(&Successor, &ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    if SelectionScope::of(receipt.intent.purpose)? != scope {
        return Err("receipt-scope");
    }
    let expected = receipt.target_identity().ok_or("receipt-identity")?;
    // Only a pool move has a pool record to bind; a session's move has none to check.
    let binding = match scope {
        SelectionScope::Pool => Some(receipt_binding(receipt)?),
        SelectionScope::Session => None,
    };
    let home = env.home.as_deref().ok_or("home-unavailable")?;
    let config = env.config.as_deref().ok_or("config-unavailable")?;
    let store = env
        .accounts_store_for(Cli::Codex)
        .ok_or("store-unavailable")?;
    let store = match takes {
        Takes::Waiting => store,
        Takes::Once => store.bounded_by(&[]),
    };
    let _signin = store
        .signin_within()
        .map_err(|_| renewal::SIGNIN_LOCK)?
        .ok_or(renewal::SIGNIN_BUSY)?;
    renewal::with_receipt_writer_of(env, &store, |writer| {
        if phase == SelectionPhase::Rollback
            && renewal::read_receipt(env, &receipt.id).as_ref() != Some(receipt)
        {
            return Err("receipt-changed");
        }
        let (policy, warnings) = crate::supervise::SupervisorConfig::from_path(Some(config));
        if !policy_allows_handoff(&policy, &warnings) {
            return Err("rotation-policy");
        }
        let raw = accounts::Roster::load_for(
            Cli::Codex,
            &env.accounts_roster_for(Cli::Codex)
                .ok_or("roster-unavailable")?,
        )
        .map_err(|_| "roster-unreadable")?;
        let (logins, _) = store
            .load_logins(env.now)
            .map_err(|_| "bindings-unreadable")?;
        if let Some(binding) = &binding {
            match logins.native_rotations.get(&receipt.id) {
                Some(recorded) if recorded != binding => return Err("pool-commit-changed"),
                None if !matches!(
                    phase,
                    SelectionPhase::Prepared | SelectionPhase::Completed | SelectionPhase::Member
                ) =>
                {
                    return Err("pool-commit-missing");
                }
                _ => {}
            }
        }
        let roster = accounts::bound(&raw, &logins);
        let default = home.join(Cli::Codex.accounts().default_home);
        let directory = |auth: &Path| -> Result<Option<String>, &'static str> {
            if auth.file_name().is_none_or(|name| name != "auth.json") || !auth.is_absolute() {
                return Err("auth-path");
            }
            let dir = auth.parent().ok_or("auth-path")?;
            Ok((dir != default).then(|| dir.to_string_lossy().into_owned()))
        };
        let old_dir = directory(&receipt.old_auth)?;
        let new_dir = directory(&receipt.new_auth)?;
        let sources = roster.rows_of_login(old_dir.as_deref());
        let targets: Vec<_> = roster
            .rows
            .iter()
            .filter(|row| row.dir == new_dir)
            .collect();
        let ([source], [target]) = (sources.as_slice(), targets.as_slice()) else {
            return Err("account-lineage");
        };
        if source.label == target.label
            || super::accounts_pool::held_source(source)
            || target.held_by.is_some()
            || renewal::row_stamp(target) != receipt.row_stamp
            || (phase != SelectionPhase::Rollback
                && logins
                    .get(&target.label)
                    .map_or(0, |value| value.renewal_generation)
                    != receipt.generation)
        {
            return Err("selection-changed");
        }
        let mut state = store.load(env.now).map_err(|_| "state-unreadable")?.0;
        store.hydrate_auth_contexts(&mut state, &roster);
        // A pin stops every move, in every phase: a pool move's and a session's alone. The lost
        // login's own pin ended when its sign-out was staged, so one standing now is a person's.
        if (scope == SelectionScope::Pool
            && !matches!(phase, SelectionPhase::Prepared | SelectionPhase::Member)
            && state.active.as_deref() != Some(target.label.as_str()))
            || state.pinned
            || !target_stands(
                phase,
                target,
                &state,
                receipt.intent.limit.as_deref(),
                env.now,
            )
        {
            return Err("target-unavailable");
        }
        if scope == SelectionScope::Session
            && matches!(phase, SelectionPhase::Prepared | SelectionPhase::Forward)
        {
            if accounts::condition(source, &state, true, env.now) != accounts::Condition::SignedOut
            {
                return Err("source-not-signed-out");
            }
            // As the decision read it: without a custody record, so a custody login never moves.
            if !codex_app_login::row_refusal(source, None, Some(&default), env.now)
                .settled()
                .is_some_and(codex_app_login::RefusalReason::ends_ordinarily)
            {
                return Err("source-not-ended");
            }
            if state.lease_verdict(&target.label, env.now).is_err() {
                return Err("target-unavailable");
            }
        }
        let custody = super::codex_account_custody::lock(
            env,
            source,
            target,
            &receipt.old_auth,
            &receipt.new_auth,
            &receipt.intent.identity,
            (phase != SelectionPhase::Rollback).then_some(expected),
        )?;
        let (latest, warnings) = crate::supervise::SupervisorConfig::from_path(Some(config));
        if !warnings.is_empty()
            || latest.accounts != policy.accounts
            || latest.relaunch != policy.relaunch
            || latest.resume_limits != policy.resume_limits
            || !custody.unchanged(env)?
        {
            return Err("policy-changed");
        }
        use_selected(
            &Successor {
                row: (*target).clone(),
                old_auth: receipt.old_auth.clone(),
                new_auth: receipt.new_auth.clone(),
                generation: receipt.generation,
                identity: expected.clone(),
            },
            writer,
        )
    })
}

/// Whether a move's destination `target` is still one its writer takes in `phase`. A phase that
/// selects or launches onto it (Prepared, Forward) or admits another member of a tree already on
/// it (Member) takes only a fresh row, usable or unseen ([`accounts::Condition::fresh`]).
/// A completion proves a row its successor was served on: one that still runs, a row read low
/// since included ([`accounts::Condition::runs`]), never one walled or signed out since. A proven
/// low row is then routed by the low rule, as any proven active row is: walled sessions leave it
/// for a fresh row, or wait where there is none. A rollback restores the source and asks nothing
/// of the destination. A move asked about another limit (`limit`) takes, in those first phases,
/// no row that limit holds on the records ([`super::accounts_pool::limit_holds`]).
fn target_stands(
    phase: SelectionPhase,
    target: &Row,
    state: &accounts::State,
    limit: Option<&str>,
    now: i64,
) -> bool {
    match phase {
        SelectionPhase::Prepared | SelectionPhase::Forward | SelectionPhase::Member => {
            super::accounts_pool::native_target_admitted(target, state, now)
                && limit.is_none_or(|id| {
                    !super::accounts_pool::limit_holds(
                        &target.label,
                        state,
                        &accounts::limit_wall(id),
                        now,
                    )
                })
        }
        SelectionPhase::Completed => accounts::condition(target, state, true, now).runs(),
        SelectionPhase::Rollback => true,
    }
}

pub(super) fn row_auth(seat: &Seat<'_>, row: &Row) -> PathBuf {
    row.dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| seat.home.join(Cli::Codex.accounts().default_home))
        .join("auth.json")
}

/// The point a host asks [`prepare`] or [`prepare_exec`] at: a usage wall its session's proven
/// turn ended on, seen at `at`, of no window. A caller names none, since the handoff's own fresh
/// read of the session's login says which limit stands ([`walled_window`]).
#[must_use]
pub fn wall_asked(at: i64) -> Point {
    Point::Wall {
        window: String::new(),
        at: Some(at),
    }
}

/// The limit a fresh read of the session's login shows it walled at, where a handoff moves it off
/// that login, and the window the plan records that wall under. A window is spent where the read's
/// sample shows it at 100% (its share taken up to the next whole percent, as
/// [`accounts::Sample::reached_reset`] reads it). In order:
///
/// 1. a spent week leaves as the week's wall ([`accounts::WEEK_LIMIT`]), as it did when only the
///    week's wall moved a session, so where both limits stand the handoff records the week's;
/// 2. a spend control or a workspace's refusal ([`codex_app_login::Refusal::outlasts_windows`])
///    leaves under the fallback window ([`codex_app_login::Usage::fallback_window`]), even beside
///    a five-hour window about to reset: no window's reset ends it;
/// 3. a spent five-hour window leaves as its own wall ([`accounts::SESSION_LIMIT`]) only where the
///    sample names its reset more than [`accounts::RESET_SOON_S`] away. Nearer than that, waiting
///    out the reset costs less than a relaunch on another login, so the session stays (`None`),
///    and an ordinary refusal that window explains stays with it. It stays too where the read names
///    no reset: a wall with none stands by rungs up to six hours ([`accounts::NO_RESET_WALL_S`]),
///    longer than the window itself, so a session with no row to move to would be held past the
///    reset it could have waited out;
/// 4. an ordinary refusal no spent window explains (a credit-only plan out of credits, or one the
///    windows do not show) leaves under the fallback window.
///
/// Where nothing is spent or refused, the session stays. A wall under the fallback window is
/// recorded with that window's reset only where the window is spent, so one below 100% stands by
/// rungs, not to that window's turnover.
///
/// A handoff asked about a limit other than the codex bucket (`limit`, a model's own) judges that
/// limit's entry in the read alone ([`codex_app_login::Usage::limits`]), by the same four rules
/// over its windows of every length: a spent week-long window is rule 1's, and a spent window of
/// any other length (five hours, a day, any other or none named) rule 3's, leaving only where the
/// latest reset such a window names is more than [`accounts::RESET_SOON_S`] away. Each rule's wall
/// is recorded under that limit's own name ([`accounts::limit_wall`]). A read that lists no entry
/// for it answers so ([`Walled::Absent`]), distinctly from an entry that shows no wall
/// ([`Walled::No`]): the session stays either way, and its log says which.
pub(super) fn walled_window(usage: &IdentifiedUsage, limit: Option<&str>, now: i64) -> Walled {
    use crate::ai::codex_limits::Bucket;
    let usage = &usage.usage;
    let Some(id) = limit else {
        let sample = &usage.sample;
        let spent = |used: Option<u32>| used.is_some_and(|used| used >= 100);
        let standing = Standing {
            week: spent(sample.week_pct),
            refusal: usage.refusal,
            shorter: spent(sample.session_pct).then_some(sample.session_reset),
        };
        return match standing.leaves(now) {
            Some(Leaves::Week) => Walled::At(accounts::WEEK_LIMIT.to_owned()),
            Some(Leaves::Refusal) => Walled::At(usage.fallback_window().to_owned()),
            Some(Leaves::Shorter) => Walled::At(accounts::SESSION_LIMIT.to_owned()),
            None => Walled::No,
        };
    };
    let Some(entry) = usage.limits.get(id) else {
        return Walled::Absent;
    };
    let shorter: Vec<_> = entry
        .walls()
        .filter(|(bucket, _)| *bucket != Bucket::Week)
        .map(|(_, reset)| reset)
        .collect();
    let standing = Standing {
        week: entry.walls().any(|(bucket, _)| bucket == Bucket::Week),
        refusal: entry.refusal,
        shorter: (!shorter.is_empty()).then(|| shorter.into_iter().flatten().max()),
    };
    match standing.leaves(now) {
        Some(_) => Walled::At(accounts::limit_wall(id)),
        None => Walled::No,
    }
}

/// What a fresh read of the session's login says of the wall a handoff was asked to leave
/// ([`walled_window`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Walled {
    /// It stands, and a handoff leaves it: the window the plan records it under.
    At(String),
    /// No wall a handoff leaves: the session stays (`not-walled`).
    No,
    /// The read lists no entry for the limit asked about, so nothing shows that limit standing:
    /// the session stays, and says why (`limit-absent`, [`NoHandoff::LimitAbsent`]).
    Absent,
}

/// One bucket's read as [`walled_window`]'s four rules take it: the codex bucket's, or another
/// limit's entry.
struct Standing {
    /// A week-long window of it is spent.
    week: bool,
    /// What refuses its use, where something does.
    refusal: Option<codex_app_login::Refusal>,
    /// Where a window of it of another length is spent: the latest reset such a window names, or
    /// `None` where none names one.
    shorter: Option<Option<i64>>,
}

/// Which of [`walled_window`]'s rules leaves a wall ([`Standing::leaves`]).
enum Leaves {
    /// Rule 1: a spent week.
    Week,
    /// Rule 2 or 4: a refusal no reset ends, or one no spent window explains.
    Refusal,
    /// Rule 3: a spent window of another length, whose reset is not near.
    Shorter,
}

impl Standing {
    fn leaves(&self, now: i64) -> Option<Leaves> {
        if self.week {
            return Some(Leaves::Week);
        }
        if self
            .refusal
            .is_some_and(codex_app_login::Refusal::outlasts_windows)
        {
            return Some(Leaves::Refusal);
        }
        if let Some(reset) = self.shorter {
            return reset
                .is_some_and(|reset| reset.saturating_sub(now) > accounts::RESET_SOON_S)
                .then_some(Leaves::Shorter);
        }
        self.refusal.map(|_| Leaves::Refusal)
    }
}

/// Whether a fresh read lets a session move to its row, by the rule its writer holds a sample on
/// record to ([`accounts::Sample::low`]), so a read it just made selects no row it would refuse.
/// A read that refuses ordinary use, by any [`codex_app_login::Refusal`], has a wall
/// ([`codex_app_login::Usage::walls`]), so its row is passed over. A handoff asked about another
/// limit (`limit`) takes, besides, only a read that lists that limit with room
/// ([`codex_app_login::LimitUsage::room`]): a figure for at least one of its windows, none of them
/// spent or low, whatever its length, and nothing refusing it.
pub(super) fn usable(usage: &IdentifiedUsage, limit: Option<&str>, now: i64) -> bool {
    usage.usage.sample.week_pct.is_some_and(|used| used < 100)
        && usage.usage.sample.session_pct.is_none_or(|used| used < 100)
        && usage.usage.walls.is_empty()
        && !usage.usage.sample.low(now)
        && limit.is_none_or(|id| {
            usage
                .usage
                .limits
                .get(id)
                .is_some_and(|entry| entry.room(now))
        })
}

/// Whether `why`, what [`prepare`] refused a session for, ends its asking, and as which kind
/// ([`NoHandoff`]): this session's login is no row of the pool, a login the pool does not manage,
/// one that names another account than its row's, or one whose fresh read shows no limit a
/// handoff leaves ([`walled_window`]). Its control answers that no handoff is coming rather than
/// to wait. How long that answer stands is its kind's. No row of the pool and a login that names
/// another account are nothing a wait changes, and stand for the control's job. No limit to
/// leave is a reading at one time: the TUI's control keeps it for its job, which is one wall's (a
/// TUI turn ends at each wall), and the exec control, whose job is a whole run, reads the login
/// again once the reset that reading waited out has passed with its grace, then less often while
/// it keeps reading so (`codex_exec_handoff_control`).
#[must_use]
pub fn prepare_refusal_stands(why: &str) -> Option<NoHandoff> {
    match why {
        "not-walled" => Some(NoHandoff::NoWallToLeave),
        LIMIT_ABSENT => Some(NoHandoff::LimitAbsent),
        "source-unbound" | "source-missing" | "source-unmanaged" => Some(NoHandoff::NoRowOfThePool),
        SOURCE_NAMES_ANOTHER => Some(NoHandoff::LoginNamesAnother),
        _ => None,
    }
}

/// [`prepare`]'s refusal of a session whose row's login names another account than the row's.
const SOURCE_NAMES_ANOTHER: &str = "source-names-another";

/// [`prepare`]'s refusal of a session asked about a limit its login's fresh read lists no entry
/// for ([`Walled::Absent`]).
const LIMIT_ABSENT: &str = "limit-absent";

/// Why no handoff is coming to a session that asked for one: what ends its asking, for its
/// control's job or, for [`Self::NoWallToLeave`], until its control reads the login again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoHandoff {
    /// A fresh read of the session's login shows no limit a handoff leaves: its week is not
    /// spent, its five-hour window is not spent, resets within [`accounts::RESET_SOON_S`] or
    /// names no reset, and the read refuses no ordinary use but by that window (no spend control,
    /// no workspace's refusal, and no ordinary refusal with no spent window). It is a reading at
    /// one time, of one wall: a session that asks again under
    /// the same request at a later wall (a headless exec run, one root turn) is read again once
    /// that reset and its grace ([`accounts::RESET_GRACE_S`]) have passed, and by longer rungs
    /// while it keeps reading so ([`accounts::NO_RESET_WALL_S`]). Asked about another limit, the
    /// same of that limit's entry ([`walled_window`]).
    NoWallToLeave,
    /// The session asked about a limit other than the codex bucket (`limit=`), and a fresh read of
    /// its login lists no entry for it: nothing shows that limit standing, so no handoff is made
    /// off it. A reading at one time, as [`Self::NoWallToLeave`] is, and read again as it is.
    LimitAbsent,
    /// The session's login is no row the pool hands off.
    NoRowOfThePool,
    /// The session's row's login names another account, which only a sign-in or a sync ends.
    LoginNamesAnother,
}

impl NoHandoff {
    /// What a log line calls it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoWallToLeave => {
                "its login stands at no weekly limit, and at no five-hour limit more than 15 \
                 minutes from its reset, and its read refuses no other ordinary use"
            }
            Self::LimitAbsent => {
                "its login's read lists no entry for the limit it asked about, so nothing shows \
                 that limit standing"
            }
            Self::NoRowOfThePool => "its login is no row the pool hands off",
            Self::LoginNamesAnother => "its row's login names another account",
        }
    }
}

/// Whether a handoff selected now could be published at all: `aterm.toml` beside the rosters
/// lets one run ([`policy_allows_handoff`]) and the machine's sign-in turn is free, the two its
/// writer refuses on before it looks at the selection (`with_selection_writer`). Read before a
/// pass, so that none presents a login's token for a selection nothing could publish; the first
/// ask after both hold reads. The turn is tried once, never waited for, and is released before
/// this returns: no read runs under it.
fn publication_open(seat: &Seat<'_>) -> bool {
    let (policy, warnings) =
        crate::supervise::SupervisorConfig::from_path(Some(&seat.config_dir.join("aterm.toml")));
    if !policy_allows_handoff(&policy, &warnings) {
        return false;
    }
    let turn = seat.store(Cli::Codex).signin_now();
    let free = matches!(turn, Ok(Some(_)));
    drop(turn);
    free
}

/// What a session asks a usage-limit handoff to leave: the wall its login `source` (the account
/// of the failed request) stands at, of the codex bucket, or of the limit `limit` names where it
/// names one (a model's own, the intent's `limit=`, [`codex_app_login::is_limit_id`]). A caller
/// outside this module makes one only of the whole intent it was asked under
/// (`From<&Intent>`), so none can drop the limit that intent names: an identity alone makes one
/// only in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ask<'a> {
    /// The identity the session's login must still read as.
    source: &'a Identity,
    /// The limit other than the codex bucket asked about, where one is.
    limit: Option<&'a str>,
}

#[cfg(test)]
impl<'a> Ask<'a> {
    /// The wall of `source` of the limit `limit` names, or of the codex bucket.
    pub(crate) const fn of(source: &'a Identity, limit: Option<&'a str>) -> Self {
        Self { source, limit }
    }
}

#[cfg(test)]
impl<'a> From<&'a Identity> for Ask<'a> {
    /// The codex bucket's wall of `source`.
    fn from(source: &'a Identity) -> Self {
        Self {
            source,
            limit: None,
        }
    }
}

impl<'a> From<&'a super::codex_renewal::Intent> for Ask<'a> {
    /// What `intent` asks: its source's wall, of the limit it names.
    fn from(intent: &'a super::codex_renewal::Intent) -> Self {
        Self {
            source: &intent.identity,
            limit: intent.limit.as_deref(),
        }
    }
}

/// Resolve only host-observed session paths. The source must still identify
/// the account of the failed request and freshly report a limit a handoff
/// leaves ([`walled_window`]): the week's, a spend control's or a workspace's refusal, a
/// five-hour one not about to reset, or an ordinary refusal no spent window explains; asked
/// about another limit ([`Ask::limit`]), that limit's, and every destination must list that
/// limit with room ([`usable`]). Every destination is freshly read using its own immutable
/// credential snapshot. One total deadline bounds all candidate reads. Nothing is read while a
/// selection could not be published ([`publication_open`]). The asking process's reads so far
/// are `pace`, its tab's ([`Paces`]): what it leaves unread is neither read nor a target, and
/// what this call reads is added to it. Its keeper noted this ask to it ([`KeptPace::asks`]).
pub fn prepare<'a>(
    seat: &Seat<'_>,
    session: &Session<'_>,
    ask: impl Into<Ask<'a>>,
    reader: &dyn UsageReader,
    wait: Duration,
    pace: &mut ReadPace,
) -> Result<Option<Proposal>, &'static str> {
    prepare_quota_wall(
        seat,
        session,
        ask.into(),
        reader,
        wait,
        pace,
        |seat, session, admit| {
            accounts_pool::native_quota_plan_admitting(seat, session, |note| note.record(), admit)
        },
    )
}

/// [`prepare`] of a process that has read nothing and keeps nothing it reads: each call reads as
/// the first ask of a new process would.
#[cfg(test)]
pub(crate) fn prepare_once<'a>(
    seat: &Seat<'_>,
    session: &Session<'_>,
    ask: impl Into<Ask<'a>>,
    reader: &dyn UsageReader,
    wait: Duration,
) -> Result<Option<Proposal>, &'static str> {
    prepare(seat, session, ask, reader, wait, &mut ReadPace::new())
}

/// [`prepare_exec`] of a process that has read nothing and keeps nothing it reads, as
/// [`prepare_once`].
#[cfg(all(test, unix))]
pub(crate) fn prepare_exec_once<'a, R: Clone>(
    seat: &Seat<'_>,
    session: &Session<'_, R>,
    program: &std::path::Path,
    ask: impl Into<Ask<'a>>,
    reader: &dyn UsageReader,
    wait: Duration,
    command: impl FnMut(&Row) -> Option<super::codex_exec_handoff::ExecCommand>,
) -> Result<Option<Proposal<super::codex_exec_handoff::ExecCommand>>, &'static str> {
    prepare_exec(
        seat,
        session,
        program,
        ask,
        reader,
        wait,
        &mut ReadPace::new(),
        command,
    )
}

/// Read and verify the source and candidate accounts through the same provider
/// and roster path, while preparing a typed cooperative exec handoff. Gated and paced as
/// [`prepare`] is, by the asking process's `pace`.
#[cfg(unix)]
#[expect(
    clippy::too_many_arguments,
    reason = "the handoff's six, the program it execs and the asking process's pace"
)]
pub fn prepare_exec<'a, R: Clone>(
    seat: &Seat<'_>,
    session: &Session<'_, R>,
    program: &std::path::Path,
    ask: impl Into<Ask<'a>>,
    reader: &dyn UsageReader,
    wait: Duration,
    pace: &mut ReadPace,
    mut command: impl FnMut(&Row) -> Option<super::codex_exec_handoff::ExecCommand>,
) -> Result<Option<Proposal<super::codex_exec_handoff::ExecCommand>>, &'static str> {
    prepare_quota_wall(
        seat,
        session,
        ask.into(),
        reader,
        wait,
        pace,
        |seat, session, admit| {
            accounts_pool::native_exec_quota_plan(
                seat,
                session,
                program,
                |note| note.record(),
                |row| admit(row).then(|| command(row)).flatten(),
            )
        },
    )
}

fn prepare_quota_wall<T, R: Clone>(
    seat: &Seat<'_>,
    session: &Session<'_, R>,
    ask: Ask<'_>,
    reader: &dyn UsageReader,
    wait: Duration,
    pace: &mut ReadPace,
    select: impl FnOnce(&Seat<'_>, &Session<'_, R>, &mut dyn FnMut(&Row) -> bool) -> SessionPlan<T>,
) -> Result<Option<Proposal<T>>, &'static str> {
    if session.cli != Cli::Codex || session.supervisor != Supervisor::Own {
        return Err("not-native-codex");
    }
    // The caller's window is no evidence and is not read: the fresh read below names the limit.
    if !matches!(&session.point, Point::Wall { at: Some(at), .. } if *at <= seat.now) {
        return Err("not-a-wall");
    }
    let mode = seat.mode_for(Cli::Codex);
    if mode == accounts::Mode::Off || session.mode != mode || seat.keyring_held(Cli::Codex) {
        return Ok(None);
    }
    let deadline = Instant::now().checked_add(wait).ok_or("deadline")?;
    let remaining = || deadline.saturating_duration_since(Instant::now());
    let roster = seat.bound_roster(Cli::Codex).map_err(|_| "roster")?;
    let who = accounts::ProcessLogin::of_env_for(Cli::Codex, session.env.iter().copied());
    let me = who.me(&roster);
    let source = roster
        .row(me.label().ok_or("source-unbound")?)
        .ok_or("source-missing")?
        .clone();
    // Codex never produces a token route: one reads as outside its stored login.
    if !me.default_home || !me.route.is_stored() {
        return Err("source-unmanaged");
    }
    // Another row's pin keeps every session where it is, and the writer takes no target under one:
    // nothing is read of the provider while it stands. A wall of the codex bucket on the session's
    // own row ends its pin; a wall of another limit ends none ([`accounts::note_wall`]), since the
    // row still serves every other limit's sessions, so a pin holds a session asked about one as
    // a pin on another row does, its own row's included.
    let mut state = seat
        .store(Cli::Codex)
        .load(seat.now)
        .map_err(|_| "state")?
        .0;
    if state.pinned
        && (ask.limit.is_some() || state.active.as_deref() != Some(source.label.as_str()))
    {
        return Ok(None);
    }
    // The source login contradicts its row before any provider request; repeating that request
    // cannot hand the session off.
    if accounts_pool::check_login_identity(Cli::Codex, &source, seat.home).is_err() {
        return Err(SOURCE_NAMES_ANOTHER);
    }
    let old_auth = who
        .login
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| seat.home.join(Cli::Codex.accounts().default_home))
        .join("auth.json");
    // What this asking process read lately it leaves unread for now ([`ReadPace`]): laid over
    // this ask's copy of the records, after the pin was judged on them as they are, and never
    // written, so the run on record stays the row's and every session's.
    pace.lay(seat, &roster, &source, &old_auth, &mut state);
    // Nor while this wall is on record and the handoff, as recorded, has no target
    // ([`accounts_pool::native_quota_target_at`]: what the plan below selects where each read admits
    // its candidate, low rows and held, pinned or moved-off defaults refused): no read here could
    // make one, and nothing reads a provider on a timer. Its client asks again every few seconds
    // while the wall stands; what can bring a target (a row's wall ending, another row read again,
    // a row added) is on record, and the first ask after it reads. The first ask at a wall the
    // record lacks still reads, and the plan records it; a wall whose reset is unknown stands for
    // its retry rung, after which the next ask reads it again. A wall lifted with nothing recorded
    // holds until its recorded reset. The wall on record is the week's or the five-hour window's,
    // whichever stands on this row: a five-hour one stands to that window's own reset and its
    // grace ([`accounts::Wall::stands_until`]), never to the week's, so the first ask after it
    // reads, and a login that read shows with room again is answered that no handoff is coming
    // ([`prepare_refusal_stands`]). A read that answered no figure is on record too
    // ([`accounts::Unanswered`]), of this login or a candidate: that row is neither read nor a
    // target before its wait ends ([`accounts_pool::handoff_reads`]). The run is the row's, not
    // this session's: every session here reads a row's one login file, under its row's identity,
    // through one provider endpoint, so another session's failed read of this login also holds
    // this one's for a rung, as its own would. Nor, last, while this process's own reads hold the
    // ask ([`ReadPace::holds`]): the ladder of its own login stands, and the forecast, made at
    // every ask on the records as laid, names a row it has read usable or unanswered, or none. A
    // row that is news to it is read past a ladder its login's limit climbed, not past one a read
    // that showed no limit or ended the pass left. Asked about another limit, the wall on record
    // is that limit's alone ([`accounts::Account::limit_walls`]), and the forecast passes over
    // every row that limit holds ([`accounts_pool::limit_holds`]).
    let account = state.accounts.get(&source.label);
    let wall_recorded = match ask.limit {
        Some(id) => account
            .and_then(|account| account.limit_wall_until(id, seat.now))
            .map(|_| accounts::limit_wall(id)),
        None => [accounts::WEEK_LIMIT, accounts::SESSION_LIMIT]
            .into_iter()
            .find(|window| {
                account
                    .and_then(|account| account.walls.get(*window))
                    .and_then(accounts::Wall::stands_until)
                    .is_some_and(|until| until > seat.now)
            })
            .map(str::to_owned),
    };
    let forecast = wall_recorded.clone().unwrap_or_else(|| {
        ask.limit
            .map_or_else(|| accounts::WEEK_LIMIT.to_owned(), accounts::limit_wall)
    });
    let named = accounts_pool::native_quota_target_at(
        (Cli::Codex, mode),
        (
            &roster,
            &state,
            &accounts::enrolled_in(Cli::Codex, &roster, seat.home),
        ),
        (&source, &forecast),
        (
            accounts::default_login_address(Cli::Codex, seat.home).as_deref(),
            seat.home,
        ),
        seat.now,
    );
    let repeat = named.as_deref().is_none_or(|label| pace.asked(label));
    if !accounts_pool::handoff_reads(&source, &state, seat.now)
        || (named.is_none() && wall_recorded.is_some())
        || pace.holds(&old_auth, repeat, seat.now)
    {
        // Each hold that stands is logged at the first ask it refuses on its rung.
        for wait in pace.waits(seat.now) {
            aterm_log::info!("codex rotation: session {} {wait}", session.sid);
        }
        return Ok(None);
    }
    let left = remaining().saturating_sub(RECORD_WITHIN);
    if left.is_zero() {
        return Ok(None);
    }
    // No pass for a selection nothing could publish ([`publication_open`]), and no rung spent on
    // the asks it refuses.
    if !publication_open(seat) {
        return Ok(None);
    }
    // From here every way out, an unwinding read included, counts as a pass of this process.
    let mut pass = pace.begin(
        session.sid,
        &old_auth,
        named.map(|label| {
            let context = roster
                .row(&label)
                .and_then(|row| seat.store(Cli::Codex).capture_auth_context(row));
            (label, context)
        }),
        seat.now,
    );
    let use_token =
        super::provider_read::Use::wall_read_use(WallUse(super::provider_read::Account::in_home(
            super::provider_read::Read::CodexEndpoint,
            &old_auth,
            Some(&source.label),
        )));
    // The session's own row, where its process still holds that row's login: a login a
    // renewal replaced says nothing of the row, and what its read answered is not recorded.
    let reads_row = me.account.is_some() && row_auth(seat, &source) == old_auth;
    let source_context = reads_row
        .then(|| seat.store(Cli::Codex).capture_auth_context(&source))
        .flatten();
    let note_own = |answer| {
        if reads_row {
            let read = accounts_pool::FreshRead {
                label: source.label.clone(),
                context: source_context.clone(),
                answer,
            };
            let _ = accounts_pool::note_fresh_reads(seat, Cli::Codex, &[read], None, remaining());
        }
    };
    let observed = match reader.read_on(
        seat,
        &old_auth,
        source.expected_identity_email(),
        seat.now,
        left,
        use_token,
    ) {
        IdentifiedRead::Usage(observed) => *observed,
        // No handoff is proposed off a login that reads signed out, and the row says so.
        IdentifiedRead::SignedOut => {
            note_own(accounts_pool::FreshAnswer::SignedOut);
            return Ok(None);
        }
        // Nor off one that answered no figure: the row's run holds the next ask's read.
        IdentifiedRead::Unread => {
            note_own(accounts_pool::FreshAnswer::Unanswered);
            return Ok(None);
        }
    };
    if observed.identity != *ask.source {
        return Err("source-identity-changed");
    }
    let window = match walled_window(&observed, ask.limit, seat.now) {
        Walled::At(window) => window,
        Walled::No => {
            pass.own_answered(OwnRead::NoWall);
            return Err("not-walled");
        }
        Walled::Absent => {
            pass.own_answered(OwnRead::NoWall);
            return Err(LIMIT_ABSENT);
        }
    };
    pass.own_answered(OwnRead::Walled);
    if remaining().is_zero() {
        return Ok(None);
    }
    // The plan records the wall under the window this read named, with that window's own reset
    // from the same read ([`accounts::Sample::reached_reset`]): a five-hour wall holds its row to
    // the five-hour reset, never to the week's, and a refusal recorded under a window it did not
    // spend (the fallback window) names no reset, so it stands by rungs.
    let mut session = session.clone();
    session.sample = Some(observed.usage.sample.clone());
    session.point = Point::Wall {
        window,
        at: Some(seat.now),
    };
    let mut selected = None;
    let mut launch = super::provider_read::Launch::launch_use(LaunchUse(()));
    let mut reads = Vec::new();
    // A limit's wall names no reset the sample shows, so the plan records it with none; the
    // session's own read, recorded after the plan, gives it that limit's own
    // ([`accounts::note_limits`]).
    if ask.limit.is_some() && reads_row {
        reads.push(accounts_pool::FreshRead {
            label: source.label.clone(),
            context: source_context.clone(),
            answer: accounts_pool::FreshAnswer::Usage(observed.usage.clone()),
        });
    }
    let mut admit = |row: &Row| {
        if accounts_pool::check_login_identity(Cli::Codex, row, seat.home).is_err() {
            return false;
        }
        let left = remaining().saturating_sub(RECORD_WITHIN);
        if left.is_zero() || !accounts_pool::handoff_reads(row, &state, seat.now) {
            return false;
        }
        let new_auth = row_auth(seat, row);
        let context = seat.store(Cli::Codex).capture_auth_context(row);
        let Some(use_token) = super::provider_read::Use::launch_read_use(
            &mut launch,
            super::provider_read::Account::in_home(
                super::provider_read::Read::CodexEndpoint,
                &new_auth,
                Some(&row.label),
            ),
        ) else {
            return false;
        };
        pass.minted(&row.label, context.clone());
        let answered = reader.read_on(
            seat,
            &new_auth,
            row.expected_identity_email(),
            seat.now,
            left,
            use_token,
        );
        // What the record of this read compares is the login as it stood before the read; what
        // the pace keeps is the one the read left.
        pass.answered(
            &row.label,
            seat.store(Cli::Codex).capture_auth_context(row),
            match &answered {
                IdentifiedRead::Usage(usage) => usable(usage, ask.limit, seat.now),
                IdentifiedRead::SignedOut => false,
                IdentifiedRead::Unread => true,
            },
        );
        let usage = match answered {
            IdentifiedRead::Usage(usage) => *usage,
            // A candidate whose login reads signed out is passed over on the record, not
            // silently: the next pick does not offer it again.
            IdentifiedRead::SignedOut => {
                reads.push(accounts_pool::FreshRead {
                    label: row.label.clone(),
                    context,
                    answer: accounts_pool::FreshAnswer::SignedOut,
                });
                return false;
            }
            IdentifiedRead::Unread => {
                reads.push(accounts_pool::FreshRead {
                    label: row.label.clone(),
                    context,
                    answer: accounts_pool::FreshAnswer::Unanswered,
                });
                return false;
            }
        };
        reads.push(accounts_pool::FreshRead {
            label: row.label.clone(),
            context,
            answer: accounts_pool::FreshAnswer::Usage(usage.usage.clone()),
        });
        if !usable(&usage, ask.limit, seat.now) || usage.identity.account == ask.source.account {
            return false;
        }
        selected = Some((row.clone(), usage.identity, new_auth));
        true
    };
    let plan = select(seat, &session, &mut admit);
    // The report and later writers judge these rows by what was just read. A selection stands
    // whether or not this records: its writer judges the row again.
    let _ = accounts_pool::note_fresh_reads(seat, Cli::Codex, &reads, ask.limit, remaining());
    // A row read walled, low or signed out that the records now refuse as a target is the
    // record's to gate: the forecast names it again only once the record lets it go, and this
    // process reads it then. One the records do not hold (its read went unrecorded) stays on its
    // rungs here.
    // Asked about another limit, the records hold a row that cannot take its session there too:
    // one read walled, low or with no figure on it, or not listing it ([`accounts::note_limits`]).
    let asked = ask.limit.map(accounts::limit_wall);
    let mut recorded = None;
    pass.held_on_record(|label| {
        recorded
            .get_or_insert_with(|| seat.store(Cli::Codex).load(seat.now).ok())
            .as_ref()
            .zip(roster.row(label))
            .is_some_and(|((after, _), row)| {
                !accounts_pool::native_target_admitted(row, after, seat.now)
                    || asked.as_deref().is_some_and(|window| {
                        accounts_pool::limit_holds(label, after, window, seat.now)
                    })
            })
    });
    let Some((destination, destination_identity, new_auth)) = selected else {
        return Ok(None);
    };
    if remaining().is_zero() {
        return Ok(None);
    }
    let Some((planned_source, planned_destination)) = plan.selected_rows() else {
        return Ok(None);
    };
    if planned_source.label != source.label || *planned_destination != destination {
        return Err("selection-changed");
    }
    Ok(Some(Proposal {
        plan,
        source,
        destination,
        source_identity: ask.source.clone(),
        destination_identity,
        old_auth,
        new_auth,
    }))
}
