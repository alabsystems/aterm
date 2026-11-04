// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Propose an account change from authenticated usage and the current pool.
//! The native host must separately prove the failed turn and authorize its
//! restart. A proposal neither commits pool selection nor changes a process.

use super::accounts::{self, Cli, Point, Row, Supervisor};
use super::accounts_pool::{self, Seat, Session, SessionPlan};
use super::cli::Env;
use super::codex_app_login::{self, IdentifiedUsage};
use super::codex_renewal::{self as renewal, Identity, Receipt, ReceiptWriter, Successor};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub trait UsageReader {
    fn read(
        &self,
        home: &Path,
        expected_email: Option<&str>,
        now: i64,
        wait: Duration,
    ) -> Option<IdentifiedUsage>;
}

/// Whether this build's host can run a weekly-limit handoff at all: the sealed renewal service's
/// native host and provider pair is qualified on Apple Silicon macOS only.
pub const QUOTA_ROTATION_QUALIFIED: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));

/// Whether aterm.toml (`policy`, as it read with `warnings`) lets a weekly-limit handoff run: a
/// handoff needs rotation on, relaunch and limit resumption on, and a file with nothing to warn
/// of. What [`with_prepared_writer`] refuses and the accounts report's `at a weekly limit:` line
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
    fn read(
        &self,
        home: &Path,
        expected_email: Option<&str>,
        now: i64,
        wait: Duration,
    ) -> Option<IdentifiedUsage> {
        codex_app_login::read_identified_login_with_config(
            home,
            self.config_home,
            self.user_home,
            wait,
            now,
            expected_email,
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

/// Validate data for a Ready receipt before the pool move has been committed.
/// This is not execution authority; native admission uses `with_selected_writer`.
pub fn with_prepared_writer<T>(
    env: &Env,
    receipt: &Receipt,
    use_prepared: impl FnOnce(&Successor, &ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    if receipt.phase != renewal::Phase::Ready
        || receipt.launch_attempted
        || receipt.accepted_turn.is_some()
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
        let source_custody = super::accounts_sync::codex_completion_source(env, source)
            .map_err(|_| "source-unreadable")?;
        let target_custody = super::accounts_sync::codex_completion_source(env, target)
            .map_err(|_| "source-unreadable")?;
        if source_custody.held_by.is_some() || target_custody.held_by.is_some() {
            return Err("source-held");
        }
        for (row, custody) in [(*source, &source_custody), (*target, &target_custody)] {
            if custody.fleet.as_ref().is_some_and(|fleet| {
                fleet.roster.accounts.iter().any(|account| {
                    account.label == row.label
                        && (account.vendor != row.vendor
                            || !row.expected_identity_email().is_some_and(|email| {
                                accounts::email_matches(email, &account.email)
                            }))
                })
            }) {
                return Err("source-conflict");
            }
        }
        if receipt.old_auth == receipt.new_auth {
            return Err("same-login");
        }
        let mut paths = vec![receipt.old_auth.clone()];
        if phase != SelectionPhase::Rollback {
            paths.push(receipt.new_auth.clone());
        }
        paths.sort();
        let _auth_locks = paths
            .iter()
            .map(|path| {
                codex_app_login::AuthLock::try_take(path)
                    .map_err(|_| "auth-lock")?
                    .ok_or("auth-busy")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let old_home = receipt.old_auth.parent().ok_or("auth-path")?;
        let new_home = receipt.new_auth.parent().ok_or("auth-path")?;
        if !crate::ai::facts::codex_file_store(old_home)
            || codex_app_login::renewal_identity(old_home).as_ref()
                != Some(&receipt.intent.identity)
            || (phase != SelectionPhase::Rollback
                && (!crate::ai::facts::codex_file_store(new_home)
                    || codex_app_login::renewal_identity(new_home).as_ref() != Some(expected)))
        {
            return Err("identity-mismatch");
        }
        let (latest, warnings) = crate::supervise::SupervisorConfig::from_path(Some(config));
        if !warnings.is_empty()
            || latest.accounts != policy.accounts
            || latest.relaunch != policy.relaunch
            || latest.resume_limits != policy.resume_limits
            || super::accounts_sync::codex_completion_source(env, source)
                .map_err(|_| "source-unreadable")?
                != source_custody
            || super::accounts_sync::codex_completion_source(env, target)
                .map_err(|_| "source-unreadable")?
                != target_custody
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

fn weekly_exhausted(usage: &IdentifiedUsage) -> bool {
    usage.usage.sample.week_pct.is_some_and(|used| used >= 100)
        && usage
            .usage
            .walls
            .iter()
            .any(|(window, _)| *window == "seven_day")
}

fn usable(usage: &IdentifiedUsage) -> bool {
    usage.usage.sample.week_pct.is_some_and(|used| used < 100)
        && usage.usage.sample.session_pct.is_none_or(|used| used < 100)
        && usage.usage.walls.is_empty()
}

/// Resolve only host-observed session paths. The source must still identify
/// the account of the failed request and freshly report weekly exhaustion.
/// Every destination is freshly read using its own immutable credential
/// snapshot. One total deadline bounds all candidate reads.
pub fn prepare(
    seat: &Seat<'_>,
    session: &Session<'_>,
    expected_source: &Identity,
    reader: &dyn UsageReader,
    wait: Duration,
) -> Result<Option<Proposal>, &'static str> {
    prepare_with(
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
    prepare_with(
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

fn prepare_with<T, R: Clone>(
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
    if !matches!(&session.point, Point::Wall { window, at: Some(at) }
        if window == "seven_day" && *at <= seat.now)
    {
        return Err("not-weekly-wall");
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
    if !me.default_home || !me.stored_login {
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
    let old_auth = who
        .login
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| seat.home.join(Cli::Codex.accounts().default_home))
        .join("auth.json");
    let left = remaining();
    if left.is_zero() {
        return Ok(None);
    }
    let Some(observed) = reader.read(&old_auth, source.expected_identity_email(), seat.now, left)
    else {
        return Ok(None);
    };
    if observed.identity != *expected_source {
        return Err("source-identity-changed");
    }
    if !weekly_exhausted(&observed) {
        return Err("not-weekly-exhausted");
    }
    if remaining().is_zero() {
        return Ok(None);
    }
    let mut session = session.clone();
    session.sample = Some(observed.usage.sample);
    session.point = Point::Wall {
        window: "seven_day".into(),
        at: Some(seat.now),
    };
    let mut selected = None;
    let mut admit = |row: &Row| {
        let left = remaining();
        if left.is_zero() {
            return false;
        }
        let new_auth = row_auth(seat, row);
        let Some(usage) = reader.read(&new_auth, row.expected_identity_email(), seat.now, left)
        else {
            return false;
        };
        if !usable(&usage) || usage.identity.account == expected_source.account {
            return false;
        }
        selected = Some((row.clone(), usage.identity, new_auth));
        true
    };
    let plan = select(seat, &session, &mut admit);
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
