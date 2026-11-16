// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;
use std::collections::HashMap;

use super::*;

struct CountedMap<'a, T> {
    entries: HashMap<u64, T>,
    visits: &'a Cell<u64>,
}

impl<'a, T> CountedMap<'a, T> {
    fn iter(&self) -> impl Iterator<Item = (&u64, &T)> {
        self.entries
            .iter()
            .inspect(|_| self.visits.set(self.visits.get() + 1))
    }

    fn values(&self) -> impl Iterator<Item = &T> {
        self.entries
            .values()
            .inspect(|_| self.visits.set(self.visits.get() + 1))
    }

    fn get(&self, id: &u64) -> Option<&T> {
        self.visits.set(self.visits.get() + 1);
        self.entries.get(id)
    }
}

struct Baseline<'a> {
    min_interval: Duration,
    sessions: CountedMap<'a, &'a schedule::ScheduledStatus>,
    agents: CountedMap<'a, &'a AgentWatch>,
    rollout_asked: CountedMap<'a, &'a crate::codex_rollout_tap::RolloutAsk>,
}

impl<'a> Baseline<'a> {
    fn of(observer: &'a StatusObserver, visits: &'a Cell<u64>, only: Option<u64>) -> Self {
        let mut sessions = HashMap::new();
        let mut agents = HashMap::new();
        let mut rollout_asked = HashMap::new();
        for (id, record) in observer
            .sessions
            .records()
            .filter(|(id, _)| only.is_none_or(|one| one == **id))
        {
            if let Some(slot) = &record.classified {
                sessions.insert(*id, slot);
            }
            if let Some(watch) = &record.agent {
                agents.insert(*id, watch);
            }
            if let Some(ask) = record.rollout.as_deref() {
                rollout_asked.insert(*id, ask);
            }
        }
        Self {
            min_interval: observer.sessions.min_interval(),
            sessions: CountedMap {
                entries: sessions,
                visits,
            },
            agents: CountedMap {
                entries: agents,
                visits,
            },
            rollout_asked: CountedMap {
                entries: rollout_asked,
                visits,
            },
        }
    }

    // Immutable production baseline: 63aee4d3bf1fc2d3e8eb51d9a6e92e6a6e21f10b,
    // session_status.rs::StatusObserver::next_wake. Counting maps measure the original visits.
    pub(crate) fn next_wake(&self) -> Option<Instant> {
        let agent_wake = self
            .agents
            .iter()
            .filter_map(|(id, w)| {
                let slot = self.sessions.get(id)?;
                let followup = w.followup.then(|| {
                    w.output_at.map_or(slot.next_due, |at| {
                        at.checked_add(self.min_interval)
                            .unwrap_or(at)
                            .max(slot.next_due)
                    })
                });
                let unresolved = if w.pgid > 0 && w.program_seen == Some(None) {
                    w.resolved_at.map(|at| {
                        (at + unresolved_program_recheck(w.resolve_attempts)).max(slot.next_due)
                    })
                } else {
                    None
                };
                let confirmation = w
                    .deferred_name_recheck_at
                    .filter(|_| w.pgid > 0)
                    .map(|at| at.max(slot.next_due));
                let build_wait = w
                    .resolved_at
                    .filter(|_| w.build_waiting && w.pgid > 0)
                    .map(|at| (at + PROGRAM_RECHECK).max(slot.next_due));
                let settle = w
                    .rest_at
                    .map(|at| (at + AGENT_MIN_INTERVAL).max(slot.next_due));
                let kimi = w.kimi_again.then(|| {
                    w.read_at.map_or(slot.next_due, |at| {
                        (at + AGENT_MIN_INTERVAL).max(slot.next_due)
                    })
                });
                let unread = (w.kimi && w.input.look.is_some_and(|look| look.known && !look.read))
                    .then_some(slot.next_due);
                [
                    followup,
                    unresolved,
                    confirmation,
                    build_wait,
                    settle,
                    kimi,
                    unread,
                ]
                .into_iter()
                .flatten()
                .min()
            })
            .min();
        let rollout_wake = self
            .rollout_asked
            .iter()
            .filter_map(|(id, ask)| {
                let slot = self.sessions.get(id)?;
                ask.wake().map(|at| at.max(slot.next_due))
            })
            .min();
        let owed = self
            .sessions
            .values()
            .filter_map(|slot| {
                // Clamp to `next_due`.
                slot.fsm.owed_wake().map(|owed| owed.max(slot.next_due))
            })
            .min();
        [owed, agent_wake, rollout_wake].into_iter().flatten().min()
    }
}

fn equivalent(observer: &StatusObserver) {
    let visits = Cell::new(0);
    let baseline = Baseline::of(observer, &visits, None);
    let expected = baseline.next_wake();
    observer.sessions.reset_reads();
    assert_eq!(observer.next_wake(), expected);
    assert_eq!(
        observer.sessions.reads(),
        0,
        "the index head reads no status, watch or ask"
    );
    let mut keys = observer
        .sessions
        .records()
        .filter_map(|(id, _)| {
            Baseline::of(observer, &visits, Some(*id))
                .next_wake()
                .map(|at| (at, *id))
        })
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        observer.sessions.transition_keys(),
        keys,
        "all owned deadlines, including those behind the head"
    );
    let (records, observations, transitions) = observer.sessions.index_sizes();
    assert_eq!(observations, baseline.sessions.entries.len());
    assert!(transitions <= observations && observations <= records);
}

fn idle() -> Evidence {
    Evidence {
        shell: Some(ShellEvidence::Prompt),
        completed_block: None,
        lifecycle: None,
        foreground_job: None,
        activity: ActivitySample {
            alt_screen: false,
            screen: 1,
            last_output: None,
        },
    }
}

fn quiet_observer(
    ids: &[u64],
    policy: StatusPolicy,
    interval: Duration,
    seed: Instant,
) -> (StatusObserver, Instant) {
    let mut observer = StatusObserver::new(policy, interval);
    for id in ids {
        observer.observe(*id, &idle(), seed);
    }
    let settled = seed + policy.dwell;
    for id in ids {
        observer.observe(*id, &idle(), settled);
        assert_eq!(
            observer.status(*id).map(|status| status.phase),
            Some(Phase::Idle)
        );
    }
    assert_eq!(
        observer.next_wake(),
        None,
        "the initial Starting dwell is discharged"
    );
    (observer, settled)
}

fn target() -> crate::self_claim::TapTarget {
    crate::self_claim::RolloutTarget {
        revision: 1,
        pid: 4242,
        start: "Mon Oct 20 16:37:58 2026".to_owned(),
        thread: "019a0000-0000-7000-8000-00000000000a".to_owned(),
        home: std::path::PathBuf::from("/private/tmp/x/codex-home"),
    }
    .into()
}

#[test]
fn two_hundred_quiet_app_sessions_have_no_transition_wake_or_unrelated_visits() {
    const COUNT: u64 = 200;
    let mut app = crate::App::headless_for_test();
    let interval = Duration::from_secs(1);
    app.session_status = StatusObserver::new(StatusPolicy::default(), interval);
    for id in 1..COUNT {
        app.pool.insert(crate::stub_session(id));
    }
    for session in app.pool.iter() {
        session
            .term
            .lock()
            .unwrap()
            .process(b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\");
    }
    let t0 = Instant::now();
    app.observe_session_statuses(t0);
    app.observe_session_statuses(t0 + interval * 2);
    for id in 0..COUNT {
        assert_eq!(
            app.session_status.status(id).map(|status| status.phase),
            Some(Phase::Idle)
        );
        app.session_status.note_input(
            id,
            aterm_session::sink::InputLook::for_testing(id, 0, true),
            0,
        );
    }
    let visits = Cell::new(0);
    let baseline = Baseline::of(&app.session_status, &visits, None);
    assert_eq!(baseline.sessions.entries.len(), COUNT as usize);
    assert_eq!(baseline.agents.entries.len(), COUNT as usize);
    let sparse = |wake: Option<Instant>, visits: u64| wake.is_none() && visits == 0;
    let old_wake = baseline.next_wake();
    assert_eq!(
        old_wake, None,
        "quiet sessions owe no transition, though observations remain eligible"
    );
    assert!(
        !sparse(old_wake, visits.get()),
        "the original production baseline must fail the sparse guard"
    );
    assert_eq!(
        visits.get(),
        COUNT * 3,
        "watch iteration, classifier joins and classifier iteration"
    );
    app.session_status.sessions.reset_reads();
    for _ in 0..100 {
        assert!(sparse(
            app.session_status.next_wake(),
            app.session_status.sessions.reads()
        ));
    }
    assert_eq!(
        app.session_status.sessions.index_sizes(),
        (COUNT as usize, COUNT as usize, 0)
    );
}

#[test]
fn every_agent_clock_and_metadata_mutation_matches_the_original_deadline() {
    let interval = Duration::from_millis(50);
    let (mut observer, t0) = quiet_observer(
        &[1, 2, 3, 4, 5, 6, 7, 8, 9],
        StatusPolicy::default(),
        interval,
        Instant::now(),
    );
    for id in 1..=9 {
        observer.sessions.with_agent(id, |watch| {
            watch.pgid = 42;
            match id {
                1 => watch.followup = true,
                2 => {
                    watch.followup = true;
                    watch.output_at = Some(t0 + interval);
                }
                3 => {
                    watch.program_seen = Some(None);
                    watch.resolved_at = Some(t0);
                    watch.resolve_attempts = 2;
                }
                4 => watch.deferred_name_recheck_at = Some(t0 + interval * 3),
                5 => {
                    watch.build_waiting = true;
                    watch.resolved_at = Some(t0 + interval);
                }
                6 => watch.rest_at = Some(t0 + interval),
                7 => {
                    watch.kimi_again = true;
                    watch.read_at = Some(t0);
                }
                8 => watch.kimi_again = true,
                9 => {
                    watch.kimi = true;
                    watch.input.look =
                        Some(aterm_session::sink::InputLook::for_testing(id, 1, false));
                }
                _ => unreachable!(),
            }
        });
        equivalent(&observer);
    }
    for id in 1..=9 {
        let before = observer.next_wake();
        observer.set_detail(id, Some("build".to_owned()));
        observer.note_seen(id);
        observer.take_status_transition(id);
        observer.screen_print_for(id, (1, 1), t0, || 42);
        assert_eq!(
            observer.next_wake(),
            before,
            "metadata alone must not move a clock"
        );
        observer
            .sessions
            .reschedule(id, t0 + interval * (20 + u32::try_from(id).unwrap()));
        equivalent(&observer);
    }
    for id in 1..=9 {
        observer.retire(id);
        equivalent(&observer);
    }
    assert_eq!(observer.sessions.index_sizes(), (0, 0, 0));
}

#[test]
fn rollout_pair_and_settle_clocks_are_owned_per_session_and_clamped_after_reschedule() {
    use crate::codex_rollout_tap::{Ask, Look, RECHECK};
    use crate::session_log::transcript::ReadWait;
    let (mut observer, t0) = quiet_observer(
        &[7, 8],
        StatusPolicy::default(),
        Duration::from_millis(50),
        Instant::now(),
    );
    for id in [7, 8] {
        assert_eq!(
            observer.rollout_action(
                id,
                Some(target()),
                Some(Look {
                    screen: crate::control::ScreenGen { epoch: 1, seq: id },
                    counted: true
                }),
                (t0, None)
            ),
            Ask::Read(target())
        );
        equivalent(&observer);
    }
    let wait = ReadWait {
        since: 100,
        read_ms: 0,
    };
    observer.note_pair_wait(7, Some((wait, t0 + RECHECK / 2)));
    equivalent(&observer);
    observer.sessions.reschedule(7, t0 + RECHECK * 3);
    equivalent(&observer);
    let other = observer.sessions.rollout(&8).copied();
    observer.note_pair_wait(7, None);
    assert_eq!(observer.sessions.rollout(&8).copied(), other);
    equivalent(&observer);
    observer.retire(8);
    equivalent(&observer);
    assert_eq!(
        observer.rollout_action(7, None, None, (t0 + RECHECK, None)),
        Ask::Stop
    );
    equivalent(&observer);
    assert_eq!(observer.next_wake(), None);
    observer.retire(7);
    let reborn = t0 + RECHECK * 4;
    observer.observe(7, &idle(), reborn);
    assert_eq!(
        observer.next_wake(),
        Some(reborn + StatusPolicy::default().dwell)
    );
    equivalent(&observer);
    observer.observe(7, &idle(), reborn + StatusPolicy::default().dwell);
    assert_eq!(
        observer.next_wake(),
        None,
        "reusing an id cannot inherit its old rollout"
    );
    equivalent(&observer);
}

#[test]
fn dwell_quiet_retirement_contention_and_policy_changes_keep_the_existing_minimum() {
    let policy = StatusPolicy {
        dwell: Duration::from_millis(100),
        quiet_after: Duration::from_secs(2),
    };
    let (mut observer, t0) =
        quiet_observer(&[1], policy, Duration::from_millis(50), Instant::now());
    let mut running = idle();
    running.shell = Some(ShellEvidence::Executing);
    running.activity.screen = 2;
    running.activity.last_output = Some(t0);
    observer.observe(1, &running, t0);
    assert_eq!(observer.next_wake(), Some(t0 + policy.dwell));
    equivalent(&observer);
    observer.observe(1, &running, t0 + policy.dwell);
    assert_eq!(
        observer.status(1).map(|status| status.phase),
        Some(Phase::Running)
    );
    equivalent(&observer);
    assert_eq!(observer.next_wake(), Some(t0 + policy.quiet_after));
    observer.note_skipped(1, t0 + policy.quiet_after);
    equivalent(&observer);
    let at = t0 + policy.quiet_after + Duration::from_secs(1);
    observer.observe(1, &running, at);
    observer.observe(1, &running, at + policy.dwell);
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Quiet));
    assert_eq!(
        observer.next_wake(),
        None,
        "a quiet running job owes no periodic timer"
    );
    equivalent(&observer);
    let at = at + policy.dwell;
    observer.note_input(
        1,
        aterm_session::sink::InputLook::for_testing(1, 0, true),
        0,
    );
    observer.sessions.with_agent(1, |watch| {
        watch.pgid = 42;
        watch.resolved_at = Some(at);
    });
    observer.note_output(1, at);
    observer.note_build_wait(1, true);
    for interval in [
        Duration::from_secs(1),
        Duration::from_millis(10),
        Duration::from_secs(3),
    ] {
        assert!(observer.reconfigure(policy, interval, at));
        equivalent(&observer);
        assert!(!observer.reconfigure(policy, interval, at));
        equivalent(&observer);
    }
    observer.clear();
    assert_eq!(observer.sessions.index_sizes(), (0, 0, 0));
    equivalent(&observer);
}

#[test]
fn preclassification_unwinding_and_repeated_rescheduling_cannot_leave_stale_entries() {
    let t0 = Instant::now();
    let mut observer = StatusObserver::new(StatusPolicy::default(), Duration::from_millis(50));
    observer.note_input(
        3,
        aterm_session::sink::InputLook::for_testing(3, 1, false),
        0,
    );
    observer.sessions.with_agent(3, |watch| {
        watch.followup = true;
        watch.output_at = Some(t0);
    });
    assert_eq!(
        observer.next_wake(),
        None,
        "a watch alone is not a classified session"
    );
    assert_eq!(observer.sessions.index_sizes(), (1, 0, 0));
    observer.observe(3, &idle(), t0);
    equivalent(&observer);
    for i in 0..1_000 {
        let due = t0 + Duration::from_millis(i);
        observer.sessions.reschedule(3, due);
        observer
            .sessions
            .with_agent(3, |watch| watch.followup = i % 2 == 0);
        equivalent(&observer);
        assert_eq!(observer.sessions.index_sizes().0, 1);
    }
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        observer.sessions.with_agent(3, |watch| {
            watch.followup = true;
            watch.output_at = Some(t0 + Duration::from_secs(10));
            panic!("interrupt this scoped edit");
        });
    }));
    assert!(panic.is_err());
    equivalent(&observer);
    let before = observer.sessions.index_sizes();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        observer
            .sessions
            .edit(99, |_| panic!("interrupt an empty edit"));
    }));
    assert!(panic.is_err());
    assert_eq!(
        observer.sessions.index_sizes(),
        before,
        "an empty edit is retired during unwinding"
    );
    observer.retire(3);
    let reborn = t0 + Duration::from_secs(20);
    observer.observe(3, &idle(), reborn);
    assert_eq!(
        observer.next_wake(),
        Some(reborn + StatusPolicy::default().dwell)
    );
    equivalent(&observer);
    observer.observe(3, &idle(), reborn + StatusPolicy::default().dwell);
    assert_eq!(observer.next_wake(), None);
    equivalent(&observer);
}
