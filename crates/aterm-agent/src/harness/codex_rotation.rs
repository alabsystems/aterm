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
        if receipt.intent.purpose != renewal::Purpose::WeeklyQuota
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
    if receipt.intent.purpose != renewal::Purpose::WeeklyQuota
        || receipt.target_identity().is_none()
    {
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
    with_selection_writer(env, receipt, SelectionPhase::Forward, use_selected)
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
    if receipt.intent.purpose != renewal::Purpose::WeeklyQuota
        || receipt.target_identity().is_none()
    {
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
pub fn with_prepared_writer<T>(
    env: &Env,
    receipt: &Receipt,
    use_prepared: impl FnOnce(&Successor, &ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    if receipt.phase != renewal::Phase::Ready
        || receipt.revoked
        || !receipt.has_no_execution_evidence()
    {
        return Err("not-ready");
    }
    with_selection_writer(env, receipt, SelectionPhase::Prepared, use_prepared)
}

/// Validate a consumed receipt after its pool deduplication record was retired.
/// This validates metadata only. The native caller must separately prove that
/// the same admitted process completed its original turn successfully.
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
    with_selection_writer(env, receipt, SelectionPhase::Completed, use_completed)
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
    with_selection_writer(env, receipt, SelectionPhase::Member, use_member)
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
    with_selection_writer(env, receipt, SelectionPhase::Rollback, |_, writer| {
        restore_source(writer)
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectionPhase {
    Prepared,
    Forward,
    Rollback,
    Completed,
    Member,
}

fn with_selection_writer<T>(
    env: &Env,
    receipt: &Receipt,
    phase: SelectionPhase,
    use_selected: impl FnOnce(&Successor, &ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    let expected = receipt.target_identity().ok_or("receipt-identity")?;
    let binding = receipt_binding(receipt)?;
    let home = env.home.as_deref().ok_or("home-unavailable")?;
    let config = env.config.as_deref().ok_or("config-unavailable")?;
    let store = env
        .accounts_store_for(Cli::Codex)
        .ok_or("store-unavailable")?;
    let _signin = store
        .signin_within()
        .map_err(|_| "signin-lock")?
        .ok_or("signin-busy")?;
    renewal::with_receipt_writer(env, |writer| {
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
        match logins.native_rotations.get(&receipt.id) {
            Some(recorded) if recorded != &binding => return Err("pool-commit-changed"),
            None if !matches!(
                phase,
                SelectionPhase::Prepared | SelectionPhase::Completed | SelectionPhase::Member
            ) =>
            {
                return Err("pool-commit-missing");
            }
            _ => {}
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
        if (!matches!(phase, SelectionPhase::Prepared | SelectionPhase::Member)
            && state.active.as_deref() != Some(target.label.as_str()))
            || state.pinned
            || (phase != SelectionPhase::Rollback
                && !super::accounts_pool::native_target_admitted(target, &state, env.now))
        {
            return Err("target-unavailable");
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

fn row_auth(seat: &Seat<'_>, row: &Row) -> PathBuf {
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
/// that login: the week's ([`accounts::WEEK_LIMIT`]) where the week is spent and the read names
/// its wall; else the five-hour window's ([`accounts::SESSION_LIMIT`]) where that is spent, the
/// read names its wall and a reset more than [`accounts::RESET_SOON_S`] away. Nearer than that,
/// waiting out the reset costs less than a relaunch on another login, so the session stays, as it
/// does where neither limit is reached (`None`). It stays too where the read names no reset: a
/// wall with none stands by rungs up to six hours ([`accounts::NO_RESET_WALL_S`]), longer than
/// the window itself, so a session with no row to move to would be held past the reset it could
/// have waited out. The week is judged first,
/// as it was when only the week's wall moved a session: where both limits stand, the handoff
/// leaves and records the week's.
fn walled_window(usage: &IdentifiedUsage, now: i64) -> Option<&'static str> {
    let sample = &usage.usage.sample;
    let wall = |window: &str| {
        usage
            .usage
            .walls
            .iter()
            .find(|(name, _)| *name == window)
            .map(|(_, reset)| *reset)
    };
    if sample.week_pct.is_some_and(|used| used >= 100) && wall(accounts::WEEK_LIMIT).is_some() {
        return Some(accounts::WEEK_LIMIT);
    }
    let reset = wall(accounts::SESSION_LIMIT)?;
    (sample.session_pct.is_some_and(|used| used >= 100)
        && reset.is_some_and(|reset| reset.saturating_sub(now) > accounts::RESET_SOON_S))
    .then_some(accounts::SESSION_LIMIT)
}

/// Whether a fresh read lets a session move to its row, by the rule its writer holds a sample on
/// record to ([`accounts::Sample::low`]), so a read it just made selects no row it would refuse.
fn usable(usage: &IdentifiedUsage, now: i64) -> bool {
    usage.usage.sample.week_pct.is_some_and(|used| used < 100)
        && usage.usage.sample.session_pct.is_none_or(|used| used < 100)
        && usage.usage.walls.is_empty()
        && !usage.usage.sample.low(now)
}

/// Whether `why`, what [`prepare`] refused a session for, stands however often it is asked: this
/// session's login is no row of the pool, a login the pool does not manage, one that names
/// another account than its row's, or one whose fresh read shows no limit a handoff leaves
/// ([`walled_window`]). A wait would change nothing, so its control answers that no handoff is
/// coming rather than to wait.
#[must_use]
pub fn prepare_refusal_stands(why: &str) -> Option<NoHandoff> {
    match why {
        "not-walled" => Some(NoHandoff::NoWallToLeave),
        "source-unbound" | "source-missing" | "source-unmanaged" => Some(NoHandoff::NoRowOfThePool),
        SOURCE_NAMES_ANOTHER => Some(NoHandoff::LoginNamesAnother),
        _ => None,
    }
}

/// [`prepare`]'s refusal of a session whose row's login names another account than the row's.
const SOURCE_NAMES_ANOTHER: &str = "source-names-another";

/// Why no handoff is coming to a session, however often it asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoHandoff {
    /// A fresh read of the session's login shows no limit a handoff leaves: its week is not
    /// spent, and its five-hour window is not spent, resets within [`accounts::RESET_SOON_S`] or
    /// names no reset.
    NoWallToLeave,
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
                 minutes from its reset"
            }
            Self::NoRowOfThePool => "its login is no row the pool hands off",
            Self::LoginNamesAnother => "its row's login names another account",
        }
    }
}

/// Resolve only host-observed session paths. The source must still identify
/// the account of the failed request and freshly report a limit a handoff
/// leaves ([`walled_window`]): the week's, or a five-hour one not about to reset.
/// Every destination is freshly read using its own immutable credential
/// snapshot. One total deadline bounds all candidate reads.
pub fn prepare(
    seat: &Seat<'_>,
    session: &Session<'_>,
    expected_source: &Identity,
    reader: &dyn UsageReader,
    wait: Duration,
) -> Result<Option<Proposal>, &'static str> {
    prepare_quota_wall(
        seat,
        session,
        expected_source,
        reader,
        wait,
        |seat, session, admit| {
            accounts_pool::native_quota_plan_admitting(seat, session, |note| note.record(), admit)
        },
    )
}

/// Read and verify the source and candidate accounts through the same provider
/// and roster path, while preparing a typed cooperative exec handoff.
#[cfg(unix)]
pub fn prepare_exec<R: Clone>(
    seat: &Seat<'_>,
    session: &Session<'_, R>,
    program: &std::path::Path,
    expected_source: &Identity,
    reader: &dyn UsageReader,
    wait: Duration,
    mut command: impl FnMut(&Row) -> Option<super::codex_exec_handoff::ExecCommand>,
) -> Result<Option<Proposal<super::codex_exec_handoff::ExecCommand>>, &'static str> {
    prepare_quota_wall(
        seat,
        session,
        expected_source,
        reader,
        wait,
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
    expected_source: &Identity,
    reader: &dyn UsageReader,
    wait: Duration,
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
    // nothing is read of the provider while it stands. The session's own row's wall ends its pin.
    let state = seat
        .store(Cli::Codex)
        .load(seat.now)
        .map_err(|_| "state")?
        .0;
    if state.pinned && state.active.as_deref() != Some(source.label.as_str()) {
        return Ok(None);
    }
    // The source login contradicts its row before any provider request; repeating that request
    // cannot hand the session off.
    if accounts_pool::check_login_identity(Cli::Codex, &source, seat.home).is_err() {
        return Err(SOURCE_NAMES_ANOTHER);
    }
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
    // this one's for a rung, as its own would.
    let wall_recorded = [accounts::WEEK_LIMIT, accounts::SESSION_LIMIT]
        .into_iter()
        .find(|window| {
            state
                .accounts
                .get(&source.label)
                .and_then(|account| account.walls.get(*window))
                .and_then(accounts::Wall::stands_until)
                .is_some_and(|until| until > seat.now)
        });
    let no_target = wall_recorded.is_some_and(|window| {
        accounts_pool::native_quota_target_at(
            (Cli::Codex, mode),
            (
                &roster,
                &state,
                &accounts::enrolled_in(Cli::Codex, &roster, seat.home),
            ),
            (&source, window),
            (
                accounts::default_login_address(Cli::Codex, seat.home).as_deref(),
                seat.home,
            ),
            seat.now,
        )
        .is_none()
    });
    if no_target || !accounts_pool::handoff_reads(&source, &state, seat.now) {
        return Ok(None);
    }
    let old_auth = who
        .login
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| seat.home.join(Cli::Codex.accounts().default_home))
        .join("auth.json");
    let left = remaining().saturating_sub(RECORD_WITHIN);
    if left.is_zero() {
        return Ok(None);
    }
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
                context: source_context,
                answer,
            };
            let _ = accounts_pool::note_fresh_reads(seat, Cli::Codex, &[read], remaining());
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
        IdentifiedRead::Usage(observed) => observed,
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
    if observed.identity != *expected_source {
        return Err("source-identity-changed");
    }
    let Some(window) = walled_window(&observed, seat.now) else {
        return Err("not-walled");
    };
    if remaining().is_zero() {
        return Ok(None);
    }
    // The plan records the wall under the window this read named, with that window's own reset
    // from the same read ([`accounts::Sample::reached_reset`]): a five-hour wall holds its row to
    // the five-hour reset, never to the week's.
    let mut session = session.clone();
    session.sample = Some(observed.usage.sample);
    session.point = Point::Wall {
        window: window.into(),
        at: Some(seat.now),
    };
    let mut selected = None;
    let mut launch = super::provider_read::Launch::launch_use(LaunchUse(()));
    let mut reads = Vec::new();
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
        let usage = match reader.read_on(
            seat,
            &new_auth,
            row.expected_identity_email(),
            seat.now,
            left,
            use_token,
        ) {
            IdentifiedRead::Usage(usage) => usage,
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
        if !usable(&usage, seat.now) || usage.identity.account == expected_source.account {
            return false;
        }
        selected = Some((row.clone(), usage.identity, new_auth));
        true
    };
    let plan = select(seat, &session, &mut admit);
    // The report and later writers judge these rows by what was just read. A selection stands
    // whether or not this records: its writer judges the row again.
    let _ = accounts_pool::note_fresh_reads(seat, Cli::Codex, &reads, remaining());
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
        source_identity: expected_source.clone(),
        destination_identity,
        old_auth,
        new_auth,
    }))
}
