// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Observations of the existing account decision, never another decision maker.

use super::*;
use aterm_agent::harness::account_progress::{
    AccountProgress, ProgressPhase, enrich_inventory_with_identity,
};

#[cfg(test)]
#[path = "progress/tests.rs"]
pub(super) mod lifecycle_tests;

fn observe_lock<T>(value: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, ()> {
    match value.try_lock() {
        Ok(held) => Ok(held),
        Err(std::sync::TryLockError::Poisoned(held)) => Ok(held.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => Err(()),
    }
}

fn observes(program: Option<Program>) -> bool {
    match program {
        Some(Program::Claude) => true,
        Some(
            Program::Codex
            | Program::Agy
            | Program::Muse
            | Program::Grok
            | Program::Kimi
            | Program::Generic,
        )
        | None => false,
    }
}

#[derive(Clone)]
pub(super) struct BoundProgress {
    snapshot: Snapshot,
    generation: Arc<Mutex<()>>,
    born: Option<u64>,
    state: Option<PathBuf>,
    labels: BTreeSet<String>,
    policy: Arc<()>,
    /// The exact gate recheck, independent of displayed wall time. Normal decisions have none.
    pub(super) execution_until: Option<Instant>,
    pub(super) value: AccountProgress,
}

#[derive(Default)]
pub(super) struct ProgressRead {
    pub(super) progress: Option<AccountProgress>,
    pub(super) activity_contended: bool,
    pub(super) execution_until: Option<Instant>,
    policy_revision: Option<u64>,
}

impl ProgressRead {
    pub(super) fn retry(&self) -> bool {
        self.activity_contended
            || self
                .progress
                .as_ref()
                .is_some_and(|value| moves_by_itself(&value.phase))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountExecutionVisibility",
            action = "Project",
            project = "harness_host::tests::account_advice::execution_transition"
        )
    )]
    pub(super) fn next_poll(&self, now: Instant) -> Option<Instant> {
        earlier(
            self.execution_until,
            self.retry().then(|| now + Duration::from_secs(1)),
        )
    }
}

fn earlier(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn expire_execution(value: &mut AccountProgress) {
    use aterm_agent::harness::account_progress::{
        EXECUTION_CONTEXT_UNREAD_FINDING, EXECUTION_HISTORY_PREFIX, EXECUTION_PREPARED_FINDING,
        EXECUTION_RECORDS_UNREAD_FINDING, EXECUTION_REFRESH_FINDING,
        EXECUTION_ROSTER_UNREAD_FINDING,
    };
    value.phase = ProgressPhase::Unknown;
    value.next_at = None;
    for detail in &mut value.details {
        if matches!(
            detail.as_str(),
            EXECUTION_PREPARED_FINDING
                | EXECUTION_ROSTER_UNREAD_FINDING
                | EXECUTION_CONTEXT_UNREAD_FINDING
                | EXECUTION_RECORDS_UNREAD_FINDING
        ) {
            *detail = format!("{EXECUTION_HISTORY_PREFIX}{detail}");
        }
    }
    value.details.insert(0, EXECUTION_REFRESH_FINDING.into());
}

fn pause_progress(value: &mut AccountProgress, off: bool, current_work: bool) {
    use aterm_agent::harness::account_progress::{
        EXECUTION_HISTORY_PREFIX, RECOVERY_HISTORY_PREFIX, RECOVERY_RESUME_FINDING,
        SESSION_AUTOPILOT_OFF_FINDING,
    };
    if !current_work {
        value.phase = ProgressPhase::Paused;
    }
    value.eligible = None;
    value.next_at = None;
    for detail in &mut value.details {
        if !detail.starts_with(EXECUTION_HISTORY_PREFIX)
            && !detail.starts_with(RECOVERY_HISTORY_PREFIX)
        {
            *detail = format!("{RECOVERY_HISTORY_PREFIX}{detail}");
        }
    }
    value.details.insert(
        0,
        if off {
            SESSION_AUTOPILOT_OFF_FINDING
        } else {
            RECOVERY_RESUME_FINDING
        }
        .into(),
    );
}

fn replace_execution_progress(kept: &mut Kept, next: Option<BoundProgress>, now: Instant) -> bool {
    let same = match (&kept.account.execution_progress, &next) {
        (Some(old), Some(new)) => {
            old.current(kept)
                && old.born == new.born
                && old.value == new.value
                && old.state == new.state
                && old.labels == new.labels
        }
        (None, None) => true,
        _ => false,
    };
    let deadline_news = match (&kept.account.execution_progress, &next) {
        (Some(old), Some(new)) => match (old.execution_until, new.execution_until) {
            (Some(old), Some(new)) => new < old || (old <= now && now < new),
            (None, Some(_)) => true,
            _ => false,
        },
        _ => false,
    };
    // A still-current lease may silently extend: its already-armed earlier wake is safe.
    // Reentering currentness or shortening a deadline needs a note even if wall words match.
    kept.account.execution_progress = next;
    !same || deadline_news
}

impl BoundProgress {
    pub(super) fn policy_is(&self, policy: &Arc<()>) -> bool {
        Arc::ptr_eq(&self.policy, policy)
    }

    /// Reuse only context already read by the gate; it is not an inventory judgment.
    pub(super) fn execution_context(&mut self, seat: &AccountSeat, roster: &accounts::Roster) {
        if self.born.is_some() {
            self.state = Some(seat.state.clone());
            self.labels = roster.rows.iter().map(|row| row.label.clone()).collect();
        }
    }

    fn current(&self, kept: &Kept) -> bool {
        Arc::ptr_eq(&self.generation, &kept.account.landing)
            && kept.snapshot.as_ref().is_some_and(|s| {
                account_process_matches(s, &self.snapshot) && s.session == self.snapshot.session
            })
    }

    fn matches(
        &self,
        kept: &Kept,
        pid: i32,
        born: Option<u64>,
        conversation: Option<&str>,
    ) -> bool {
        self.current(kept)
            && same_progress_process(
                self.snapshot.pid,
                self.born,
                self.snapshot.session.as_deref(),
                pid,
                born,
                conversation,
            )
    }

    fn matches_execution(
        &self,
        kept: &Kept,
        pid: i32,
        born: Option<u64>,
        conversation: Option<&str>,
    ) -> bool {
        self.matches(kept, pid, born, conversation)
            || (self.born.is_none()
                && born.is_none()
                && self.current(kept)
                && u32::try_from(pid).ok() == Some(self.snapshot.pid)
                && conversation.is_some()
                && self.snapshot.session.as_deref() == conversation)
    }
}

fn progress_phase(advice: Option<&AccountAdvice>, point: AccountPoint) -> ProgressPhase {
    match advice {
        Some(AccountAdvice::Probe { .. }) => ProgressPhase::Watching,
        Some(AccountAdvice::Move { .. } | AccountAdvice::Resume { .. }) => ProgressPhase::Ready,
        Some(AccountAdvice::Wait { .. } | AccountAdvice::Hold { .. }) => ProgressPhase::Waiting,
        Some(_) => ProgressPhase::Unknown,
        None if matches!(point, AccountPoint::Wall { kind } if kind.reads_limited()) => {
            ProgressPhase::Limited
        }
        None if point.at_wall() => ProgressPhase::Waiting,
        None => ProgressPhase::Watching,
    }
}

fn wall_name(point: AccountPoint) -> Option<&'static str> {
    let AccountPoint::Wall { kind } = point else {
        return None;
    };
    kind.reads_limited().then_some(match kind {
        aterm_phase::WallKind::UsageWeekly => "weekly",
        aterm_phase::WallKind::UsageSession => "session",
        aterm_phase::WallKind::ModelBucket { .. } => "model",
        aterm_phase::WallKind::Spend => "spend",
        _ => "rate",
    })
}

fn same_progress_process(
    pid: u32,
    born: Option<u64>,
    conversation: Option<&str>,
    asked_pid: i32,
    asked_born: Option<u64>,
    asked_conversation: Option<&str>,
) -> bool {
    u32::try_from(asked_pid).ok() == Some(pid)
        && born.is_some_and(|birth| Some(birth) == asked_born)
        && conversation.is_some()
        && conversation == asked_conversation
}

fn reason(word: &str) -> String {
    match word {
        "off" => "Automatic account rotation is off",
        "no-roster" => "No account roster is available",
        "not-listed" => "This process is not bound to a listed account",
        "other-home" => "This process uses a separate account home",
        "not-a-stored-login" => "This process does not use a stored login",
        "canary" => "Waiting for the selected account to answer",
        "no-candidate" | "no-fresh" => "No eligible alternate account",
        "breaker" => "Waiting for the account-switch limit to reset",
        "pinned" => "The selected account is pinned",
        "offer-expiring" => "Keeping an expiring reset offer",
        "nothing-due" => "No account switch is due",
        "probe: due" => "An account usage check is due",
        "jobs" => "Waiting until restart is safe",
        "person" => "Restart held or signal refused",
        "line" => "Account restart was refused",
        "unmeasured" => "Automatic switching has not been verified for this CLI build",
        "other-supervisor" => "Another supervisor owns account switching",
        "keyring-store" => "The CLI uses its own OS credential store",
        "no-line" => "The alternate account could not be prepared for restart",
        other => other,
    }
    .to_owned()
}

/// Do not round a positive remaining wait down into the current displayed second.
fn wait_deadline(now: i64, wait: Duration) -> Option<i64> {
    let seconds = wait
        .as_secs()
        .checked_add(u64::from(wait.subsec_nanos() != 0))?;
    now.checked_add(i64::try_from(seconds).ok()?)
}

/// Keep retained blockers first and append the process-local activity's own
/// timestamp, never turning a fresh UI read into evidence of a new attempt.
fn apply_login_activity(
    value: &mut AccountProgress,
    labels: &BTreeSet<String>,
    activity: Vec<aterm_agent::harness::accounts_sync::LoginActivity>,
    now: i64,
) {
    for entry in activity
        .into_iter()
        .filter(|entry| labels.contains(&entry.label))
    {
        if entry.observed_at > 0 && entry.observed_at <= now {
            value
                .details
                .push(format!("Local sign-in activity: {}", entry.label));
            value.details.push(format!(
                "Activity at {}",
                aterm_agent::harness::usage::rfc3339_utc(entry.observed_at),
            ));
        }
        if entry.active
            && !matches!(
                value.phase,
                ProgressPhase::Switching { .. } | ProgressPhase::Verifying { .. }
            )
        {
            value.phase = ProgressPhase::SigningIn {
                label: entry.label,
                route: entry.route,
            };
        }
        if let Some(next) = entry.next_at.filter(|next| *next > now) {
            value.next_at = Some(
                value
                    .next_at
                    .filter(|old| *old > now)
                    .map_or(next, |old| old.min(next)),
            );
        }
    }
}

impl WorkerIdle {
    /// Capture the existing publication fences without starting another advice calculation.
    pub(super) fn capture_execution_progress(&self) -> Option<BoundProgress> {
        if !observes(Some(self.agent)) {
            return None;
        }
        let (snapshot, generation, policy) = with_kept_of(&self.kept, |kept| {
            (
                kept.snapshot.clone(),
                Arc::clone(&kept.account.landing),
                Arc::clone(&kept.account.progress_policy),
            )
        });
        let snapshot = snapshot?;
        let born = snapshot.current_start_seconds();
        let now = aterm_time::unix_secs()
            .and_then(|v| i64::try_from(v).ok())
            .unwrap_or(0);
        Some(BoundProgress {
            snapshot,
            generation,
            born,
            state: None,
            labels: BTreeSet::new(),
            policy,
            execution_until: None,
            value: AccountProgress::unknown(now),
        })
    }

    /// Publish only the gate's actual result, or clear its last hold on admission.
    pub(super) fn publish_execution_progress(
        &self,
        captured: Option<BoundProgress>,
        until: Option<Instant>,
        reason: &'static str,
        account: Option<String>,
    ) {
        let Some(mut bound) = captured else {
            return;
        };
        bound.execution_until = until;
        if let Some(until) = until {
            bound.value.phase = ProgressPhase::Waiting;
            bound.value.account = bound.born.and(account);
            bound.value.details.push(
                if bound.born.is_some() {
                    reason
                } else {
                    aterm_agent::harness::account_progress::EXECUTION_CONTEXT_UNREAD_FINDING
                }
                .into(),
            );
            // Map this exact monotonic recheck into the display's whole seconds, rounding up.
            bound.value.next_at = aterm_time::unix_millis().and_then(|now| {
                let remaining = until.saturating_duration_since(Instant::now());
                let ms = i64::try_from(remaining.as_millis())
                    .ok()?
                    .checked_add(i64::from(
                        !remaining.subsec_nanos().is_multiple_of(1_000_000),
                    ))?;
                i64::try_from(now)
                    .ok()?
                    .checked_add(ms)?
                    .checked_add(999)
                    .map(|at| at / 1000)
            });
        }
        let changed = with_kept_of(&self.kept, |kept| {
            if !bound.current(kept) {
                return false;
            }
            let next = until.map(|_| bound);
            replace_execution_progress(kept, next, Instant::now())
        });
        if changed {
            (self.hooks.note)(HostNote::AccountProgress);
        }
    }

    /// Runs on the existing supervisor worker, after its actual account judgment.
    pub(super) fn read_account_progress(
        &self,
        request: &AdviceRequest,
        point: AccountPoint,
        advice: Option<&AccountAdvice>,
        judged: Option<&mut AccountJudgment>,
    ) -> Option<BoundProgress> {
        if !observes(Some(self.agent)) {
            return None;
        }
        let snapshot = request.snap.as_ref()?.clone();
        let born = Some(snapshot.current_start_seconds()?);
        let now = aterm_time::unix_secs()
            .and_then(|v| i64::try_from(v).ok())
            .unwrap_or(0);
        let mut value = AccountProgress::unknown(now);
        value.phase = progress_phase(advice, point);
        value.wall = wall_name(point).map(str::to_owned);
        if self.switches.accounts() == accounts::Mode::Off {
            value.phase = ProgressPhase::Disabled;
        }
        if let Some(AccountAdvice::Wait { until } | AccountAdvice::Hold { until }) = advice {
            value.next_at = wait_deadline(now, until.saturating_duration_since(Instant::now()));
        }
        if let Some(AccountAdvice::Move { to, .. }) = advice {
            value.details.push(format!("Selected alternate: {to}"));
        }
        if matches!(advice, Some(AccountAdvice::Resume { .. })) {
            value.wall = None;
            value
                .details
                .push("The limit was cleared; the session may continue".into());
        }
        if matches!(advice, Some(AccountAdvice::Probe { .. })) {
            value.details.push("Usage check requested".into());
        }
        let stay = with_kept_of(&self.kept, |k| k.account.stay.or(k.account.held));
        if let Some(why) = stay.filter(|why| *why != "off") {
            value.details.push(reason(why));
        }
        let mut bound = BoundProgress {
            snapshot,
            generation: request.turn.generation.clone(),
            born,
            state: None,
            labels: BTreeSet::new(),
            policy: Arc::clone(&request.turn.progress_policy),
            execution_until: None,
            value,
        };
        let Some(seat) = (self.hooks.acts.account_seat)() else {
            bound
                .value
                .details
                .push("Account state is unavailable".into());
            return Some(bound);
        };
        bound.value.observed_at = seat.now;
        bound.state = Some(seat.state.clone());
        let roster = match roster_read(&seat) {
            Ok(roster) => roster,
            Err(why) => {
                bound.value.details.push(
                    match why {
                        RosterUnread::Nothing => "No accounts are listed",
                        RosterUnread::Refused => "The account roster could not be read",
                        RosterUnread::LoginsUnread => "Stored logins could not be read",
                    }
                    .into(),
                );
                return Some(bound);
            }
        };
        bound.labels = roster.rows.iter().map(|r| r.label.clone()).collect();
        bound.value.known = Some(roster.rows.len());
        let Some(who) = exec_login(&self.hooks, &bound.snapshot) else {
            bound
                .value
                .details
                .push("This process's account identity is unavailable".into());
            return Some(bound);
        };
        let me = who.me(&roster);
        bound.value.account = me.label().map(str::to_owned);
        let store = accounts::Store::at(&seat.state).bounded_by(seat.ladder);
        let (Ok((state, None)), Ok((logins, None))) =
            (store.load(seat.now), store.load_logins(seat.now))
        else {
            bound
                .value
                .details
                .push("Account records could not be read consistently".into());
            return Some(bound);
        };
        let (point, mut identities) = match judged {
            Some(judged)
                if judged.roster == roster && judged.identities.seat.context().matches(&seat) =>
            {
                (Some(&judged.point), Some(&mut judged.identities))
            }
            Some(_) => {
                bound
                    .value
                    .details
                    .push(aterm_agent::harness::account_progress::BINDINGS_CHANGED_FINDING.into());
                (None, None)
            }
            None => (None, None),
        };
        enrich_inventory_with_identity(
            &mut bound.value,
            &roster,
            &state,
            &logins,
            &accounts::enrolled(&roster),
            &me,
            point,
            |label| {
                identities.as_mut().is_none_or(|identities| {
                    roster.row(label).is_some_and(|row| identities.matches(row))
                })
            },
        );
        if !me.default_home || !me.stored_login || me.label().is_none() {
            bound.value.eligible = None;
        }
        Some(bound)
    }
}

impl HostHandle {
    fn progress_policy(&self, sid: &str) -> Result<(bool, u64), ()> {
        let state = observe_lock(&self.shared.state)?;
        Ok((state.off.contains(sid), state.off_revision))
    }

    fn progress_policy_current(&self, revision: Option<u64>) -> bool {
        revision.is_none_or(|revision| {
            observe_lock(&self.shared.state).is_ok_and(|state| state.off_revision == revision)
        })
    }

    fn unobserved_progress(off: bool, policy_revision: u64) -> ProgressRead {
        let mut progress = off.then(|| AccountProgress::unknown(0));
        if let Some(progress) = &mut progress {
            pause_progress(progress, true, false);
        }
        ProgressRead {
            progress,
            policy_revision: Some(policy_revision),
            ..Default::default()
        }
    }

    /// Pure in-memory read. Contention is distinct from an absent observation.
    #[cfg(test)]
    pub(crate) fn account_progress(
        &self,
        sid: &str,
        pid: i32,
        started: Option<u64>,
        conversation: Option<&str>,
    ) -> Result<Option<AccountProgress>, ()> {
        self.account_progress_read(sid, pid, started, conversation)
            .map(|read| read.progress)
    }

    fn account_progress_read(
        &self,
        sid: &str,
        pid: i32,
        started: Option<u64>,
        conversation: Option<&str>,
    ) -> Result<ProgressRead, ()> {
        self.account_progress_read_with_activity(
            sid,
            pid,
            started,
            conversation,
            || {
                (
                    Instant::now(),
                    aterm_time::unix_secs().and_then(|v| i64::try_from(v).ok()),
                )
            },
            aterm_agent::harness::accounts_sync::login_activity,
        )
    }

    /// Keep display expiry and activity inputs controllable without changing the observed gate.
    #[cfg(test)]
    pub(super) fn account_progress_with_activity(
        &self,
        sid: &str,
        pid: i32,
        started: Option<u64>,
        conversation: Option<&str>,
        clock: impl FnOnce() -> (Instant, Option<i64>),
        activity: impl FnOnce(
            &accounts::Store,
            accounts::Cli,
            i64,
        )
            -> Option<Vec<aterm_agent::harness::accounts_sync::LoginActivity>>,
    ) -> Result<Option<AccountProgress>, ()> {
        self.account_progress_read_with_activity(sid, pid, started, conversation, clock, activity)
            .map(|read| read.progress)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountExecutionVisibility",
            action = "Elapse",
            reason = "The native fixture advances the injected monotonic display clock through the gate's actual retained deadline"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountExecutionVisibility",
            action = "Project",
            project = "harness_host::tests::account_advice::execution_transition"
        )
    )]
    pub(super) fn account_progress_read_with_activity(
        &self,
        sid: &str,
        pid: i32,
        started: Option<u64>,
        conversation: Option<&str>,
        clock: impl FnOnce() -> (Instant, Option<i64>),
        activity: impl FnOnce(
            &accounts::Store,
            accounts::Cli,
            i64,
        )
            -> Option<Vec<aterm_agent::harness::accounts_sync::LoginActivity>>,
    ) -> Result<ProgressRead, ()> {
        // Sample before cloning any lease. A concurrent silent extension then either appears
        // in this read or leaves its earlier deadline armed, even if this read resumes late.
        let (mono, wall) = clock();
        // This is the host's actual OffSessions verdict, not a missing-worker heuristic.
        // Its guard is released before touching any session observation.
        let (off, policy_revision) = self.progress_policy(sid)?;
        let map = observe_lock(&self.shared.kept)?;
        let Some(kept) = map.get(sid).cloned() else {
            return Ok(Self::unobserved_progress(off, policy_revision));
        };
        drop(map);
        let kept = observe_lock(&kept)?;
        let execution = kept
            .account
            .execution_progress
            .as_ref()
            .filter(|bound| bound.matches_execution(&kept, pid, started, conversation));
        let normal = kept
            .account
            .progress
            .as_ref()
            .filter(|bound| bound.matches(&kept, pid, started, conversation));
        let Some(bound) = execution.or(normal) else {
            return Ok(Self::unobserved_progress(off, policy_revision));
        };
        let policy_retired = !bound.policy_is(&kept.account.progress_policy);
        let execution_active = execution.is_some();
        let execution_until = execution.and_then(|bound| bound.execution_until);
        let mut value = bound.value.clone();
        // Prefer the gate's own roster; use a current decision only if that gate read failed.
        let activity_context = if bound.born.is_some() && bound.state.is_none() {
            normal
                .filter(|normal| normal.state.is_some())
                .unwrap_or(bound)
        } else {
            bound
        };
        let state = activity_context.state.clone();
        let labels = activity_context.labels.clone();
        let mut current_work = false;
        if bound.born.is_some() {
            if let Some(flying) = kept.account.flying.as_ref().filter(|flying| {
                flying.pid == bound.snapshot.pid && flying.start == bound.snapshot.start
            }) {
                current_work = true;
                if flying.rejected.is_some() {
                    value.phase = ProgressPhase::Waiting;
                    value.details.insert(
                        0,
                        "Account restart was refused; awaiting selection rollback".into(),
                    );
                } else {
                    value.phase = ProgressPhase::Switching {
                        to: flying.to.clone(),
                    };
                }
            } else if let Some(moved) = kept.account.moved.as_ref().filter(|moved| {
                account_process_matches(&moved.process, &bound.snapshot) && !moved.answered
            }) {
                current_work = true;
                value.phase = ProgressPhase::Verifying {
                    to: moved.to.clone(),
                };
            } else if kept.account.probing.as_ref().is_some_and(|probe| {
                probe.pid == bound.snapshot.pid && probe.start == bound.snapshot.start
            }) {
                current_work = true;
                value.phase = ProgressPhase::Probing;
            } else if let Some(held) = kept
                .account
                .held
                .filter(|why| *why != "off" && !execution_active)
            {
                value.phase = ProgressPhase::Waiting;
                value.details.insert(0, reason(held));
            }
        }
        drop(kept);
        // Currentness is monotonic; the wall clock is only for displayed timestamps and activity.
        let mut execution_until = execution_until.filter(|until| mono < *until);
        if execution_active && execution_until.is_none() {
            let phase = value.phase.clone();
            expire_execution(&mut value);
            if current_work {
                value.phase = phase;
            }
        }
        let now = wall.unwrap_or(value.observed_at);
        // A normal retained decision can outlive its wait. Never present a past wall deadline
        // as the next event; an execution receipt's currentness already used the monotonic clock.
        value.next_at = value.next_at.filter(|at| *at > now);
        let mut activity_contended = false;
        if let Some(state) = state {
            let gate_deadline = execution_until.is_some().then_some(value.next_at);
            let activity = activity(&accounts::Store::at(&state), accounts::Cli::Claude, now);
            match activity {
                Some(activity) => apply_login_activity(&mut value, &labels, activity, now),
                None if execution_active => activity_contended = true,
                None => return Err(()),
            }
            if let Some(deadline) = gate_deadline {
                value.next_at = deadline;
            }
        }
        if self.shared.switches.accounts() == accounts::Mode::Off && execution_active {
            value
                .details
                .push("Automatic account rotation is off".into());
        } else if self.shared.switches.accounts() == accounts::Mode::Off
            && !matches!(
                value.phase,
                ProgressPhase::Switching { .. }
                    | ProgressPhase::Verifying { .. }
                    | ProgressPhase::SigningIn { .. }
            )
        {
            value.phase = ProgressPhase::Disabled;
        } else if value.phase == ProgressPhase::Disabled {
            value.phase = ProgressPhase::Unknown;
            value.details.insert(
                0,
                "Waiting for a decision under the current account policy".into(),
            );
        }
        if !execution_active && (off || policy_retired) {
            // A separately observed operation may still finish while an Off worker yields.
            // Its phase is evidence; another selection or gate retry is not promised.
            let current_work =
                current_work || matches!(value.phase, ProgressPhase::SigningIn { .. });
            pause_progress(&mut value, off, current_work);
            execution_until = None;
        }
        Ok(ProgressRead {
            progress: Some(value),
            activity_contended,
            execution_until,
            policy_revision: Some(policy_revision),
        })
    }
}

/// Whether an observation in `phase` moves on with no note to the window: a restart in flight, its
/// check, a probe or a sign-in, whose steps the host takes on its own threads. Their start and
/// every settled phase come with a [`HostNote::AccountProgress`](super::HostNote::AccountProgress).
fn moves_by_itself(phase: &ProgressPhase) -> bool {
    matches!(
        phase,
        ProgressPhase::Switching { .. }
            | ProgressPhase::Verifying { .. }
            | ProgressPhase::Probing
            | ProgressPhase::SigningIn { .. }
    )
}

impl crate::App {
    /// Poll only immutable supervisor observations; never read an account file on the GUI thread.
    ///
    /// Each loop wake reads them, a [`HostNote::AccountProgress`](super::HostNote::AccountProgress)
    /// among them. Active work or contention polls a second on; a current execution receipt also
    /// arms one wake at its exact gate recheck so an unrefreshed refusal becomes history. A settled
    /// expired receipt arms no timer, preserving the idle window's zero timer wakes. An unavailable
    /// supervisor read marks retained facts historical until a same-binding read succeeds; it
    /// retains the one-second observation retry without inventing a semantic next event.
    pub(crate) fn service_harness_account_progress(&mut self, now: Instant) {
        self.service_harness_account_progress_with_reader(
            now,
            |host, sid, pid, birth, conversation| {
                host.account_progress_read(sid, pid, birth, conversation)
            },
        );
    }

    pub(super) fn service_harness_account_progress_with_reader(
        &mut self,
        now: Instant,
        mut read: impl FnMut(
            &HostHandle,
            &str,
            i32,
            Option<u64>,
            Option<&str>,
        ) -> Result<ProgressRead, ()>,
    ) {
        if self.harness_progress_next_poll.is_some_and(|at| at > now) {
            return;
        }
        let Some(host) = self.harness.clone() else {
            self.harness_progress_next_poll = None;
            return;
        };
        let sessions: Vec<_> = self
            .pool
            .iter()
            .map(|entry| (entry.id, entry.ctx.timeline.clone()))
            .collect();
        let mut again = false;
        let mut next_poll = None;
        let mut changed = Vec::new();
        for (id, timeline) in sessions {
            let Ok(held) = observe_lock(&timeline) else {
                again = true;
                continue;
            };
            if !observes(identified_agent(held.agent())) {
                continue;
            }
            let Some((pid, birth)) = held.claude_footer_identity() else {
                continue;
            };
            let conversation = held
                .claude_footer()
                .and_then(|f| f.owner.as_ref())
                .map(|o| o.session_id.clone());
            drop(held);
            let Some(sid) = self.session_sid(id) else {
                continue;
            };
            let mut read = read(&host, &sid, pid, birth, conversation.as_deref());
            if read
                .as_ref()
                .is_ok_and(|read| !host.progress_policy_current(read.policy_revision))
            {
                read = Err(());
            }
            match &read {
                Ok(read) => next_poll = earlier(next_poll, read.next_poll(now)),
                Err(()) => again = true,
            }
            let Ok(mut held) = observe_lock(&timeline) else {
                again = true;
                continue;
            };
            let updated = match read {
                Ok(read) => {
                    held.set_account_progress(pid, birth, conversation.as_deref(), read.progress)
                }
                Err(()) => held.set_account_progress_busy(pid, birth, conversation.as_deref()),
            };
            if updated {
                changed.push(id);
            }
        }
        self.harness_progress_next_poll =
            earlier(next_poll, again.then(|| now + Duration::from_secs(1)));
        let lab_changed = !changed.is_empty() && self.lab_page_shown();
        for id in changed {
            self.on_claude_footer_changed(id);
        }
        if lab_changed {
            self.publish_native_lab_state();
        }
    }

    /// The host said some tab's account progress may have moved: read every tab's now, past a poll
    /// already armed.
    pub(crate) fn account_progress_noted(&mut self, now: Instant) {
        self.harness_progress_next_poll = None;
        self.service_harness_account_progress(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_and_context_failures_do_not_become_quota_exhaustion() {
        assert!(
            wall_name(AccountPoint::Wall {
                kind: aterm_phase::WallKind::UsageWeekly
            })
            .is_some()
        );
        for kind in [
            aterm_phase::WallKind::Auth,
            aterm_phase::WallKind::Context,
            aterm_phase::WallKind::Memory,
            aterm_phase::WallKind::Overloaded,
        ] {
            let point = AccountPoint::Wall { kind };
            assert_eq!(wall_name(point), None);
            assert_eq!(progress_phase(None, point), ProgressPhase::Waiting);
        }
    }

    #[test]
    fn a_plan_to_move_is_not_a_report_that_a_move_started() {
        assert_eq!(
            progress_phase(
                Some(&AccountAdvice::Move {
                    to: "other".into(),
                    yields: None
                }),
                AccountPoint::TurnEnd
            ),
            ProgressPhase::Ready
        );
        assert_eq!(
            progress_phase(
                Some(&AccountAdvice::Probe { otherwise: None }),
                AccountPoint::TurnEnd
            ),
            ProgressPhase::Watching
        );
        assert_eq!(
            progress_phase(None, AccountPoint::LoginEnded),
            ProgressPhase::Waiting
        );
        assert_eq!(
            progress_phase(None, AccountPoint::TurnEnd),
            ProgressPhase::Watching
        );
    }

    #[test]
    fn a_progress_observation_cannot_cross_a_process_or_conversation() {
        assert!(same_progress_process(
            42,
            Some(100),
            Some("session"),
            42,
            Some(100),
            Some("session")
        ));
        for (pid, born, session) in [
            (43, Some(100), Some("session")),
            (42, Some(101), Some("session")),
            (42, None, Some("session")),
            (42, Some(100), Some("other")),
            (42, Some(100), None),
        ] {
            assert!(!same_progress_process(
                42,
                Some(100),
                Some("session"),
                pid,
                born,
                session
            ));
        }
        assert!(!same_progress_process(
            42,
            None,
            Some("session"),
            42,
            None,
            Some("session")
        ));
    }
}
