// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn autopilot_off_retires_a_normal_wait_until_a_fresh_same_binding_decision() {
    use aterm_agent::harness::account_progress::{
        ProgressPhase, RECOVERY_HISTORY_PREFIX, RECOVERY_RESUME_FINDING,
        SESSION_AUTOPILOT_OFF_FINDING,
    };
    use aterm_agent::harness::footer::{FactsOwner, FooterFacts};
    use aterm_agent::harness::{account_series, outlook};
    let mut app = crate::App::headless_for_test();
    app.proxy = None;
    let local = app.focused_session_id(crate::WindowId(0)).unwrap();
    let sid = app.session_sid(local).unwrap();
    let ctx = app.pool.get(local).unwrap().ctx.clone();
    let (a, idle, mut host, born) = execution_fixture(&sid);
    let now = aterm_time::unix_secs()
        .and_then(|at| i64::try_from(at).ok())
        .unwrap();
    a.now.store(now, Ordering::SeqCst);
    // This account passes the real execution gate: a different execution receipt must not
    // mask the normal decision whose recheck disappears when the session is switched Off.
    assert_eq!(idle.account_execution_hold(), None);
    assert!(
        idle.kept
            .lock()
            .unwrap()
            .account
            .execution_progress
            .is_none()
    );
    idle.switches
        .account_controls
        .store(ACCOUNTS_WALL, Ordering::SeqCst);
    // The gate fixture binds the real test process. Its limit must be observed after that
    // kernel birth, rather than using the fixture's older synthetic process-era timestamp.
    a.wall_of(ACCT_TALK_A, now, Some("five_hour"), Some(now + 3600));
    let roster = a.roster();
    a.store()
        .transact(now, &mut None, |state| {
            let mut events = Vec::new();
            for row in &roster.rows {
                if row.label != "pool-a" {
                    events.extend(
                        accounts::note_reading_held(
                            state,
                            &row.label,
                            &accounts::TapRead {
                                seven_day: Some(accounts::WindowRead {
                                    pct_x10: if row.label == "main" { 580 } else { 310 },
                                    reset: now + 4 * 86_400,
                                    refilled: None,
                                }),
                                at: now,
                                seen_before: true,
                                ..Default::default()
                            },
                            now,
                            accounts::ReadingOwner::Unobserved,
                        )
                        .0,
                    );
                }
                events.extend(accounts::note_wall(
                    state,
                    &row.label,
                    "five_hour",
                    Some(now + 3600),
                    now,
                    now,
                ));
            }
            Ok(events)
        })
        .unwrap();
    assert!(matches!(
        idle.account(usage_wall()),
        Some(AccountAdvice::Wait { .. })
    ));
    let retained = idle
        .kept
        .lock()
        .unwrap()
        .account
        .progress
        .as_ref()
        .unwrap()
        .value
        .clone();
    assert_eq!(retained.phase, ProgressPhase::Waiting);
    assert!(retained.next_at.is_some());
    // Use actual bound rows and store readings, with a retained usage series, rather than
    // planting forecast prose. Both accounts' standing limits make this a real future wait.
    let mut forecast_roster = a.roster();
    forecast_roster.rows.retain(|row| row.label != "pool-a");
    assert_eq!(forecast_roster.rows.len(), 2);
    let forecast_state = a.state();
    let series = forecast_roster
        .rows
        .iter()
        .map(|row| {
            let used = if row.label == "main" { 580 } else { 310 };
            let history = [(now - 86_400, 100), (now, used)]
                .into_iter()
                .map(|(at, pct_x10)| account_series::SeriesRow {
                    t: at,
                    by: forecast_state.machine.clone(),
                    said: account_series::Said::Fig {
                        five_hour: None,
                        seven_day: Some(account_series::Fig {
                            pct_x10,
                            reset: now + 4 * 86_400,
                            at,
                            src: accounts::SOURCE_TAP.into(),
                            epoch: None,
                        }),
                        ac: None,
                        key: None,
                    },
                })
                .collect();
            (row.label.clone(), history)
        })
        .collect();
    let forecast = outlook::outlook(&outlook::Inputs {
        roster: &forecast_roster,
        state: &forecast_state,
        enrolled: &forecast_roster
            .rows
            .iter()
            .map(|row| row.label.clone())
            .collect(),
        series: &series,
        renewals: &BTreeMap::new(),
        mode: accounts::Mode::Wall,
        credits: false,
        now,
        offset: |_| 0,
        signing_in: None,
    });
    let own = Some(outlook::Figure::Exact(580));
    let view = forecast.quota("main", own, true);
    assert!(view.tail.verdict.is_some() && !view.chart.runs.is_empty());
    assert!(!view.spoken.is_empty());
    let observed = forecast
        .observations_only(outlook::Refusal::SessionPaused)
        .quota("main", own, true);
    assert!(observed.tail.verdict.is_none() && observed.chart.runs.is_empty());
    {
        let mut timeline = ctx.timeline.lock().unwrap();
        let pid = i32::try_from(std::process::id()).unwrap();
        timeline.note_foreground_group(pid);
        timeline.set_program(pid, Some("claude".into()));
        timeline.set_agent_reader_program_for_test(Some(Program::Claude));
        timeline.set_claude_footer(
            pid,
            Some(born),
            Some(FooterFacts {
                owner: Some(FactsOwner {
                    floor: Some(born),
                    session_id: ACCT_TALK_A.into(),
                }),
                quota: Some(view.clone()),
                quota_observed: Some(observed.clone()),
                ..Default::default()
            }),
        );
    }
    let notes = Arc::new(Mutex::new(Vec::new()));
    let queued = Arc::clone(&notes);
    host.hooks.note = Arc::new(move |note| queued.lock().unwrap().push(note));
    let context = Arc::clone(&ctx);
    let session = sid.clone();
    host.hooks.autopilot = Arc::new(move || {
        context
            .meta
            .lock()
            .unwrap()
            .autopilot_word()
            .map(|word| HashMap::from([(session.clone(), word)]))
            .unwrap_or_default()
    });
    app.harness = Some(host.clone());
    let check_view = |paused: bool| {
        let facts = ctx
            .timeline
            .lock()
            .unwrap()
            .claude_footer()
            .unwrap()
            .clone();
        assert_eq!(
            (facts.quota.as_ref(), facts.quota_observed.as_ref()),
            (Some(&view), Some(&observed)),
            "raw pool evidence stays intact"
        );
        assert_eq!(facts.account_forecasts_paused(), paused);
        let shown = facts.quota_view().unwrap();
        if paused {
            assert_eq!(shown, &observed);
            assert_eq!(
                shown.tab().map(|named| named.figure),
                view.tab().map(|named| named.figure),
                "the observation stays"
            );
            // The raw view would still expose this real wait.
            assert!(facts.quota.as_ref().unwrap().tail.verdict.is_some());
        } else {
            assert_eq!(shown, &view);
        }
    };
    let model = aterm_models_harness::account_progress_policy_model();
    let mut policy = model.init_state();
    let mut epochs = vec![Arc::clone(
        &idle.kept.lock().unwrap().account.progress_policy,
    )];
    let check = |state: &aterm_spec::interp::State,
                 epochs: &[Arc<()>],
                 producer: Option<&AdviceRequest>| {
        let index = |token: &Arc<()>| {
            i64::try_from(
                epochs
                    .iter()
                    .position(|epoch| Arc::ptr_eq(epoch, token))
                    .unwrap(),
            )
            .unwrap()
        };
        let (epoch, record_epoch, value) = {
            let kept = idle.kept.lock().unwrap();
            let record = kept.account.progress.as_ref().unwrap();
            (
                index(&kept.account.progress_policy),
                i64::try_from(
                    epochs
                        .iter()
                        .position(|epoch| record.policy_is(epoch))
                        .unwrap(),
                )
                .unwrap(),
                i64::from(record.value.observed_at != retained.observed_at),
            )
        };
        let off = host.shared.lock().off.contains(&sid);
        let view = crate::harness_recovery_verb::snapshot(&ctx.timeline.lock().unwrap());
        for (field, actual) in [
            (
                "turn",
                if off {
                    1
                } else {
                    i64::try_from(epochs.len() - 1).unwrap()
                },
            ),
            ("epoch", epoch),
            ("record_epoch", record_epoch),
            ("record_value", value),
            (
                "producer_epoch",
                producer.map_or(3, |producer| index(&producer.turn.progress_policy)),
            ),
            (
                "view",
                view.as_ref().map_or(0, |view| match view.phase {
                    ProgressPhase::Waiting => 1,
                    ProgressPhase::Paused => 2,
                    ProgressPhase::Unknown => 3,
                    _ => panic!("the policy trace has no in-flight work"),
                }),
            ),
            (
                "next",
                i64::from(view.as_ref().is_some_and(|view| view.next_at.is_some())),
            ),
        ] {
            assert_eq!(state[field], actual, "{field}: {state:?}");
        }
        assert_eq!(
            ctx.timeline
                .lock()
                .unwrap()
                .claude_footer()
                .is_some_and(|facts| { facts.account_forecast_refusal().is_some() }),
            matches!(state["view"], 2 | 3),
            "the delivered policy view also withdraws forecast claims"
        );
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, state),
                "{}: {state:?}",
                invariant.name
            );
        }
    };
    let read = |app: &mut crate::App| {
        app.harness_progress_next_poll = None;
        app.service_harness_account_progress_with_reader(
            Instant::now(),
            |host, sid, pid, birth, talk| {
                host.account_progress_read_with_activity(
                    sid,
                    pid,
                    birth,
                    talk,
                    || (Instant::now(), Some(now)),
                    |_, _, _| Some(Vec::new()),
                )
            },
        );
    };
    check(&policy, &epochs, None);
    assert!(model.fire("Read", &mut policy));
    read(&mut app);
    assert!(model.fire("Deliver", &mut policy));
    check(&policy, &epochs, None);
    assert_eq!(
        crate::harness_recovery_verb::snapshot(&ctx.timeline.lock().unwrap()),
        Some(retained.clone())
    );
    check_view(false);
    let wanted = HashMap::from([(sid.clone(), Program::Claude)]);
    let mut worker = idle_worker();
    worker.agent = Program::Claude;
    worker.kept = Arc::clone(&idle.kept);
    let mut workers = Workers(HashMap::from([(sid.clone(), worker)]));
    let set = |app: &mut crate::App,
               workers: &mut Workers,
               word: Autopilot,
               policy: &mut aterm_spec::interp::State,
               epochs: &mut Vec<Arc<()>>,
               producer: Option<&AdviceRequest>| {
        crate::session_timeline::apply_autopilot(
            &ctx,
            Some(word.word().into()),
            None,
            aterm_slog::Actor::Human,
            None,
        );
        let off = OffSessions::read(&host.hooks, Autopilot::Cruise);
        let steps = StepsSeen::read(workers);
        visit_workers(
            &host.shared,
            workers,
            &steps,
            &mut BTreeSet::new(),
            &wanted,
            &off,
            true,
            0,
            &host.hooks,
        );
        publish_off(&host.shared, workers, &wanted, &off, &host.hooks);
        epochs.push(Arc::clone(
            &idle.kept.lock().unwrap().account.progress_policy,
        ));
        assert!(model.fire(if word == Autopilot::Off { "Off" } else { "On" }, policy));
        check(policy, epochs, producer);
        // A real host note must bypass an already armed future GUI poll.
        app.harness_progress_next_poll = Some(Instant::now() + Duration::from_secs(3600));
        let pending = std::mem::take(&mut *notes.lock().unwrap());
        assert!(
            pending
                .iter()
                .any(|note| matches!(note, HostNote::AccountProgress))
        );
        for note in pending {
            assert!(matches!(note, HostNote::AccountProgress));
            assert!(model.fire("Read", policy));
            app.note_harness_host(note);
            assert!(model.fire("Deliver", policy));
            check(policy, epochs, producer);
        }
        let progress =
            crate::harness_recovery_verb::snapshot(&ctx.timeline.lock().unwrap()).unwrap();
        assert_eq!(progress.phase, ProgressPhase::Paused);
        assert!(progress.forecasts_paused);
        assert_eq!(
            progress.details.first().map(String::as_str),
            Some(if word == Autopilot::Off {
                SESSION_AUTOPILOT_OFF_FINDING
            } else {
                RECOVERY_RESUME_FINDING
            })
        );
        assert_eq!(progress.next_at, None);
    };
    let before_off = AdviceTurn::begin(&idle.kept).capture(&idle.kept).unwrap();
    assert!(model.fire("CaptureChanged", &mut policy));
    check(&policy, &epochs, Some(&before_off));
    set(
        &mut app,
        &mut workers,
        Autopilot::Off,
        &mut policy,
        &mut epochs,
        Some(&before_off),
    );
    assert!(workers.0[&sid].stop.load(Ordering::SeqCst));
    read(&mut app);
    let paused = crate::harness_recovery_verb::snapshot(&ctx.timeline.lock().unwrap()).unwrap();
    assert_eq!(paused.phase, ProgressPhase::Paused);
    assert!(paused.forecasts_paused);
    check_view(true);
    // This real concurrent writer is outside the policy trace. Restore its prior view
    // before continuing that trace; contention must never revive the stored forecast.
    progress::lifecycle_tests::with_progress_writer(&host.shared.state, || read(&mut app));
    {
        let timeline = ctx.timeline.lock().unwrap();
        let facts = timeline.claude_footer().unwrap();
        assert!(facts.account_progress_busy);
        let busy = facts.account_progress_view().unwrap();
        assert_eq!(busy.phase, ProgressPhase::Unknown);
        assert!(busy.forecasts_paused);
    }
    check_view(true);
    read(&mut app);
    check_view(true);
    // Former presentation kept the same normal wait/deadline despite a known Off verdict.
    let mut former = policy.clone();
    former.insert("view", 1);
    former.insert("next", 1);
    assert!(
        model
            .invariants
            .iter()
            .any(|law| !model.check_invariant(law.name, &former))
    );
    assert_eq!((paused.eligible, paused.next_at), (None, None));
    assert_eq!(
        (
            paused.observed_at,
            &paused.account,
            paused.known,
            &paused.wall,
            paused.own_claim
        ),
        (
            retained.observed_at,
            &retained.account,
            retained.known,
            &retained.wall,
            retained.own_claim
        )
    );
    assert_eq!(
        paused.details.first().map(String::as_str),
        Some(SESSION_AUTOPILOT_OFF_FINDING)
    );
    for detail in &retained.details {
        assert!(
            paused
                .details
                .contains(&format!("{RECOVERY_HISTORY_PREFIX}{detail}"))
        );
    }
    assert_eq!(app.harness_progress_next_poll, None);
    app.open_fact_menu(
        crate::WindowId(0),
        local,
        crate::claude_lights::Fact::Account,
        (0, 0),
        false,
    );
    // The card states the pause in its one sentence where it has one, never the finding; the
    // finding is in the details the card's Copy details item copies. The retired decision's
    // stay word does not outlive it.
    assert_eq!(paused.stay, None);
    let menu = &app.autopilot_menu.as_ref().unwrap().rows;
    assert!(
        menu.iter()
            .all(|row| { row.text() != SESSION_AUTOPILOT_OFF_FINDING })
    );
    let report = crate::fact_menu::details_report(
        &app.fact_menu_facts(local, crate::claude_lights::Fact::Account),
    );
    assert!(
        report
            .lines()
            .any(|line| line == SESSION_AUTOPILOT_OFF_FINDING),
        "{report}"
    );
    assert!(
        report.contains("Automatic switching: Paused for this tab."),
        "{report}"
    );
    let lab = app.harness_state().lab;
    let lab = lab
        .sessions
        .iter()
        .find(|session| session.local_id == local)
        .unwrap();
    assert!(
        lab.facts
            .iter()
            .any(|(_, value)| value == SESSION_AUTOPILOT_OFF_FINDING)
    );
    let reply = crate::harness_recovery_verb::cmd("recovery", true, || Ok(Some(paused.clone())));
    for field in [
        "phase=paused",
        "eligible=-",
        "next_at=-",
        "forecasts_paused=1",
    ] {
        assert!(
            reply.split_whitespace().any(|word| word == field),
            "{reply}"
        );
    }
    // A calculation begun before Off may still return; it changes neither the retained
    // observation nor the separate action/advice policy when its display epoch is retired.
    let mut obsolete = idle
        .kept
        .lock()
        .unwrap()
        .account
        .progress
        .as_ref()
        .unwrap()
        .clone();
    obsolete.value.observed_at += 1;
    before_off.publish_scoped(
        &idle.kept,
        None,
        usage_wall(),
        Some(obsolete.clone()),
        false,
    );
    assert!(model.fire("Publish", &mut policy));
    check(&policy, &epochs, None);
    assert_eq!(
        idle.kept
            .lock()
            .unwrap()
            .account
            .progress
            .as_ref()
            .unwrap()
            .value,
        retained
    );
    let while_off = AdviceTurn::begin(&idle.kept).capture(&idle.kept).unwrap();
    assert!(model.fire("CaptureChanged", &mut policy));
    check(&policy, &epochs, Some(&while_off));
    let mut obsolete_off = idle
        .read_account_progress(&while_off, usage_wall(), None, None)
        .unwrap();
    obsolete_off.value = obsolete.value;
    set(
        &mut app,
        &mut workers,
        Autopilot::Cruise,
        &mut policy,
        &mut epochs,
        Some(&while_off),
    );
    while_off.publish_scoped(&idle.kept, None, usage_wall(), Some(obsolete_off), false);
    assert!(model.fire("Publish", &mut policy));
    check(&policy, &epochs, None);
    read(&mut app);
    let resumed = crate::harness_recovery_verb::snapshot(&ctx.timeline.lock().unwrap()).unwrap();
    assert_eq!(resumed.phase, ProgressPhase::Paused);
    assert!(resumed.forecasts_paused);
    check_view(true);
    assert_eq!(resumed.observed_at, retained.observed_at);
    assert_eq!(
        resumed.details.first().map(String::as_str),
        Some(RECOVERY_RESUME_FINDING)
    );
    assert_eq!(resumed.next_at, None);
    // The real account publication may legitimately emit identical data in the same second.
    let request = AdviceTurn::begin(&idle.kept).capture(&idle.kept).unwrap();
    assert!(model.fire("CaptureSame", &mut policy));
    check(&policy, &epochs, Some(&request));
    let mut same = idle
        .read_account_progress(&request, usage_wall(), None, None)
        .unwrap();
    same.value = retained.clone();
    request.publish_scoped(&idle.kept, None, usage_wall(), Some(same), false);
    assert!(model.fire("Publish", &mut policy));
    check(&policy, &epochs, None);
    assert!(model.fire("Read", &mut policy));
    read(&mut app);
    assert!(model.fire("Deliver", &mut policy));
    check(&policy, &epochs, None);
    let fresh = crate::harness_recovery_verb::snapshot(&ctx.timeline.lock().unwrap()).unwrap();
    assert_eq!(fresh, retained);
    assert!(!fresh.forecasts_paused);
    check_view(false);
    assert!(
        idle.kept
            .lock()
            .unwrap()
            .account
            .execution_progress
            .is_none()
    );
    // Current work is independently observed and intentionally outside the policy model.
    // It remains visible under Off without promising a later selection or retry.
    publish_off(
        &host.shared,
        &workers,
        &wanted,
        &OffSessions {
            words: HashMap::new(),
            default: Autopilot::Off,
        },
        &host.hooks,
    );
    let origin = a.seat().context();
    {
        let mut kept = idle.kept.lock().unwrap();
        kept.account.moved = Some(Moved {
            id: MoveId::default(),
            origin,
            to: "work-2".into(),
            at: AccountAt::Wall,
            login: None,
            process: kept.snapshot.clone().unwrap(),
            answered: false,
            untold: false,
        });
    }
    read(&mut app);
    let working = crate::harness_recovery_verb::snapshot(&ctx.timeline.lock().unwrap()).unwrap();
    assert_eq!(
        working.phase,
        ProgressPhase::Verifying {
            to: "work-2".into()
        }
    );
    assert_eq!((working.eligible, working.next_at), (None, None));
    assert!(working.forecasts_paused);
    check_view(true);
    let reply = crate::harness_recovery_verb::cmd("recovery", true, || Ok(Some(working.clone())));
    for field in [
        "phase=verifying",
        "target=work-2",
        "next_at=-",
        "forecasts_paused=1",
    ] {
        assert!(
            reply.split_whitespace().any(|word| word == field),
            "{reply}"
        );
    }
}

fn ready(sid: &str) -> (Arc<Accts>, WorkerIdle) {
    let a = Accts::new();
    a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let idle = a.worker(sid, &on());
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    (a, idle)
}

fn execution_fixture(sid: &str) -> (Arc<Accts>, WorkerIdle, HostHandle, u64) {
    let a = Accts::new();
    let mut snap = a.spawn(sid, ACCT_TALK_A, "main");
    let env = a.procs.lock().unwrap().remove(&snap.pid).unwrap();
    snap.pid = std::process::id();
    snap.start = aterm_agent::harness::restart_stamp::kernel_start(snap.pid).unwrap();
    let born = snap.current_start_seconds().unwrap();
    a.procs.lock().unwrap().insert(snap.pid, env);
    a.tabs.lock().unwrap().insert(sid.into(), snap);
    let mut cfg = on();
    cfg.set("accounts", "off").unwrap();
    cfg.set("logins", "off").unwrap();
    let idle = a.worker(sid, &cfg);
    let host = progress::lifecycle_tests::host_for(
        Arc::clone(&idle.kept),
        Arc::clone(&idle.switches),
        idle.hooks.clone(),
    );
    (a, idle, host, born)
}

pub(in crate::harness_host) struct ProgressExecutionFixture {
    accounts: Arc<Accts>,
    pub(in crate::harness_host) seat: AccountSeat,
    pub(in crate::harness_host) idle: WorkerIdle,
    pub(in crate::harness_host) host: HostHandle,
    pub(in crate::harness_host) born: u64,
}

impl ProgressExecutionFixture {
    pub(in crate::harness_host) fn account_calls(&self) -> [usize; 3] {
        ["env:", "restart:", "made:"].map(|call| self.accounts.asked(call))
    }
}

pub(in crate::harness_host) fn progress_execution_fixture(sid: &str) -> ProgressExecutionFixture {
    let (accounts, idle, host, born) = execution_fixture(sid);
    ProgressExecutionFixture {
        seat: accounts.seat(),
        accounts,
        idle,
        host,
        born,
    }
}

// The actual gate/deadline stay real; observations pin both clocks without a scheduling budget.
fn execution_clock(host: &HostHandle, sid: &str, elapsed: bool) -> (Instant, Option<i64>) {
    let kept = host.shared.kept.lock().unwrap().get(sid).cloned();
    let Some(kept) = kept else {
        return (Instant::now(), None);
    };
    let kept = kept.lock().unwrap();
    let Some(bound) = kept
        .account
        .execution_progress
        .as_ref()
        .or(kept.account.progress.as_ref())
    else {
        return (Instant::now(), None);
    };
    (
        bound.execution_until.map_or_else(Instant::now, |until| {
            if elapsed {
                until
            } else {
                until - Duration::from_nanos(1)
            }
        }),
        Some(bound.value.observed_at),
    )
}

fn execution_view(
    host: &HostHandle,
    sid: &str,
    born: u64,
) -> Option<aterm_agent::harness::account_progress::AccountProgress> {
    execution_read(host, sid, born).progress
}

fn execution_read(host: &HostHandle, sid: &str, born: u64) -> progress::ProgressRead {
    execution_read_at(host, sid, born, false)
}

fn execution_read_at(
    host: &HostHandle,
    sid: &str,
    born: u64,
    elapsed: bool,
) -> progress::ProgressRead {
    host.account_progress_read_with_activity(
        sid,
        std::process::id().try_into().unwrap(),
        Some(born),
        Some(ACCT_TALK_A),
        || execution_clock(host, sid, elapsed),
        |_, _, _| Some(Vec::new()),
    )
    .unwrap()
}

fn start_execution_work(a: &Accts, idle: &WorkerIdle, case: i64) {
    match case {
        17 => {
            a.store()
                .transact(ACCT_NOW, &mut None, |state| {
                    state.prepared_accounts.remove("main");
                    Ok(Vec::new())
                })
                .unwrap();
            idle.switches
                .account_controls
                .store(ACCOUNTS_EARLY, Ordering::SeqCst);
            assert!(idle.account_probe().is_ok());
            let mut kept = idle.kept.lock().unwrap();
            let mut stale_flight = pending_move(kept.snapshot.as_ref().unwrap());
            stale_flight.pid += 1;
            kept.account.flying = Some(stale_flight);
        }
        18 => {
            let mut kept = idle.kept.lock().unwrap();
            let mut flying = pending_move(kept.snapshot.as_ref().unwrap());
            flying.to = "work-2".into();
            kept.account.flying = Some(flying);
        }
        19 => {
            let mut kept = idle.kept.lock().unwrap();
            kept.account.moved = Some(Moved {
                id: MoveId::default(),
                origin: a.seat().context(),
                to: "work-2".into(),
                at: AccountAt::Ahead,
                login: None,
                process: kept.snapshot.clone().unwrap(),
                answered: false,
                untold: false,
            });
        }
        _ => {}
    }
}

fn execution_work_read(
    a: &Accts,
    host: &HostHandle,
    sid: &str,
    born: u64,
    case: i64,
    elapsed: bool,
) -> progress::ProgressRead {
    if matches!(case, 20 | 21) {
        let activity_roster = a.roster();
        host.account_progress_read_with_activity(
            sid,
            std::process::id().try_into().unwrap(),
            Some(born),
            Some(ACCT_TALK_A),
            || execution_clock(host, sid, elapsed),
            |store, cli, now| {
                assert_eq!(store.dir, a.store().dir);
                assert_eq!(cli, accounts::Cli::Claude);
                (case == 20).then(|| {
                    ["work-2", "unlisted-account"]
                        .map(|label| aterm_agent::harness::accounts_sync::LoginActivity {
                            queued: None,
                            label: label.into(),
                            identity: activity_roster
                                .row(label)
                                .map(aterm_agent::harness::accounts_sync::LoginIdentity::for_test),
                            route: Some("fixture-browser".into()),
                            active: true,
                            observed_at: now,
                            next_at: Some(now + 1),
                            diagnostic: None,
                        })
                        .to_vec()
                })
            },
        )
        .unwrap()
    } else {
        execution_read_at(host, sid, born, elapsed)
    }
}

fn execution_work_view(
    a: &Accts,
    host: &HostHandle,
    sid: &str,
    born: u64,
    case: i64,
) -> Option<aterm_agent::harness::account_progress::AccountProgress> {
    execution_work_read(a, host, sid, born, case, false).progress
}

fn execution_case(case: i64, project: bool, legacy_hidden: bool) -> [i64; 14] {
    execution_case_at(case, project, legacy_hidden, false, 0)
}

fn execution_case_at(
    case: i64,
    project: bool,
    legacy_hidden: bool,
    elapsed: bool,
    legacy_expiry: u8,
) -> [i64; 14] {
    use aterm_agent::harness::account_progress::{self as facts, ProgressPhase};
    let sid = "s-execution-model";
    let (a, mut idle, host, born) = execution_fixture(sid);
    if matches!(case, 1 | 3) {
        idle.switches
            .account_controls
            .store(ACCOUNTS_WALL, Ordering::SeqCst);
    }
    let mut normal = idle.capture_execution_progress().unwrap();
    normal.value.phase = ProgressPhase::Watching;
    idle.kept.lock().unwrap().account.progress = Some(normal);
    let prepare = |prepared| {
        a.store()
            .transact(ACCT_NOW, &mut None, |state| {
                if prepared {
                    state.prepared_accounts.insert("main".into());
                } else {
                    state.prepared_accounts.remove("main");
                }
                Ok(Vec::new())
            })
            .unwrap();
    };
    if !matches!(case, 4..=6 | 12 | 13) {
        prepare(true);
    }
    if matches!(case, 2 | 3) {
        idle.account_execution_hold().unwrap();
        prepare(false);
    }
    if case == 4 {
        std::fs::write(a.roster_path(), "[[account").unwrap();
    }
    if case == 5 {
        std::fs::create_dir_all(&a.store().dir).unwrap();
        std::fs::write(a.store().state_file(), "{").unwrap();
    }
    if case == 6 {
        let mut kept = idle.kept.lock().unwrap();
        kept.account.flying = Some(pending_move(kept.snapshot.as_ref().unwrap()));
    }
    if matches!(case, 7 | 12) {
        let original = Arc::clone(&idle.hooks.acts.account_env);
        let kept = Arc::clone(&idle.kept);
        idle.hooks.acts.account_env = Arc::new(move |snap| {
            let mut env = original(snap)?;
            if case == 7 {
                kept.lock().unwrap().account.landing = Arc::default();
            } else {
                env.retain(|entry| !entry.starts_with(&format!("{}=", accounts::LOGIN_ENV)));
                env.push(format!("{}=/unlisted/account", accounts::LOGIN_ENV));
            }
            Some(env)
        });
    }
    let old_admission = if case == 13 {
        let captured = idle.capture_execution_progress();
        let admitted = idle.account_execution_hold();
        assert!(admitted.is_none());
        idle.kept.lock().unwrap().account.landing = Arc::default();
        prepare(true);
        Some((captured, admitted))
    } else {
        None
    };
    if (14..=16).contains(&case) {
        idle.kept.lock().unwrap().snapshot.as_mut().unwrap().start =
            "unconfirmed-process-start".into();
    }
    let authority = || {
        [
            a.store().state_file(),
            a.store().logins_file(),
            a.store().ledger_path(),
            a.store().machine_file(),
            a.roster_path(),
        ]
        .map(|path| std::fs::read(path).ok())
    };
    let before = authority();
    let actual = idle.account_execution_hold();
    let hold = if let Some((old, admitted)) = old_admission {
        assert!(actual.is_some());
        idle.publish_execution_progress(old, admitted, "old admission", None);
        admitted.is_some()
    } else {
        actual.is_some()
    };
    let preserved = authority() == before && a.asked("restart:") == 0 && a.asked("made:") == 0;
    if project {
        start_execution_work(&a, &idle, case);
        let mut kept = idle.kept.lock().unwrap();
        match case {
            9 => kept.snapshot.as_mut().unwrap().session = Some("other-conversation".into()),
            10 => kept
                .snapshot
                .as_mut()
                .unwrap()
                .argv
                .push("--other-auth-context".into()),
            11 | 16 => kept.account.landing = Arc::default(),
            _ => {}
        }
    }
    let before_projection = authority();
    let published = idle
        .kept
        .lock()
        .unwrap()
        .account
        .execution_progress
        .is_some();
    let hidden = if project && legacy_hidden && case == 0 {
        idle.kept.lock().unwrap().account.execution_progress.take()
    } else {
        None
    };
    let hidden_probe = if project && legacy_hidden && case == 17 {
        idle.kept.lock().unwrap().account.probing.take()
    } else {
        None
    };
    let mut read = if project && case >= 17 {
        Some(execution_work_read(&a, &host, sid, born, case, elapsed))
    } else {
        project.then(|| {
            host.account_progress_read_with_activity(
                sid,
                std::process::id().try_into().unwrap(),
                if matches!(case, 14 | 16) {
                    None
                } else {
                    Some(born + u64::from(case == 8))
                },
                Some(ACCT_TALK_A),
                || execution_clock(&host, sid, elapsed),
                |_, _, _| Some(Vec::new()),
            )
            .unwrap()
        })
    };
    if legacy_hidden && case == 21 {
        read.as_mut().unwrap().activity_contended = false;
    }
    if project && legacy_expiry == 1 {
        // Former projection ignored elapsed monotonic time and kept the original refusal.
        let mut value = idle
            .kept
            .lock()
            .unwrap()
            .account
            .execution_progress
            .as_ref()
            .unwrap()
            .value
            .clone();
        value
            .details
            .push("Automatic account rotation is off".into());
        read.as_mut().unwrap().progress = Some(value);
        read.as_mut().unwrap().execution_until = None;
    } else if project && legacy_expiry == 2 {
        read.as_mut().unwrap().execution_until = None;
    }
    let retry = read.as_ref().is_some_and(progress::ProgressRead::retry);
    let wake = read.as_ref().is_some_and(|read| {
        read.next_poll(execution_clock(&host, sid, elapsed).0)
            .is_some()
    });
    let view = read.and_then(|read| read.progress);
    if hidden.is_some() {
        idle.kept.lock().unwrap().account.execution_progress = hidden;
    }
    if hidden_probe.is_some() {
        idle.kept.lock().unwrap().account.probing = hidden_probe;
    }
    let visible = view.as_ref().is_some_and(|v| {
        v.details.iter().any(|finding| {
            matches!(
                finding
                    .strip_prefix(facts::EXECUTION_HISTORY_PREFIX)
                    .unwrap_or(finding),
                facts::EXECUTION_PREPARED_FINDING
                    | facts::EXECUTION_ROSTER_UNREAD_FINDING
                    | facts::EXECUTION_CONTEXT_UNREAD_FINDING
                    | facts::EXECUTION_RECORDS_UNREAD_FINDING
            )
        })
    });
    let generic_safe = !(14..=16).contains(&case)
        || view.as_ref().is_none_or(|v| {
            v.account.is_none()
                && v.wall.is_none()
                && v.known.is_none()
                && v.eligible.is_none()
                && v.details.first().map(String::as_str)
                    == Some(if elapsed {
                        facts::EXECUTION_REFRESH_FINDING
                    } else {
                        facts::EXECUTION_CONTEXT_UNREAD_FINDING
                    })
        });
    if visible && !elapsed && !(14..=16).contains(&case) {
        assert_eq!(
            view.as_ref().unwrap().details.first().map(String::as_str),
            Some(match case {
                4 => facts::EXECUTION_ROSTER_UNREAD_FINDING,
                5 => facts::EXECUTION_RECORDS_UNREAD_FINDING,
                6 => facts::EXECUTION_CONTEXT_UNREAD_FINDING,
                _ => facts::EXECUTION_PREPARED_FINDING,
            })
        );
    }
    [
        i64::from(hold),
        i64::from(published),
        i64::from(visible),
        view.as_ref().map_or(0, |v| match v.phase {
            ProgressPhase::Waiting => 1,
            ProgressPhase::Disabled => 2,
            ProgressPhase::Watching => 3,
            ProgressPhase::Probing => 4,
            ProgressPhase::Switching { .. } => 5,
            ProgressPhase::Verifying { .. } => 6,
            ProgressPhase::SigningIn { .. } => 7,
            _ => 8,
        }),
        i64::from(
            view.as_ref()
                .is_none_or(|v| v.known.is_none() && v.eligible.is_none()),
        ),
        i64::from(view.as_ref().is_some_and(|v| v.next_at.is_some())),
        i64::from(preserved),
        i64::from(generic_safe),
        i64::from(authority() == before_projection),
        i64::from(retry),
        i64::from(view.as_ref().is_some_and(|v| {
            v.details.iter().any(|s| {
                matches!(
                    s.as_str(),
                    facts::EXECUTION_PREPARED_FINDING
                        | facts::EXECUTION_ROSTER_UNREAD_FINDING
                        | facts::EXECUTION_CONTEXT_UNREAD_FINDING
                        | facts::EXECUTION_RECORDS_UNREAD_FINDING
                )
            })
        })),
        i64::from(view.as_ref().is_some_and(|v| {
            v.details
                .iter()
                .any(|s| s.starts_with(facts::EXECUTION_HISTORY_PREFIX))
        })),
        i64::from(wake),
        i64::from(
            view.as_ref()
                .is_some_and(|value| !value.independent_work.is_empty()),
        ),
    ]
}

fn diagnostic_variants(before: &aterm_spec::interp::State) -> &'static [u8] {
    if before["activity"] == 3 && before["diagnostic_ok"] == 0 {
        &[1, 2, 3, 4] // Missing, zero-time, future-time and a different diagnostic row.
    } else {
        &[0]
    }
}

fn execution_activity_transition(
    before: &aterm_spec::interp::State,
    legacy: u8,
) -> aterm_spec::interp::State {
    let mut observed = None;
    for off in [false, true] {
        for &diagnostic_variant in diagnostic_variants(before) {
            let values = progress::expired_activity_tests::expired_activity_observation(
                before["elapsed"] == 1 || legacy == 2,
                before["activity_rows_changed"] == 1 && legacy != 3,
                legacy != 1,
                if before["activity"] == 3 { 3 } else { 1 },
                off,
                diagnostic_variant,
            );
            let mut after = before.clone();
            after.insert("stage", 2);
            for (field, value) in [
                ("view", values[0]),
                ("deadline", values[1]),
                ("current", values[2]),
                ("historical", values[3]),
                ("counts_unknown", values[4]),
                ("authority", values[6]),
                ("retained", values[6]),
                ("retry", values[8]),
                ("wake", values[9]),
                ("visible", i64::from(values[2] == 1 || values[3] == 1)),
                ("work_visible", values[10]),
                ("activity_history", i64::from(values[7] > 0)),
            ] {
                after.insert(field, value);
            }
            if before["activity"] != 3 {
                assert_eq!(values[7], 0, "active work acquired a historical diagnostic");
            }
            if let Some(expected) = &observed {
                assert_eq!(
                    expected, &after,
                    "Off or invalid-diagnostic variants changed the projection"
                );
            } else {
                observed = Some(after);
            }
        }
    }
    observed.unwrap()
}

fn retained_normal_execution_transition(
    before: &aterm_spec::interp::State,
    legacy: u8,
) -> aterm_spec::interp::State {
    let mut observed = None;
    for resumed in [false, true] {
        for &diagnostic_variant in diagnostic_variants(before) {
            let context_present = before["context"] == 1 && before["context_record"] == 1;
            let values = progress::retired_activity_tests::retired_activity_observation(
                if legacy == 4 { 1 } else { before["normal"] },
                context_present && legacy != 3,
                before["activity"],
                resumed,
                diagnostic_variant,
            );
            let mut after = before.clone();
            after.insert("stage", 2);
            for (field, value) in [
                ("view", values[0]),
                ("counts_unknown", values[6]),
                ("deadline", values[2]),
                ("generic_safe", values[7]),
                (
                    "retry",
                    i64::from(values[0] == 7 || values[6] == 1 || values[8] == 1),
                ),
                (
                    "wake",
                    i64::from(values[0] == 7 || values[6] == 1 || values[8] == 1),
                ),
                ("work_visible", values[8]),
                ("forecasts", i64::from(values[1] == 1 || values[2] == 1)),
                ("paused", values[3]),
                ("retained", values[4]),
                ("activity_history", i64::from(values[5] > 0)),
            ] {
                after.insert(field, value);
            }
            if let Some(expected) = &observed {
                assert_eq!(
                    expected, &after,
                    "retirement or invalid-diagnostic variants changed the projection"
                );
            } else {
                observed = Some(after);
            }
        }
    }
    observed.unwrap()
}

fn independent_execution_transition(
    before: &aterm_spec::interp::State,
    action: &str,
    legacy: u8,
) -> aterm_spec::interp::State {
    use aterm_agent::harness::account_progress::ProgressPhase;
    use progress::activity::ActivityCapture;

    let sid = "s-independent-execution-model";
    let (a, idle, host, born) = execution_fixture(sid);
    let seat = a.seat();
    let initial_generation = Arc::clone(&idle.kept.lock().unwrap().account.landing);
    let authority = || {
        [
            a.store().state_file(),
            a.store().logins_file(),
            a.store().ledger_path(),
            a.store().machine_file(),
            a.roster_path(),
        ]
        .map(|path| std::fs::read(path).ok())
    };
    let images = authority();
    let capture = if before["context"] > 0 || action == "CaptureContext" {
        Some(ActivityCapture::capture(&idle.kept.lock().unwrap()).unwrap())
    } else {
        None
    };
    let replace = || idle.kept.lock().unwrap().account.landing = Arc::default();
    if before["context"] == 2 {
        replace();
    }
    if before["stage"] == 1 {
        if let Some(capture) = capture.as_ref() {
            let read = capture.read_for_test(&seat, born);
            capture.publish_for_test(&idle.kept, &idle.hooks, &seat, read, Some(born));
        }
        if before["context"] == 3 {
            replace();
        }
    }
    let mut after = before.clone();
    match action {
        "CaptureContext" => {
            // Capturing metadata does not publish a judgment or activity observation.
            assert!(capture.is_some());
            after.insert("context", 1);
        }
        "ReplaceContext" => {
            replace();
            after.insert("context", if before["stage"] == 0 { 2 } else { 3 });
        }
        "WorkStarts" | "ActivityBusy" => {
            after.insert("activity", if action == "WorkStarts" { 1 } else { 2 });
        }
        "PublishContext" => {
            if let Some(capture) = capture.as_ref() {
                let read = capture.read_for_test(&seat, born);
                capture.publish_for_test(&idle.kept, &idle.hooks, &seat, read, Some(born));
            }
            after.insert("stage", 1);
            after.insert(
                "context_record",
                i64::from(idle.kept.lock().unwrap().account.activity_context.is_some()),
            );
        }
        "Project" => {
            if legacy == 1 {
                // The former reader had no independent metadata source at all.
                idle.kept.lock().unwrap().account.activity_context = None;
            } else if legacy == 2 {
                // Counterfactual reuse of the captured fence authorizes the retired row.
                idle.kept.lock().unwrap().account.landing = initial_generation;
            }
            let roster = a.roster();
            let clock = Instant::now();
            let read = host.account_progress_read_with_activity(
                sid,
                std::process::id().try_into().unwrap(),
                Some(born),
                Some(ACCT_TALK_A),
                || (clock, Some(ACCT_NOW)),
                |store, cli, now| {
                    assert_eq!(store.dir, a.store().dir);
                    assert_eq!(cli, accounts::Cli::Claude);
                    match before["activity"] {
                        0 => Some(Vec::new()),
                        1 => Some(vec![aterm_agent::harness::accounts_sync::LoginActivity {
                            queued: None,
                            label: "work-2".into(),
                            route: Some("fixture-browser".into()),
                            active: true,
                            observed_at: now,
                            // Context-only views must not inherit a scheduling judgment.
                            next_at: Some(now + 900),
                            diagnostic: None,
                            identity: roster
                                .row("work-2")
                                .map(aterm_agent::harness::accounts_sync::LoginIdentity::for_test),
                        }]),
                        2 => None,
                        3 => Some(vec![aterm_agent::harness::accounts_sync::LoginActivity {
                            queued: None,
                            label: "work-2".into(),
                            route: Some("fixture-browser".into()),
                            active: false,
                            observed_at: now,
                            next_at: Some(now + 900),
                            diagnostic: if before["diagnostic_ok"] == 1 {
                                Some(aterm_agent::harness::accounts_sync::LoginDiagnostic::from_exit_for_test(
                                    roster.row("work-2").unwrap(), now - 1, 1, None,
                                ))
                            } else { None },
                            identity: roster.row("work-2").map(
                                aterm_agent::harness::accounts_sync::LoginIdentity::for_test,
                            ),
                        }]),
                        _ => unreachable!(),
                    }
                },
            );
            let (view, retry, wake) = match read {
                Ok(read) => {
                    let retry = read.retry();
                    let wake = read.next_poll(clock).is_some();
                    (read.progress, retry, wake)
                }
                Err(()) => (None, true, true),
            };
            after.insert("stage", 2);
            after.insert(
                "view",
                view.as_ref().map_or(0, |value| {
                    if matches!(value.phase, ProgressPhase::SigningIn { .. }) {
                        7
                    } else {
                        8
                    }
                }),
            );
            after.insert(
                "counts_unknown",
                i64::from(
                    view.as_ref()
                        .is_none_or(|value| value.known.is_none() && value.eligible.is_none()),
                ),
            );
            after.insert(
                "generic_safe",
                i64::from(view.as_ref().is_none_or(|value| {
                    value.account.is_none()
                        && value.wall.is_none()
                        && !matches!(value.phase, ProgressPhase::Ready)
                })),
            );
            after.insert(
                "deadline",
                i64::from(view.as_ref().is_some_and(|value| value.next_at.is_some())),
            );
            after.insert("retry", i64::from(retry));
            after.insert("wake", i64::from(wake));
            after.insert(
                "work_visible",
                i64::from(
                    view.as_ref()
                        .is_some_and(|value| !value.independent_work.is_empty()),
                ),
            );
            after.insert(
                "activity_history",
                i64::from(
                    view.as_ref()
                        .is_some_and(|value| !value.login_diagnostics.is_empty()),
                ),
            );
        }
        _ => panic!("unknown independent activity action {action}"),
    }
    let kept = idle.kept.lock().unwrap();
    assert!(kept.account.progress.is_none());
    assert!(kept.account.execution_progress.is_none());
    assert_eq!(
        authority(),
        images,
        "metadata observation changed account records"
    );
    assert_eq!(a.asked("restart:"), 0);
    assert_eq!(a.asked("made:"), 0);
    after
}

pub(crate) fn execution_transition(
    before: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut after = before.clone();
    if action == "Case" {
        after.insert("case", before["case"] + 1);
        return after;
    }
    if action == "WorkSettles" {
        after.insert("activity", 3);
        return after;
    }
    if action == "RejectDiagnostic" {
        after.insert("diagnostic_ok", 0);
        return after;
    }
    if before["case"] == 22 {
        if action == "RetainNormal" || action == "RetireNormal" {
            // These fixture inputs reuse the real policy publication paths in the
            // retained-normal projection adapter; this machine does not decide policy.
            after.insert("normal", if action == "RetainNormal" { 1 } else { 2 });
            return after;
        }
        if action == "Project" && before["normal"] > 0 {
            return retained_normal_execution_transition(before, 0);
        }
        return independent_execution_transition(before, action, 0);
    }
    if action == "Elapse" {
        after.insert("elapsed", 1);
        return after;
    }
    if action == "ReplaceActivityRows" {
        // This input is replayed as an admitted Store replacement by the native projection.
        after.insert("activity_rows_changed", 1);
        return after;
    }
    if before["case"] == 20 && action == "Project" {
        return execution_activity_transition(before, 0);
    }
    let values = execution_case_at(
        before["case"],
        action == "Project",
        false,
        before["elapsed"] == 1,
        0,
    );
    after.insert("stage", if action == "Observe" { 1 } else { 2 });
    let fields: &[(&str, usize)] = if action == "Observe" {
        &[("hold", 0), ("published", 1), ("authority", 6)]
    } else {
        assert_eq!(action, "Project");
        &[
            ("visible", 2),
            ("view", 3),
            ("counts_unknown", 4),
            ("deadline", 5),
            ("generic_safe", 7),
            ("retry", 9),
            ("current", 10),
            ("historical", 11),
            ("wake", 12),
            ("work_visible", 13),
        ]
    };
    for &(field, index) in fields {
        after.insert(field, values[index]);
    }
    assert_eq!(values[8], 1, "projection changed durable account records");
    after
}

#[test]
fn execution_visibility_conforms_to_the_actual_gate_and_rejects_hidden_off_control() {
    use aterm_spec::verify;
    let model = aterm_models_harness::account_execution_visibility_model();
    assert_eq!(model.mutant_dials(), (1..=45).collect::<Vec<_>>());
    let coverage = verify::prove_and_catch_scalar(&model, "account execution visibility");
    if verify::checkers_required() {
        assert_eq!(coverage, verify::Covered::InterpreterAndTy);
    }
    let mut transitions = 0;
    let mut independent_transitions = 0;
    for before in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &before) {
                let after = execution_transition(&before, action.name);
                let (accepted, why) = verify::validate_transition_tiered(
                    &model,
                    &[],
                    &before,
                    &after,
                    Some(action.name),
                    "account execution visibility",
                );
                eprintln!(
                    "{} case={} elapsed={} {}: {why}",
                    model.name, before["case"], before["elapsed"], action.name
                );
                assert!(accepted, "{before:?} -> {after:?}");
                transitions += 1;
                independent_transitions += usize::from(before["case"] == 22);
            }
        }
    }
    assert!(independent_transitions > 0);
    eprintln!(
        "{transitions} actual transitions, {independent_transitions} independent-context transitions"
    );
    let independent = aterm_spec::interp::reachable_states(&model);
    let settled_normal = independent
        .iter()
        .find(|state| {
            state["case"] == 22
                && state["stage"] == 1
                && state["normal"] == 1
                && state["context"] == 1
                && state["context_record"] == 1
                && state["activity"] == 3
                && state["diagnostic_ok"] == 1
        })
        .unwrap();
    let hidden_history = retained_normal_execution_transition(settled_normal, 3);
    assert!(!model.check_invariant("SettledHistoryUsesCurrentContext", &hidden_history));
    assert!(
        !model
            .successors("Project", settled_normal)
            .contains(&hidden_history)
    );
    let settled_gate = independent
        .iter()
        .find(|state| {
            state["case"] == 20
                && state["stage"] == 1
                && state["elapsed"] == 0
                && state["activity_rows_changed"] == 1
                && state["activity"] == 3
                && state["diagnostic_ok"] == 1
        })
        .unwrap();
    let hidden_history = execution_activity_transition(settled_gate, 1);
    assert!(!model.check_invariant("SettledHistoryUsesCurrentContext", &hidden_history));
    assert!(
        !model
            .successors("Project", settled_gate)
            .contains(&hidden_history)
    );
    for (elapsed, legacy, law) in [
        (1, 1, "CurrentWorkVisible"),
        (0, 2, "CurrentWorkVisible"),
        (1, 3, "ActualRecheckShown"),
    ] {
        let before = independent
            .iter()
            .find(|state| {
                state["case"] == 20
                    && state["stage"] == 1
                    && state["activity"] == 0
                    && state["activity_rows_changed"] == 1
                    && state["elapsed"] == elapsed
            })
            .unwrap();
        // Actual native projections distinguish missing B metadata, premature expiry,
        // and the unchanged-row retry forecast that must not cross a changed context.
        let old = execution_activity_transition(before, legacy);
        assert!(!model.check_invariant(law, &old));
        assert!(!model.successors("Project", before).contains(&old));
    }
    let live = independent
        .iter()
        .find(|state| {
            state["case"] == 20
                && state["stage"] == 1
                && state["activity"] == 0
                && state["elapsed"] == 0
                && state["activity_rows_changed"] == 1
        })
        .unwrap();
    let hidden_work = execution_activity_transition(live, 1);
    assert!(!model.check_invariant("CurrentScopedWorkVisible", &hidden_work));
    let normal = independent
        .iter()
        .find(|state| {
            state["case"] == 22
                && state["stage"] == 1
                && state["normal"] == 1
                && state["context"] == 1
                && state["context_record"] == 1
                && state["activity"] == 1
        })
        .unwrap();
    let hidden_work = retained_normal_execution_transition(normal, 3);
    assert!(!model.check_invariant("CurrentScopedWorkVisible", &hidden_work));
    for (context, legacy, law) in [
        (1, 1, "IndependentWorkVisible"),
        (3, 2, "ContextScopeRespected"),
    ] {
        let before = independent
            .iter()
            .find(|state| {
                state["case"] == 22
                    && state["stage"] == 1
                    && state["normal"] == 0
                    && state["context"] == context
                    && state["context_record"] == 1
                    && state["activity"] == 1
            })
            .unwrap();
        let old = independent_execution_transition(before, "Project", legacy);
        assert!(!model.check_invariant(law, &old));
        assert!(!model.successors("Project", before).contains(&old));
    }
    for (activity, legacy, law) in [
        (1, 3, "IndependentWorkVisible"),
        (0, 4, "RetiredNormalWithdrawsForecasts"),
    ] {
        let before = independent
            .iter()
            .find(|state| {
                state["case"] == 22
                    && state["stage"] == 1
                    && state["normal"] == 2
                    && state["context"] == 1
                    && state["context_record"] == 1
                    && state["activity"] == activity
            })
            .unwrap();
        // Removing current B metadata reproduces old-context priority; replaying the
        // current-policy projection reproduces a retired receipt keeping its forecasts.
        let old = retained_normal_execution_transition(before, legacy);
        assert!(!model.check_invariant(law, &old));
        assert!(!model.successors("Project", before).contains(&old));
    }
    let mut before = model.init_state();
    before = execution_transition(&before, "Observe");
    let mut hidden = before.clone();
    hidden.insert("stage", 2);
    let actual = execution_case(0, true, true);
    for (field, index) in [
        ("visible", 2),
        ("view", 3),
        ("counts_unknown", 4),
        ("deadline", 5),
        ("generic_safe", 7),
        ("retry", 9),
        ("current", 10),
        ("historical", 11),
        ("wake", 12),
    ] {
        hidden.insert(field, actual[index]);
    }
    let (accepted, why) = verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &hidden,
        Some("Project"),
        "former hidden rotation-off execution hold",
    );
    eprintln!(
        "{} executable old-hidden-off control: {why}; {coverage:?}",
        model.name
    );
    assert!(!accepted);
    assert!(!model.check_invariant("RotationOffKeepsGate", &hidden));
    let mut before = model.init_state();
    before.insert("case", 17);
    before = execution_transition(&before, "Observe");
    let mut hidden_work = before.clone();
    hidden_work.insert("stage", 2);
    let actual = execution_case(17, true, true);
    for (field, index) in [
        ("visible", 2),
        ("view", 3),
        ("counts_unknown", 4),
        ("deadline", 5),
        ("generic_safe", 7),
        ("retry", 9),
        ("current", 10),
        ("historical", 11),
        ("wake", 12),
    ] {
        hidden_work.insert(field, actual[index]);
    }
    let (accepted, why) = verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &hidden_work,
        Some("Project"),
        "former retained gate hides current probe",
    );
    eprintln!(
        "{} executable hidden-work control: {why}; {coverage:?}",
        model.name
    );
    assert!(!accepted);
    assert!(!model.check_invariant("CurrentWorkVisible", &hidden_work));
    let mut before = model.init_state();
    before.insert("case", 21);
    before = execution_transition(&before, "Observe");
    let mut lost_retry = before.clone();
    lost_retry.insert("stage", 2);
    let actual = execution_case(21, true, true);
    for (field, index) in [
        ("visible", 2),
        ("view", 3),
        ("counts_unknown", 4),
        ("deadline", 5),
        ("generic_safe", 7),
        ("retry", 9),
        ("current", 10),
        ("historical", 11),
        ("wake", 12),
    ] {
        lost_retry.insert(field, actual[index]);
    }
    let (accepted, why) = verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &lost_retry,
        Some("Project"),
        "former busy gate observation loses retry metadata",
    );
    eprintln!(
        "{} executable discarded-contention control: {why}; {coverage:?}",
        model.name
    );
    assert!(!accepted);
    assert!(!model.check_invariant("UnobservedActivityRechecks", &lost_retry));
    for (elapsed, legacy, law, label) in [
        (
            true,
            1,
            "CurrentHoldRequiresUnexpiredLease",
            "former expired receipt still Waiting",
        ),
        (
            false,
            2,
            "LeaseExpirySchedulesObservation",
            "former settled gate arms no expiry wake",
        ),
    ] {
        let mut before = model.init_state();
        before = execution_transition(&before, "Observe");
        if elapsed {
            before = execution_transition(&before, "Elapse");
        }
        let mut old = before.clone();
        old.insert("stage", 2);
        let actual = execution_case_at(0, true, false, elapsed, legacy);
        for (field, index) in [
            ("visible", 2),
            ("view", 3),
            ("counts_unknown", 4),
            ("deadline", 5),
            ("generic_safe", 7),
            ("retry", 9),
            ("current", 10),
            ("historical", 11),
            ("wake", 12),
        ] {
            old.insert(field, actual[index]);
        }
        let (accepted, why) =
            verify::validate_transition_tiered(&model, &[], &before, &old, Some("Project"), label);
        eprintln!(
            "{} executable expiry control: {why}; {coverage:?}",
            model.name
        );
        assert!(!accepted);
        assert!(!model.check_invariant(law, &old));
    }
}

#[test]
fn a_repaired_execution_hold_becomes_history_at_its_monotonic_deadline() {
    use aterm_agent::harness::account_progress::{
        EXECUTION_HISTORY_PREFIX, EXECUTION_PREPARED_FINDING, EXECUTION_REFRESH_FINDING,
        ProgressPhase,
    };
    let sid = "s-execution-deadline-boundary";
    let (a, idle, host, born) = execution_fixture(sid);
    a.store()
        .transact(ACCT_NOW, &mut None, |state| {
            state.prepared_accounts.insert("main".into());
            Ok(Vec::new())
        })
        .unwrap();
    idle.account_execution_hold().unwrap();
    let (retained, until) = {
        let kept = idle.kept.lock().unwrap();
        let bound = kept.account.execution_progress.as_ref().unwrap();
        (bound.value.clone(), bound.execution_until.unwrap())
    };
    // The account is repaired while the observer is parked behind another owner; no new gate
    // call occurs. Store news alone must not turn the retained refusal into fresh evidence.
    a.store()
        .transact(ACCT_NOW, &mut None, |state| {
            state.prepared_accounts.remove("main");
            Ok(Vec::new())
        })
        .unwrap();
    host.note_accounts();
    let repaired = std::fs::read(a.store().state_file()).unwrap();
    for (mono, wall, fresh) in [
        (
            until - Duration::from_nanos(1),
            Some(retained.observed_at),
            true,
        ),
        (until, Some(retained.observed_at), false),
        (until + Duration::from_secs(1), None, false),
        (
            until + Duration::from_secs(1),
            Some(retained.observed_at - 86_400),
            false,
        ),
    ] {
        let read = host
            .account_progress_read_with_activity(
                sid,
                std::process::id().try_into().unwrap(),
                Some(born),
                Some(ACCT_TALK_A),
                || (mono, wall),
                |_, _, _| Some(Vec::new()),
            )
            .unwrap();
        assert_eq!(read.next_poll(mono), fresh.then_some(until));
        let view = read.progress.unwrap();
        assert_eq!(view.observed_at, retained.observed_at);
        assert_eq!(view.next_at, fresh.then_some(retained.next_at.unwrap()));
        assert_eq!(
            view.phase,
            if fresh {
                ProgressPhase::Waiting
            } else {
                ProgressPhase::Unknown
            }
        );
        if fresh {
            assert_eq!(view.details.first(), retained.details.first());
        } else {
            assert_eq!(
                view.details.first().map(String::as_str),
                Some(EXECUTION_REFRESH_FINDING)
            );
            assert!(view.details.contains(&format!(
                "{EXECUTION_HISTORY_PREFIX}{EXECUTION_PREPARED_FINDING}"
            )));
            assert!(!view.details.iter().any(|s| s == EXECUTION_PREPARED_FINDING));
        }
        assert_eq!(
            idle.kept
                .lock()
                .unwrap()
                .account
                .execution_progress
                .as_ref()
                .unwrap()
                .value,
            retained
        );
        assert_eq!(std::fs::read(a.store().state_file()).unwrap(), repaired);
        assert_eq!(a.asked("restart:"), 0);
        assert_eq!(a.asked("made:"), 0);
    }
    // Once the gate is historical, a current sign-in owns its own future event time.
    let next_activity = retained.next_at.unwrap() + 60;
    let activity_row = a.roster().row("work-2").unwrap().clone();
    for wall in [None, Some(retained.observed_at - 86_400)] {
        let read = host
            .account_progress_read_with_activity(
                sid,
                std::process::id().try_into().unwrap(),
                Some(born),
                Some(ACCT_TALK_A),
                || (until, wall),
                |_, _, _| {
                    Some(vec![aterm_agent::harness::accounts_sync::LoginActivity {
                        queued: None,
                        label: "work-2".into(),
                        identity: Some(
                            aterm_agent::harness::accounts_sync::LoginIdentity::for_test(
                                &activity_row,
                            ),
                        ),
                        route: None,
                        active: true,
                        observed_at: retained.observed_at,
                        next_at: Some(next_activity),
                        diagnostic: None,
                    }])
                },
            )
            .unwrap();
        assert_eq!(read.next_poll(until), Some(until + Duration::from_secs(1)));
        let view = read.progress.unwrap();
        assert!(matches!(view.phase, ProgressPhase::SigningIn { .. }));
        assert_eq!(view.next_at, Some(next_activity));
        assert_eq!(view.observed_at, retained.observed_at);
        assert!(view.details.contains(&format!(
            "{EXECUTION_HISTORY_PREFIX}{EXECUTION_PREPARED_FINDING}"
        )));
    }
}

#[test]
fn the_gui_wakes_once_to_expire_a_settled_repaired_execution_observation() {
    use aterm_agent::harness::account_progress::{
        EXECUTION_HISTORY_PREFIX, EXECUTION_PREPARED_FINDING, EXECUTION_REFRESH_FINDING,
        ProgressPhase,
    };
    use aterm_agent::harness::footer::{FactsOwner, FooterFacts};

    for discard_expiry in [false, true] {
        let mut app = crate::App::headless_for_test();
        let local = app.focused_session_id(crate::WindowId(0)).unwrap();
        let ctx = app.pool.get(local).unwrap().ctx.clone();
        let sid = app.session_sid(local).unwrap();
        let (a, idle, host, born) = execution_fixture(&sid);
        a.store()
            .transact(ACCT_NOW, &mut None, |state| {
                state.prepared_accounts.insert("main".into());
                Ok(Vec::new())
            })
            .unwrap();
        idle.account_execution_hold().unwrap();
        let (retained, until) = {
            let kept = idle.kept.lock().unwrap();
            let bound = kept.account.execution_progress.as_ref().unwrap();
            (bound.value.clone(), bound.execution_until.unwrap())
        };
        a.store()
            .transact(ACCT_NOW, &mut None, |state| {
                state.prepared_accounts.remove("main");
                Ok(Vec::new())
            })
            .unwrap();
        host.note_accounts();
        let bytes = std::fs::read(a.store().state_file()).unwrap();
        let pid = std::process::id().try_into().unwrap();
        {
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(pid);
            timeline.set_program(pid, Some("node".into()));
            timeline.set_agent_reader_program_for_test(Some(Program::Claude));
            timeline.set_claude_footer(
                pid,
                Some(born),
                Some(FooterFacts {
                    owner: Some(FactsOwner {
                        floor: Some(born),
                        session_id: ACCT_TALK_A.into(),
                    }),
                    ..FooterFacts::default()
                }),
            );
        }
        app.harness = Some(host);
        let start = until - Duration::from_secs(2);
        for mono in [start, until, until + Duration::from_secs(1)] {
            let mut reads = 0;
            app.service_harness_account_progress_with_reader(
                mono,
                |host, sid, pid, birth, conversation| {
                    reads += 1;
                    let mut read = host.account_progress_read_with_activity(
                        sid,
                        pid,
                        birth,
                        conversation,
                        || (mono, Some(retained.observed_at - 86_400)),
                        |_, _, _| Some(Vec::new()),
                    )?;
                    if discard_expiry {
                        read.execution_until = None;
                    }
                    Ok(read)
                },
            );
            assert_eq!(reads, 1);
            let value = ctx
                .timeline
                .lock()
                .unwrap()
                .claude_footer()
                .unwrap()
                .account_progress
                .clone()
                .unwrap();
            assert_eq!(value.observed_at, retained.observed_at);
            if mono == start {
                assert_eq!(value.phase, ProgressPhase::Waiting);
                assert_eq!(
                    app.harness_progress_next_poll,
                    (!discard_expiry).then_some(until)
                );
                if discard_expiry {
                    break;
                } // Former scheduling loses the only expiry wake.
                app.service_harness_account_progress_with_reader(
                    until - Duration::from_nanos(1),
                    |_, _, _, _, _| panic!("a settled observation must wait for its one deadline"),
                );
            } else {
                assert_eq!(value.phase, ProgressPhase::Unknown);
                assert_eq!(value.next_at, None);
                assert_eq!(
                    value.details.first().map(String::as_str),
                    Some(EXECUTION_REFRESH_FINDING)
                );
                assert!(value.details.contains(&format!(
                    "{EXECUTION_HISTORY_PREFIX}{EXECUTION_PREPARED_FINDING}"
                )));
                assert_eq!(
                    app.harness_progress_next_poll, None,
                    "expired idle observation arms no repeating timer"
                );
            }
            assert_eq!(std::fs::read(a.store().state_file()).unwrap(), bytes);
            assert_eq!(a.asked("restart:"), 0);
            assert_eq!(a.asked("made:"), 0);
        }
    }
}

#[test]
fn an_execution_hold_keeps_its_reason_without_hiding_current_work() {
    use aterm_agent::harness::account_progress::{EXECUTION_PREPARED_FINDING, ProgressPhase};
    for case in 17..=21 {
        let sid = "s-execution-current-work";
        let (a, idle, host, born) = execution_fixture(sid);
        a.store()
            .transact(ACCT_NOW, &mut None, |state| {
                state.prepared_accounts.insert("main".into());
                Ok(Vec::new())
            })
            .unwrap();
        if case == 20 {
            let mut old = idle.capture_execution_progress().unwrap();
            let mut old_seat = a.seat();
            old_seat.state = old_seat.state.join("old-state");
            old.execution_context(
                &old_seat,
                &accounts::Roster {
                    rows: vec![accounts::Row {
                        label: "unlisted-account".into(),
                        ..Default::default()
                    }],
                },
            );
            idle.kept.lock().unwrap().account.progress = Some(old);
        }
        idle.account_execution_hold().unwrap();
        let retained = idle
            .kept
            .lock()
            .unwrap()
            .account
            .execution_progress
            .as_ref()
            .unwrap()
            .value
            .clone();
        if case != 20 {
            assert!(idle.account(AccountPoint::TurnEnd).is_none());
        }
        assert_eq!(
            execution_view(&host, sid, born).unwrap().phase,
            ProgressPhase::Waiting
        );
        start_execution_work(&a, &idle, case);
        let view = execution_work_view(&a, &host, sid, born, case).unwrap();
        assert_eq!(
            view.phase,
            match case {
                17 => ProgressPhase::Probing,
                18 => ProgressPhase::Switching {
                    to: "work-2".into()
                },
                19 => ProgressPhase::Verifying {
                    to: "work-2".into()
                },
                20 => ProgressPhase::SigningIn {
                    label: "work-2".into(),
                    route: Some("fixture-browser".into())
                },
                _ => ProgressPhase::Waiting,
            }
        );
        assert_eq!(
            view.details.first().map(String::as_str),
            Some(EXECUTION_PREPARED_FINDING)
        );
        assert_eq!((view.known, view.eligible), (None, None));
        assert_eq!(view.next_at, retained.next_at);
        assert_eq!(
            idle.kept
                .lock()
                .unwrap()
                .account
                .execution_progress
                .as_ref()
                .unwrap()
                .value,
            retained
        );
        idle.kept
            .lock()
            .unwrap()
            .snapshot
            .as_mut()
            .unwrap()
            .argv
            .push("--changed-auth-context".into());
        assert!(execution_work_view(&a, &host, sid, born, case).is_none());
        assert_eq!(a.asked("restart:"), 0);
        assert_eq!(a.asked("made:"), 0);
    }
}

#[test]
fn prepared_execution_is_held_with_account_automation_off() {
    use aterm_agent::harness::account_progress::{
        EXECUTION_PREPARED_FINDING, EXECUTION_ROSTER_UNREAD_FINDING, ProgressPhase,
    };
    let sid = "s-execution-off";
    let (a, idle, host, born) = execution_fixture(sid);
    assert!(idle.account_execution_hold().is_none());
    assert!(execution_view(&host, sid, born).is_none());
    a.store()
        .transact(ACCT_NOW, &mut None, |state| {
            state.prepared_accounts.insert("main".into());
            Ok(Vec::new())
        })
        .unwrap();
    assert!(idle.account(AccountPoint::TurnEnd).is_none());
    idle.account_execution_hold().unwrap();
    let observed = execution_view(&host, sid, born).unwrap();
    assert_eq!(observed.phase, ProgressPhase::Waiting);
    assert_eq!(observed.account.as_deref(), Some("main"));
    assert_eq!(
        (observed.known, observed.eligible, observed.wall.as_ref()),
        (None, None, None)
    );
    assert_eq!(
        observed.details,
        [
            EXECUTION_PREPARED_FINDING,
            "Automatic account rotation is off"
        ]
    );
    assert_eq!(
        observed.next_at,
        idle.kept
            .lock()
            .unwrap()
            .account
            .execution_progress
            .as_ref()
            .unwrap()
            .value
            .next_at
    );
    assert!(observed.next_at.unwrap() >= observed.observed_at);
    assert_eq!(a.asked("restart:"), 0);
    assert_eq!(a.asked("made:"), 0);
    a.store()
        .transact(ACCT_NOW, &mut None, |state| {
            state.prepared_accounts.remove("main");
            Ok(Vec::new())
        })
        .unwrap();
    assert!(idle.account_execution_hold().is_none());
    assert!(
        idle.kept
            .lock()
            .unwrap()
            .account
            .execution_progress
            .is_none()
    );
    assert_eq!(
        execution_view(&host, sid, born).unwrap().phase,
        ProgressPhase::Disabled
    );
    std::fs::write(a.roster_path(), "[[account").unwrap();
    assert!(idle.account_execution_hold().is_some());
    let unread = execution_view(&host, sid, born).unwrap();
    assert_eq!(unread.phase, ProgressPhase::Waiting);
    assert_eq!(
        unread.details.first().map(String::as_str),
        Some(EXECUTION_ROSTER_UNREAD_FINDING)
    );
    assert!(unread.wall.is_none());
    assert!(unread.next_at.is_some());
    a.write_roster();
    assert!(idle.account_execution_hold().is_none());
    let machine = a.store().machine_file();
    let saved = std::fs::read(&machine).unwrap();
    std::fs::write(&machine, [0xff]).unwrap();
    assert!(a.store().load(ACCT_NOW).unwrap().1.is_some());
    assert!(idle.account_execution_hold().is_none());
    std::fs::write(machine, saved).unwrap();
}

#[test]
fn execution_observations_reject_changed_process_queries_and_stale_publications() {
    let sid = "s-execution-stale";
    let (a, idle, host, born) = execution_fixture(sid);
    a.store()
        .transact(ACCT_NOW, &mut None, |state| {
            state.prepared_accounts.insert("main".into());
            Ok(Vec::new())
        })
        .unwrap();
    idle.account_execution_hold().unwrap();
    assert!(execution_view(&host, sid, born).is_some());
    for (pid, birth, conversation) in [
        (std::process::id() as i32 + 1, Some(born), Some(ACCT_TALK_A)),
        (std::process::id() as i32, Some(born + 1), Some(ACCT_TALK_A)),
        (
            std::process::id() as i32,
            Some(born),
            Some("other-conversation"),
        ),
        (std::process::id() as i32, None, Some(ACCT_TALK_A)),
    ] {
        assert!(
            host.account_progress(sid, pid, birth, conversation)
                .unwrap()
                .is_none()
        );
    }
    let old = idle.capture_execution_progress();
    idle.kept.lock().unwrap().account.landing = Arc::default();
    assert!(execution_view(&host, sid, born).is_none());
    idle.account_execution_hold().unwrap();
    let current = execution_view(&host, sid, born).unwrap();
    idle.publish_execution_progress(old.clone(), Some(Instant::now()), "stale hold", None);
    assert_eq!(execution_view(&host, sid, born), Some(current.clone()));
    idle.publish_execution_progress(old, None, "unused", None);
    assert_eq!(execution_view(&host, sid, born), Some(current));
    idle.kept.lock().unwrap().snapshot.as_mut().unwrap().session = Some("new-conversation".into());
    assert!(execution_view(&host, sid, born).is_none());
    assert!(
        host.account_progress(
            sid,
            std::process::id().try_into().unwrap(),
            Some(born),
            Some("new-conversation")
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(a.asked("restart:"), 0);
    assert_eq!(a.asked("made:"), 0);
}

#[test]
fn unconfirmed_birth_exposes_only_a_generic_context_hold_to_an_unknown_birth_reader() {
    use aterm_agent::harness::account_progress::{EXECUTION_CONTEXT_UNREAD_FINDING, ProgressPhase};
    let sid = "s-execution-unknown-birth";
    let (a, idle, host, born) = execution_fixture(sid);
    a.store()
        .transact(ACCT_NOW, &mut None, |state| {
            state.prepared_accounts.insert("main".into());
            Ok(Vec::new())
        })
        .unwrap();
    idle.kept.lock().unwrap().snapshot.as_mut().unwrap().start = "unconfirmed-process-start".into();
    {
        let mut kept = idle.kept.lock().unwrap();
        kept.account.flying = Some(pending_move(kept.snapshot.as_ref().unwrap()));
    }
    idle.account_execution_hold().unwrap();
    let pid = std::process::id().try_into().unwrap();
    let read = || {
        host.account_progress_with_activity(
            sid,
            pid,
            None,
            Some(ACCT_TALK_A),
            || execution_clock(&host, sid, false),
            aterm_agent::harness::accounts_sync::login_activity,
        )
        .unwrap()
    };
    let view = read().unwrap();
    assert_eq!(view.phase, ProgressPhase::Waiting);
    assert_eq!(
        view.details.first().map(String::as_str),
        Some(EXECUTION_CONTEXT_UNREAD_FINDING)
    );
    assert_eq!(
        (view.account, view.known, view.eligible, view.wall),
        (None, None, None, None)
    );
    assert!(view.next_at.is_some());
    assert!(
        host.account_progress_with_activity(
            sid,
            pid,
            None,
            Some(ACCT_TALK_A),
            || execution_clock(&host, sid, false),
            |_, _, _| panic!("unknown birth must not read attributed sign-in activity")
        )
        .unwrap()
        .is_some()
    );
    assert!(execution_view(&host, sid, born).is_none());
    idle.kept.lock().unwrap().account.landing = Arc::default();
    assert!(read().is_none());
    idle.account_execution_hold().unwrap();
    assert!(read().is_some());
    idle.kept
        .lock()
        .unwrap()
        .snapshot
        .as_mut()
        .unwrap()
        .argv
        .push("--changed-auth-context".into());
    assert!(read().is_none());
    assert_eq!(a.asked("restart:"), 0);
    assert_eq!(a.asked("made:"), 0);
}

#[test]
fn execution_hold_defers_in_flight_and_changed_unmanaged_contexts() {
    let a = Accts::new();
    a.spawn("s-execution-context", ACCT_TALK_A, "main");
    let mut cfg = on();
    cfg.set("accounts", "off").unwrap();
    cfg.set("logins", "off").unwrap();
    let mut idle = a.worker("s-execution-context", &cfg);
    {
        let mut kept = idle.kept.lock().unwrap();
        kept.account.flying = Some(pending_move(kept.snapshot.as_ref().unwrap()));
    }
    assert!(idle.account_execution_hold().is_some());
    idle.kept.lock().unwrap().account.flying = None;
    assert!(idle.account_execution_hold().is_none());

    let original = Arc::clone(&idle.hooks.acts.account_env);
    let kept = Arc::clone(&idle.kept);
    let change = Arc::new(AtomicBool::new(false));
    let changed = Arc::clone(&change);
    idle.hooks.acts.account_env = Arc::new(move |snap| {
        let mut env = original(snap)?;
        env.retain(|entry| !entry.starts_with(&format!("{}=", accounts::LOGIN_ENV)));
        env.push(format!("{}=/unlisted/account", accounts::LOGIN_ENV));
        if changed.load(Ordering::SeqCst) {
            kept.lock().unwrap().account.landing = Arc::default();
        }
        Some(env)
    });
    assert!(idle.account_execution_hold().is_none());
    change.store(true, Ordering::SeqCst);
    assert!(idle.account_execution_hold().is_some());
}

#[test]
fn a_mismatched_restart_preserves_the_current_advice() {
    let (a, idle) = ready("s-advice-mismatch");
    let before = idle.kept.lock().unwrap().account.advised.clone();
    assert_eq!(
        idle.restart_once(&move_to("pool-a", AccountAt::Wall))
            .as_deref(),
        Some("wait:account-changed")
    );
    assert_eq!(idle.kept.lock().unwrap().account.advised, before);
    assert_eq!(a.asked("restart:"), 0);
    assert_eq!(
        idle.restart_once(&move_to("work-2", AccountAt::Wall))
            .as_deref(),
        Some("adopted")
    );
    assert_eq!(a.asked("restart:"), 1);
}

#[test]
fn a_busy_landing_gate_preserves_advice_for_retry() {
    let (a, idle) = ready("s-advice-busy");
    let before = idle.kept.lock().unwrap().account.advised.clone();
    let gate = Arc::clone(&idle.kept.lock().unwrap().account.landing);
    let held = gate.lock().unwrap();
    assert_eq!(
        idle.restart_once(&move_to("work-2", AccountAt::Wall))
            .as_deref(),
        Some("wait:account-unverified")
    );
    assert_eq!(idle.kept.lock().unwrap().account.advised, before);
    assert_eq!(a.asked("restart:"), 0);
    drop(held);
    assert_eq!(
        idle.restart_once(&move_to("work-2", AccountAt::Wall))
            .as_deref(),
        Some("adopted")
    );
    assert_eq!(a.asked("restart:"), 1);
}

#[test]
fn an_old_calculation_cannot_clear_a_new_generations_advice() {
    let (a, mut idle) = ready("s-advice-old-publish");
    let original = Arc::clone(&idle.hooks.acts.account_env);
    let kept = Arc::clone(&idle.kept);
    let replaced = AtomicBool::new(false);
    idle.hooks.acts.account_env = Arc::new(move |snap| {
        let env = original(snap);
        if !replaced.swap(true, Ordering::SeqCst) {
            kept.lock().unwrap().account.landing = Arc::default();
            let request = AdviceTurn::begin(&kept).capture(&kept).unwrap();
            let _ = request.publish(
                &kept,
                Some(AccountAdvice::Move {
                    to: "pool-a".into(),
                    yields: None,
                }),
                AccountPoint::TurnEnd,
            );
        }
        env
    });
    assert_eq!(idle.account(usage_wall()), None);
    let k = idle.kept.lock().unwrap();
    let advised = k
        .account
        .advised
        .as_ref()
        .expect("the new generation's advice");
    assert_eq!(advised.to, "pool-a");
    assert_eq!(advised.point, AccountPoint::TurnEnd);
    assert_eq!(a.asked("restart:"), 0);
}

fn move_advice(to: &str) -> Option<AccountAdvice> {
    Some(AccountAdvice::Move {
        to: to.into(),
        yields: None,
    })
}

fn publish_label(kept: &Mutex<Kept>, to: Option<&str>) -> Option<AccountAdvice> {
    AdviceTurn::begin(kept).capture(kept).unwrap().publish(
        kept,
        to.and_then(move_advice),
        usage_wall(),
    )
}

fn advice_label(advice: Option<AccountAdvice>) -> i64 {
    match advice {
        None => 0,
        Some(AccountAdvice::Move { to, .. }) if to == "first" => 1,
        Some(AccountAdvice::Move { to, .. }) if to == "second" => 2,
        _ => unreachable!(),
    }
}

fn queue_state(job: &WorkerJob, returned: i64, taken: bool, cancelled: bool) -> [i64; 5] {
    let k = job.kept.lock().unwrap();
    let (queued, usable) = k.account.advised.as_ref().map_or((0, false), |a| {
        (
            if a.to == "first" {
                1
            } else {
                assert_eq!(a.to, "second");
                2
            },
            a.request.matches_context(&k),
        )
    });
    [
        returned,
        queued,
        i64::from(taken),
        i64::from(usable),
        i64::from(cancelled),
    ]
}

fn pending_move(snap: &Snapshot) -> Flying {
    Flying {
        origin: AccountContext {
            roster: None,
            state: "/example/state".into(),
            home: "/example".into(),
        },
        to: "first".into(),
        at: AccountAt::Wall,
        login: snap.login.clone(),
        pid: snap.pid,
        start: snap.start.clone(),
        rejected: None,
    }
}

fn queue_case(case: i64) -> [i64; 5] {
    let job = left_job("s-advice-model", Program::Claude, &on());
    job.kept.lock().unwrap().snapshot = Some(snap(&job.sid));
    let turn = AdviceTurn::begin(&job.kept);
    if case == 14 {
        assert_eq!(advice_label(publish_label(&job.kept, Some("second"))), 2);
        assert!(
            turn.capture(&job.kept).is_none(),
            "an older preflight cannot overtake"
        );
        return queue_state(&job, 0, false, false);
    }
    if case == 15 {
        job.kept
            .lock()
            .unwrap()
            .snapshot
            .as_mut()
            .unwrap()
            .argv
            .push("--preflight-context".into());
    }
    if case == 16 {
        job.kept.lock().unwrap().snapshot = None;
    }
    let request = turn.capture(&job.kept).unwrap();
    if matches!(case, 2..=5) {
        if case == 5 {
            job.kept.lock().unwrap().account.landing = Arc::default();
        }
        let _ = publish_label(&job.kept, (case != 4).then_some("second"));
        let returned = request.publish(
            &job.kept,
            if case == 3 {
                None
            } else {
                move_advice("first")
            },
            usage_wall(),
        );
        return queue_state(&job, advice_label(returned), false, false);
    }
    if case == 6 {
        job.kept
            .lock()
            .unwrap()
            .snapshot
            .as_mut()
            .unwrap()
            .argv
            .push("--other-auth".into());
    }
    if case == 7 {
        job.kept.lock().unwrap().snapshot.as_mut().unwrap().session = Some("new-record".into());
    }
    if case == 21 {
        let mut k = job.kept.lock().unwrap();
        k.account.flying = Some(pending_move(k.snapshot.as_ref().unwrap()));
    }
    let mut returned = advice_label(request.publish(&job.kept, move_advice("first"), usage_wall()));
    if case == 17 {
        returned = advice_label(publish_label(&job.kept, None));
    }
    let target = if case == 8 { "second" } else { "first" };
    let peek = with_kept(&job, |k| advised_for(k, target, case != 9));
    if case == 10 {
        let _ = publish_label(&job.kept, Some("second"));
    }
    if case == 11 {
        job.kept.lock().unwrap().snapshot = None;
    }
    if case == 12 {
        job.kept.lock().unwrap().snapshot.as_mut().unwrap().login =
            Some("/example/new-login".into());
    }
    if case == 18 {
        job.kept.lock().unwrap().account.landing = Arc::default();
    }
    if case == 19 {
        job.kept.lock().unwrap().snapshot.as_mut().unwrap().cwd = "/example/new-directory".into();
    }
    let pending = (case == 20).then(|| AdviceTurn::begin(&job.kept));
    let taken = if matches!(case, 8..=13 | 18..=20) {
        peek.is_some_and(|a| {
            let gate = Arc::clone(&a.request.turn.generation);
            let _held = gate.lock().unwrap();
            take_advised(&job.kept, &a).is_some()
        })
    } else {
        false
    };
    let cancelled = pending.is_some_and(|turn| turn.capture(&job.kept).is_none());
    queue_state(&job, returned, taken, cancelled)
}

#[test]
fn a_pending_move_still_allows_wait_advice() {
    let job = left_job("s-advice-wait", Program::Claude, &on());
    let snap = snap(&job.sid);
    {
        let mut k = job.kept.lock().unwrap();
        k.account.flying = Some(pending_move(&snap));
        k.snapshot = Some(snap);
    }
    let wait = AccountAdvice::Wait {
        until: Instant::now(),
    };
    let request = AdviceTurn::begin(&job.kept).capture(&job.kept).unwrap();
    assert_eq!(
        request.publish(&job.kept, Some(wait.clone()), usage_wall()),
        Some(wait)
    );
    assert!(job.kept.lock().unwrap().account.advised.is_none());
}

pub(crate) fn queue_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Choose" => {
            next.insert("phase", 1);
        }
        "Apply" => {
            next.insert("phase", 2);
            for (key, value) in ["returned", "queued", "taken", "usable", "cancelled"]
                .into_iter()
                .zip(queue_case(state["case"]))
            {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn advice_queue_conforms_across_publication_and_claim_races() {
    let model = aterm_models_harness::account_advice_queue_model();
    assert_eq!(model.mutant_dials(), (1..=17).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 20);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = queue_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, key, value, law) in [
        (2, "queued", 1, "QueueIdentity"),
        (6, "returned", 1, "CurrentPublication"),
        (8, "taken", 1, "CurrentClaim"),
        (18, "usable", 1, "QueueContext"),
        (20, "cancelled", 0, "ClaimInvalidatesProducer"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("case", case);
        let mut bad = queue_transition(&state, "Apply");
        bad.insert(key, value);
        assert!(!model.check_invariant(law, &bad));
    }
}
