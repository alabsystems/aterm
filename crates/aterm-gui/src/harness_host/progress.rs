// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Observations of the existing account decision, never another decision maker.

use super::*;
use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase, enrich_inventory};

#[cfg(test)]
#[path = "progress/tests.rs"]
mod lifecycle_tests;

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
    pub(super) value: AccountProgress,
}

fn progress_phase(advice: Option<&AccountAdvice>, point: AccountPoint) -> ProgressPhase {
    match advice {
        Some(AccountAdvice::Probe { .. }) => ProgressPhase::Watching,
        Some(AccountAdvice::Move { .. } | AccountAdvice::Resume { .. }) => ProgressPhase::Ready,
        Some(AccountAdvice::Wait { .. }) => ProgressPhase::Waiting,
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
        "unmeasured" => "Automatic switching has not been verified for this CLI build",
        "other-supervisor" => "Another supervisor owns account switching",
        "keyring-store" => "The CLI uses its own OS credential store",
        "no-line" => "The alternate account could not be prepared for restart",
        other => other,
    }
    .to_owned()
}

impl WorkerIdle {
    /// Runs on the existing supervisor worker, after its actual account judgment.
    pub(super) fn read_account_progress(
        &self,
        request: &AdviceRequest,
        point: AccountPoint,
        advice: Option<&AccountAdvice>,
        judged: Option<&accounts::Point>,
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
        if let Some(AccountAdvice::Wait { until }) = advice {
            value.next_at =
                i64::try_from(until.saturating_duration_since(Instant::now()).as_secs())
                    .ok()
                    .and_then(|wait| now.checked_add(wait));
        }
        if let Some(AccountAdvice::Move { to }) = advice {
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
        enrich_inventory(
            &mut bound.value,
            &roster,
            &state,
            &logins,
            &accounts::enrolled(&roster),
            &me,
            judged,
        );
        if !me.default_home || !me.stored_login || me.label().is_none() {
            bound.value.eligible = None;
        }
        Some(bound)
    }
}

impl HostHandle {
    /// Pure in-memory read. Contention is distinct from an absent observation.
    pub(crate) fn account_progress(
        &self,
        sid: &str,
        pid: i32,
        started: Option<u64>,
        conversation: Option<&str>,
    ) -> Result<Option<AccountProgress>, ()> {
        let map = observe_lock(&self.shared.kept)?;
        let Some(kept) = map.get(sid).cloned() else {
            return Ok(None);
        };
        drop(map);
        let kept = observe_lock(&kept)?;
        let Some(bound) = &kept.account.progress else {
            return Ok(None);
        };
        if !Arc::ptr_eq(&bound.generation, &kept.account.landing)
            || !same_progress_process(
                bound.snapshot.pid,
                bound.born,
                bound.snapshot.session.as_deref(),
                pid,
                started,
                conversation,
            )
            || !kept.snapshot.as_ref().is_some_and(|s| {
                account_process_matches(s, &bound.snapshot) && s.session == bound.snapshot.session
            })
        {
            return Ok(None);
        }
        let mut value = bound.value.clone();
        if let Some(flying) = &kept.account.flying {
            if flying.pid == bound.snapshot.pid && flying.start == bound.snapshot.start {
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
            }
        } else if let Some(moved) = &kept.account.moved {
            if account_process_matches(&moved.process, &bound.snapshot) && !moved.answered {
                value.phase = ProgressPhase::Verifying {
                    to: moved.to.clone(),
                };
            }
        } else if kept.account.probing.as_ref().is_some_and(|probe| {
            probe.pid == bound.snapshot.pid && probe.start == bound.snapshot.start
        }) {
            value.phase = ProgressPhase::Probing;
        } else if let Some(held) = kept.account.held.filter(|why| *why != "off") {
            value.phase = ProgressPhase::Waiting;
            value.details.insert(0, reason(held));
        }
        let state = bound.state.clone();
        let labels = bound.labels.clone();
        drop(kept);
        if let Some(state) = state {
            let now = aterm_time::unix_secs()
                .and_then(|v| i64::try_from(v).ok())
                .unwrap_or(value.observed_at);
            let activity = aterm_agent::harness::accounts_sync::login_activity(
                &accounts::Store::at(&state),
                accounts::Cli::Claude,
                now,
            )
            .ok_or(())?;
            for entry in activity
                .into_iter()
                .filter(|entry| labels.contains(&entry.label))
            {
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
        if self.shared.switches.accounts() == accounts::Mode::Off
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
        Ok(Some(value))
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
    /// among them, and the window arms a wake of its own a second on only while a read was
    /// contended or an observation moves by itself ([`moves_by_itself`]). A Claude tab at rest arms
    /// no timer: `lab forever`'s END holds an idle window to `timer_wakes=0`.
    pub(crate) fn service_harness_account_progress(&mut self, now: Instant) {
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
        let mut changed = Vec::new();
        for (id, timeline) in sessions {
            let Ok(held) = observe_lock(&timeline) else {
                again = true;
                continue;
            };
            if !observes(
                held.agent()
                    .program
                    .as_deref()
                    .and_then(aterm_phase::program_of),
            ) {
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
            let Ok(progress) = host.account_progress(&sid, pid, birth, conversation.as_deref())
            else {
                again = true;
                continue;
            };
            again |= progress.as_ref().is_some_and(|p| moves_by_itself(&p.phase));
            let Ok(mut held) = observe_lock(&timeline) else {
                again = true;
                continue;
            };
            if held.set_account_progress(pid, birth, conversation.as_deref(), progress) {
                changed.push(id);
            }
        }
        self.harness_progress_next_poll = again.then(|| now + Duration::from_secs(1));
        for id in changed {
            self.on_claude_footer_changed(id);
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
                Some(&AccountAdvice::Move { to: "other".into() }),
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
