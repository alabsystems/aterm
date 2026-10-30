// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn switching_one_agent_off_fences_an_inflight_idle_read() {
    switch_off_during_idle(false);
}

#[test]
fn switching_one_agent_off_discards_an_inflight_upgrade_result() {
    switch_off_during_idle(true);
}

fn release_after_edit(release: &(Mutex<bool>, Condvar), entered: &std::sync::mpsc::Sender<()>) {
    entered.send(()).unwrap();
    let (lock, wake) = release;
    let (open, timeout) = wake
        .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(60), |open| !*open)
        .unwrap();
    assert!(
        *open && !timeout.timed_out(),
        "the test releases the in-flight operation"
    );
}

fn switch_off_during_idle(in_step: bool) {
    let world = Arc::new(World::default());
    world.set(&[("off", Program::Claude), ("other", Program::Agy)]);
    let reading = Arc::new(AtomicBool::new(false));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let advance = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (other_tx, other_rx) = std::sync::mpsc::channel();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let steps = Arc::new(Mutex::new(Vec::new()));
    let body: Arc<BodyFn> = {
        let reading = Arc::clone(&reading);
        let advance = Arc::clone(&advance);
        Arc::new(move |job| {
            let me = std::thread::current();
            let parked = me.clone();
            *job.interrupt.lock().unwrap() = Some(Box::new(move || parked.unpark()));
            if job.sid == "off" {
                reading.store(true, Ordering::SeqCst);
            } else {
                other_tx.send(me).unwrap();
                while !advance.load(Ordering::SeqCst) && !job.stop.load(Ordering::SeqCst) {
                    std::thread::park();
                }
            }
            let answer = job.opts.idle_host.as_ref().unwrap().at_idle();
            result_tx.send((job.sid.clone(), answer.is_some())).unwrap();
            while !job.stop.load(Ordering::SeqCst) {
                std::thread::park();
            }
            BodyEnd::Stopped
        })
    };
    let mut h = hooks(&world, body);
    h.acts.due = {
        let release = Arc::clone(&release);
        let entered = entered_tx.clone();
        Arc::new(move |sid| {
            if sid == "off" {
                if !reading.load(Ordering::SeqCst) {
                    return Due::No;
                }
                if !in_step {
                    release_after_edit(&release, &entered);
                }
            }
            Due::Yes
        })
    };
    h.acts.step = {
        let steps = Arc::clone(&steps);
        let release = Arc::clone(&release);
        Arc::new(move |sid, _, _| {
            steps.lock().unwrap().push(sid.to_string());
            if sid == "off" && in_step {
                release_after_edit(&release, &entered_tx);
            }
            "wait:point".to_string()
        })
    };
    let host = HostHandle::start(on(), false, false, h);
    entered_rx.recv_timeout(Duration::from_secs(60)).unwrap();
    let other = other_rx.recv_timeout(Duration::from_secs(60)).unwrap();
    let mut cfg = on();
    cfg.set("claude.enabled", "false").unwrap();
    host.set_config(cfg);
    let stops: Vec<_> = host
        .shared
        .lock()
        .stops
        .iter()
        .map(|s| (s.agent, s.stop.load(Ordering::SeqCst)))
        .collect();
    advance.store(true, Ordering::SeqCst);
    other.unpark();
    {
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
    let results: HashMap<_, _> = (0..2)
        .map(|_| result_rx.recv_timeout(Duration::from_secs(60)).unwrap())
        .collect();
    host.shutdown_and_join();
    assert!(
        stops.contains(&(Program::Claude, true)),
        "off synchronously stops Claude Code"
    );
    assert!(
        stops.contains(&(Program::Agy, false)),
        "the other agent's existing worker keeps running"
    );
    let steps = steps.lock().unwrap();
    assert_eq!(
        steps.iter().filter(|s| *s == "off").count(),
        usize::from(in_step),
        "a late read starts no upgrade; an already-started request runs once"
    );
    assert_eq!(
        results.get("off"),
        Some(&false),
        "the switched-off agent's late result is discarded"
    );
    assert!(
        results.contains_key("other"),
        "the other agent's worker still reaches its idle point"
    );
}

#[test]
fn switching_off_revokes_a_restart_exit_read_already_inflight() {
    for commit in [false, true] {
        revoke_exit_read(commit);
    }
}

fn revoke_exit_read(commit: bool) {
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert("exit".to_string());
    a.set(&[("exit", Program::Claude)]);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let carries = Arc::new(AtomicUsize::new(0));
    let mut h = a.hooks();
    h.acts.restarted = Arc::new(|_, _, _| true);
    h.acts.status = {
        let release = Arc::clone(&release);
        Arc::new(move |_| {
            release_after_edit(&release, &entered_tx);
            Some("OK hold=0 hand=- human_ms=- supervisor=-".to_string())
        })
    };
    h.acts.carry = {
        let carries = Arc::clone(&carries);
        Arc::new(move |_, _, _, _, _| {
            carries.fetch_add(1, Ordering::SeqCst);
            "done".to_string()
        })
    };
    let host = HostHandle::start(on(), false, false, h);
    a.attached(&host, "exit");
    a.set(&[]);
    entered_rx.recv_timeout(Duration::from_secs(60)).unwrap();
    if commit {
        host.suspend();
    }
    let mut off = on();
    off.set("claude.enabled", "false").unwrap();
    host.set_config(off);
    host.set_config(on());
    if commit {
        host.resume();
    }
    {
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
    until("the revoked exit worker is reaped", || {
        host.live().is_empty()
    });
    host.shutdown_and_join();
    assert_eq!(
        carries.load(Ordering::SeqCst),
        0,
        "no restart override after revocation"
    );
    assert!(a.relaunched.lock().unwrap().is_empty());
}

#[test]
fn switching_off_revokes_a_cold_relaunch_read_already_inflight() {
    let world = Arc::new(World::default());
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let relaunched = Arc::new(AtomicUsize::new(0));
    let mut h = hooks(&world, parking_body(&world));
    h.acts.account_cold = {
        let release = Arc::clone(&release);
        Arc::new(move |_| {
            release_after_edit(&release, &entered_tx);
            None
        })
    };
    h.acts.relaunch_restored = {
        let relaunched = Arc::clone(&relaunched);
        Arc::new(move |_, _, _, _| {
            relaunched.fetch_add(1, Ordering::SeqCst);
            "done".to_string()
        })
    };
    let host = HostHandle::start(on(), false, false, h);
    host.relaunch_restored(vec![restored("cold", "in tab 1")]);
    entered_rx.recv_timeout(Duration::from_secs(60)).unwrap();
    let mut off = on();
    off.set("claude.enabled", "false").unwrap();
    host.set_config(off);
    host.set_config(on());
    {
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
    until("the revoked restored queue is finished", || {
        host.restored_worker_ended()
    });
    host.shutdown_and_join();
    assert_eq!(
        relaunched.load(Ordering::SeqCst),
        0,
        "the revoked read starts no relaunch"
    );
}

#[test]
fn an_off_on_edit_revokes_a_policy_handover_before_the_body_returns() {
    for revoke in [false, true] {
        let (host, job, workers, hooks) = revocation_fixture();
        workers.0["revoked"].hand_over();
        assert!(job.handover.load(Ordering::SeqCst));
        if revoke {
            off_and_on(&host);
        }
        stopped(&job, &hooks);
        let exits = unwatched_exits(
            &host.shared,
            &Workers::default(),
            &HashMap::new(),
            &OffSessions::default(),
            &hooks,
        );
        let handed_over = job.handover.load(Ordering::SeqCst);
        end_exit_watches(&host.shared, EVERY_WATCH, |_| true);
        host.shutdown_and_join();
        assert_eq!(
            exits.is_empty(),
            revoke,
            "only an ordinary handover leaves an exit claim"
        );
        assert_eq!(
            handed_over, !revoke,
            "revocation also lets the old loop clear its badges"
        );
    }
}

#[test]
fn an_off_on_edit_revokes_an_inflight_exit_attempt_result() {
    let (host, job, _workers, mut hooks) = revocation_fixture();
    job.left.hand();
    with_kept(&job, |k| {
        k.owed = Some(OwedRelaunch {
            snap: snap("revoked"),
            asked: None,
        })
    });
    hooks.acts.snapshot = Arc::new(|sid| {
        Some(Snapshot {
            pid: 5151,
            ..snap(sid)
        })
    });
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let released = Arc::clone(&release);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let kept = Arc::clone(&job.kept);
    let run = std::thread::spawn(move || {
        relaunch_until(
            &job,
            &hooks,
            Duration::ZERO,
            &|| OnExit::Restarted,
            Some(&|| 120),
            || {
                release_after_edit(&released, &entered_tx);
                "adopted".to_string()
            },
        );
    });
    entered_rx.recv_timeout(Duration::from_secs(60)).unwrap();
    off_and_on(&host);
    {
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
    run.join().unwrap();
    let rearmed = with_kept_of(&kept, |k| {
        k.unwatched.is_some()
            || k.exit_watch
                .as_ref()
                .is_some_and(|watch| watch.hands.is_some())
    });
    end_exit_watches(&host.shared, EVERY_WATCH, |_| true);
    host.shutdown_and_join();
    assert!(
        !rearmed,
        "the late result cannot rearm the revoked exit claim"
    );
}

#[test]
fn an_off_on_edit_revokes_an_orphaned_landing_claim() {
    let (host, job, _workers, hooks) = revocation_fixture();
    runs_again(&job, &hooks, true);
    publish_workers(&mut host.shared.lock(), &Workers::default());
    assert!(with_kept(&job, |k| k
        .exit_watch
        .as_ref()
        .is_some_and(|w| w.hands.is_some())));
    off_and_on(&host);
    let rearmed = with_kept(&job, |k| {
        k.unwatched.is_some()
            || k.exit_watch
                .as_ref()
                .is_some_and(|watch| watch.hands.is_some())
    });
    end_exit_watches(&host.shared, EVERY_WATCH, |_| true);
    host.shutdown_and_join();
    assert!(
        !rearmed,
        "off revokes kept claims after their worker was reaped"
    );
}

#[test]
fn an_off_on_edit_revokes_an_inflight_restored_landing_read() {
    let world = Arc::new(World::default());
    let mut hooks = hooks(&world, Arc::new(|_| BodyEnd::Stopped));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let released = Arc::clone(&release);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    hooks.acts.relaunch_restored = Arc::new(|_, _, _, _| "adopted".to_string());
    hooks.acts.exited = Arc::new(|_, _| true);
    hooks.acts.snapshot = Arc::new(move |sid| {
        release_after_edit(&released, &entered_tx);
        Some(Snapshot {
            pid: 5151,
            ..snap(sid)
        })
    });
    let host = HostHandle::start(on(), false, true, hooks);
    host.relaunch_restored(vec![restored("landing", "in tab 1")]);
    entered_rx.recv_timeout(Duration::from_secs(60)).unwrap();
    off_and_on(&host);
    {
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
    until("the revoked restored landing read is finished", || {
        host.restored_worker_ended()
    });
    let rearmed = host.shared.kept.lock().unwrap().values().any(|kept| {
        with_kept_of(kept, |k| {
            k.exit_watch.as_ref().is_some_and(|w| w.hands.is_some())
        })
    });
    end_exit_watches(&host.shared, EVERY_WATCH, |_| true);
    host.shutdown_and_join();
    assert!(
        !rearmed,
        "a removed restored attempt cannot rearm from its late snapshot"
    );
}

#[test]
fn a_handed_snapshot_cannot_admit_an_agent_disabled_at_startup() {
    let world = Arc::new(World::default());
    let mut hooks = hooks(&world, Arc::new(|_| BodyEnd::Stopped));
    hooks.acts.exited = Arc::new(|_, _| true);
    let mut off = on();
    off.off.push(Program::Claude);
    let host = HostHandle::start(off, false, true, hooks);
    host.watch_handed(vec![("handed".to_string(), snap("handed"))]);
    host.set_config(on());
    let admitted = host.shared.kept.lock().unwrap().values().any(|kept| {
        with_kept_of(kept, |k| {
            k.unwatched.is_some() || k.exit_watch.as_ref().is_some_and(|w| w.hands.is_some())
        })
    });
    end_exit_watches(&host.shared, EVERY_WATCH, |_| true);
    host.shutdown_and_join();
    assert!(
        !admitted,
        "a disabled handoff snapshot leaves no future exit claim"
    );
}

#[test]
fn claude_code_switched_off_turns_the_accounts_off() {
    let under = |cfg: &SupervisorConfig| {
        let s = Switches::default();
        s.set(cfg, false);
        (s.accounts(), s.logins(), s.accounts_live())
    };
    let switched_off = |agent: Program| {
        let mut cfg = on();
        cfg.off.push(agent);
        cfg
    };
    let full = under(&on());
    assert!(
        full.0 != accounts::Mode::Off && full.1 && full.2,
        "{full:?}"
    );
    assert_eq!(
        under(&switched_off(Program::Claude)),
        (accounts::Mode::Off, false, false)
    );
    assert_eq!(under(&switched_off(Program::Codex)), full);
}

#[test]
fn the_reopened_row_asks_each_agents_admission() {
    use aterm_agent::harness::upgrade::Agent;
    let of = |cfg: SupervisorConfig| {
        let world = Arc::new(World::default());
        let host = HostHandle::start(cfg, false, true, hooks(&world, parking_body(&world)));
        let admitted = Agent::ALL.map(|agent| host.admits(agent.program()));
        let told = Agent::ALL.map(|agent| host.relaunches_restored_of(agent));
        host.shutdown_and_join();
        (admitted, told)
    };
    let mut codex_off = on();
    codex_off.off.push(Program::Codex);
    let ([_, _, lanes @ ..], told) = of(codex_off);
    assert_eq!(lanes, [true; 2], "PRECONDITION: agy, Muse Code admitted");
    assert_eq!(told, [true, false, true, true]);
    assert_eq!(of(on()).1, [true; 4]);
    let mut muse_off = on();
    muse_off.off.push(Program::Muse);
    assert_eq!(of(muse_off).1, [true, true, true, false]);
    let mut agy_off = on();
    agy_off.off.push(Program::Agy);
    assert_eq!(of(agy_off).1, [true, true, false, true]);
    let mut limited = on();
    limited.set("relaunch", "false").unwrap();
    assert_eq!(of(limited).1, [false; 4]);
}

#[test]
fn an_agent_switched_off_in_the_tab_ends_the_exit_left_to_the_host() {
    for codex in [true, false] {
        let a = Arc::new(Acting::default());
        a.only_claude_sweeps.store(true, Ordering::SeqCst);
        a.open.lock().unwrap().insert("s-a".to_string());
        a.set(&[("s-a", Program::Claude)]);
        let mut h = a.hooks();
        h.acts.relaunch_restored = Arc::new(|_, _, _, _| "refused:inert".to_string());
        let (world, line) = (Arc::clone(&a), Arc::clone(&h.acts.relaunch));
        h.acts.relaunch = Arc::new(move |sid, pid, snap, left, upgrade, asked| {
            if world.agent_in(sid).is_some_and(|(now, _)| now != snap.pid) {
                return "wait:shell-prompt".to_string();
            }
            line(sid, pid, snap, left, upgrade, asked)
        });
        let mut codex_off = on();
        codex_off.off.push(Program::Codex);
        let host = HostHandle::start(codex_off, false, false, h);
        until("attached", || {
            host.live() == ["s-a"] && a.runs.load(Ordering::SeqCst) == 1
        });
        let marked = || {
            let kept = host.shared.kept.lock().unwrap().get("s-a").cloned();
            kept.is_some_and(|k| k.lock().unwrap().unwatched.is_some())
        };
        let passes = |n: usize, what: &str, idle: bool| {
            let from = a.passes.load(Ordering::SeqCst);
            until(what, || {
                ring();
                a.passes.load(Ordering::SeqCst) >= from + n && (!idle || host.live().is_empty())
            });
        };
        host.pause_restored();
        host.suspend();
        until("the Commit's stop left the exit to the host", || {
            host.live().is_empty() && marked()
        });
        *a.holds.lock().unwrap() = Some(false);
        if codex {
            a.set(&[("s-a", Program::Codex)]);
        } else {
            a.set(&[]);
        }
        host.resume();
        host.resume_restored();
        host.relaunch_stranded(&|_| "in tab 1".to_string());
        if codex {
            until("the exit read", || a.exit_looks.load(Ordering::SeqCst) > 0);
            passes(2, "two passes with the Codex in front", false);
            a.set(&[]);
            passes(2, "two passes after it exits, no worker left", true);
            assert_eq!(
                (a.relaunched.lock().unwrap().len(), marked()),
                (0, false),
                "(relaunched, marked)"
            );
        } else {
            until("the exit relaunched, its worker ended", || {
                !a.relaunched.lock().unwrap().is_empty() && host.live().is_empty()
            });
            assert_eq!(
                (
                    a.exit_looks.load(Ordering::SeqCst) > 0,
                    a.relaunched.lock().unwrap().len()
                ),
                (true, 1),
                "(an exit read, relaunched)"
            );
        }
        host.shutdown_and_join();
    }
}

fn off_and_on(host: &HostHandle) {
    let cfg = host.shared.lock().cfg.clone();
    let mut off = cfg.clone();
    off.off.push(Program::Claude);
    host.set_config(off);
    host.set_config(cfg);
}

fn revocation_fixture() -> (HostHandle, WorkerJob, Workers, Hooks) {
    let world = Arc::new(World::default());
    let mut hooks = hooks(&world, Arc::new(|_| BodyEnd::Stopped));
    hooks.acts.open = Arc::new(|_| true);
    hooks.acts.exited = Arc::new(|_, _| true);
    let host = HostHandle::start(on(), false, true, hooks.clone());
    let mut worker = idle_worker();
    worker.agent = Program::Claude;
    let job = WorkerJob {
        revoked: Arc::clone(&worker.revoked),
        sid: "revoked".to_string(),
        agent: Program::Claude,
        opts: SuperviseOpts::hosted(),
        stop: Arc::clone(&worker.stop),
        interrupt: Arc::clone(&worker.interrupt),
        handover: Arc::clone(&worker.handover),
        clear_badge: false,
        unset_mark: false,
        faults: Arc::default(),
        park: Arc::clone(&worker.park),
        look_at: Arc::clone(&worker.look_at),
        left: Arc::clone(&worker.left),
        left_at_commit: Arc::clone(&worker.left_at_commit),
        acting: Arc::clone(&worker.acting),
        switches: Arc::clone(&host.shared.switches),
        kept: Arc::clone(&worker.kept),
        note: Arc::clone(&worker.note),
        group: Arc::clone(&worker.group),
        host: Arc::downgrade(&host.shared),
        watch_until: None,
        beside: Arc::default(),
    };
    with_kept(&job, |k| k.snapshot = Some(snap("revoked")));
    host.shared
        .kept
        .lock()
        .unwrap()
        .insert(job.sid.clone(), Arc::clone(&job.kept));
    let workers = Workers(HashMap::from([(job.sid.clone(), worker)]));
    publish_workers(&mut host.shared.lock(), &workers);
    (host, job, workers, hooks)
}

#[test]
fn a_worker_is_refused_exactly_where_the_host_would_not_carry_it() {
    const WORKER: &str = "s-worker";
    let codex = Some(("s-codex", Program::Codex, None));
    let opted = Some(("s-opted", Program::Claude, Some(Autopilot::Cruise)));
    let table = |agent: Program| format!("[harness.{}]\nenabled = false\n", agent.name());
    let switch = |agent: Program| format!("[harness.{}] enabled = false", agent.name());
    for agent in [Program::Claude, Program::Agy, Program::Muse] {
        let other = if agent == Program::Claude {
            Program::Muse
        } else {
            Program::Claude
        };
        for (text, headless, refused, beside) in [
            (
                "[harness]\nenabled = false\n".to_string(),
                false,
                Some("[harness] enabled = false".to_string()),
                None,
            ),
            (
                "[harness]\nheadless = false\n".to_string(),
                true,
                Some("[harness] headless = false".to_string()),
                None,
            ),
            (table(agent), false, Some(switch(agent)), codex),
            (
                "[harness]\nautopilot = \"off\"\n".to_string(),
                false,
                Some("[harness] autopilot = \"off\"".to_string()),
                opted,
            ),
            (String::new(), false, None, None),
            (String::new(), true, None, None),
            (
                "[harness]\nheadless = false\n".to_string(),
                false,
                None,
                None,
            ),
            (table(Program::Codex), false, None, None),
            (table(other), false, None, None),
            (
                "[harness]\nautopilot = \"land\"\n".to_string(),
                false,
                None,
                None,
            ),
        ] {
            let case = format!("{agent:?}: {text:?} headless={headless}");
            let (cfg, notes) = SupervisorConfig::from_aterm_toml(&text);
            assert!(notes.is_empty(), "{case}: {notes:?}");
            assert_eq!(
                worker_unsupervised(&cfg, headless, agent),
                refused.as_deref(),
                "{case}"
            );
            let world = Arc::new(World::default());
            let mut roster = vec![(WORKER, agent)];
            if let Some((sid, program, word)) = beside {
                roster.push((sid, program));
                world.word(sid, word);
            }
            world.set(&roster);
            let host = HostHandle::start(cfg, headless, false, hooks(&world, parking_body(&world)));
            match (refused, beside) {
                (None, _) => until(&format!("{case}: the worker carried"), || {
                    host.live() == [WORKER]
                }),
                (Some(_), Some((sid, ..))) => {
                    // Its worker is live before its body has run: wait for both.
                    until(&format!("{case}: {sid} carried beside it"), || {
                        host.live() == [sid] && world.runs.load(Ordering::SeqCst) == 1
                    });
                    world.passes();
                    assert_eq!(host.live(), [sid], "{case}: no worker for {WORKER}");
                    assert_eq!(world.runs.load(Ordering::SeqCst), 1, "{case}");
                }
                (Some(_), None) => {
                    assert!(!host.supervising(), "{case}: no host at all");
                    assert!(host.live().is_empty(), "{case}: {:?}", host.live());
                    assert_eq!(world.runs.load(Ordering::SeqCst), 0, "{case}");
                }
            }
            host.shutdown_and_join();
        }
    }
}

/// A CODEX WORKER IS REFUSED where nothing in this instance would see its task through:
/// the master switch, headless, its own `[harness.codex]` switch, Autopilot off — never by
/// another agent's switch. codex-alab carries itself, so the host carries no codex: the
/// refusal is the gate's alone.
#[test]
fn a_codex_worker_is_refused_exactly_where_its_switches_say() {
    let table = |agent: Program| format!("[harness.{}]\nenabled = false\n", agent.name());
    for (text, headless, refused) in [
        (
            "[harness]\nenabled = false\n".to_string(),
            false,
            Some("[harness] enabled = false"),
        ),
        (
            "[harness]\nheadless = false\n".to_string(),
            true,
            Some("[harness] headless = false"),
        ),
        (
            table(Program::Codex),
            false,
            Some("[harness.codex] enabled = false"),
        ),
        (
            "[harness]\nautopilot = \"off\"\n".to_string(),
            false,
            Some("[harness] autopilot = \"off\""),
        ),
        (table(Program::Claude), false, None),
        (String::new(), false, None),
        (String::new(), true, None),
    ] {
        let (cfg, notes) = SupervisorConfig::from_aterm_toml(&text);
        assert!(notes.is_empty(), "{text:?}: {notes:?}");
        assert_eq!(
            worker_unsupervised(&cfg, headless, Program::Codex),
            refused,
            "{text:?} headless={headless}"
        );
    }
}

#[test]
fn an_agent_switched_off_overtakes_a_restored_relaunch_of_its_tab() {
    for named in [true, false] {
        let a = Arc::new(Acting::default());
        a.open.lock().unwrap().insert("s-a".to_string());
        let mut h = a.hooks();
        h.acts.relaunch_restored = Arc::new(|_, _, _, _| {
            hold_this_steps_retry();
            "wait:shell-prompt".to_string()
        });
        h.pause = holding_retries(Arc::clone(&h.pause));
        let mut codex_off = on();
        codex_off.off.push(Program::Codex);
        let host = HostHandle::start(codex_off, false, false, h);
        host.relaunch_restored(vec![restored("s-a", "in tab 1")]);
        until("the relaunch between its steps, its retry put off", || {
            let queue = host.shared.restored.lock().unwrap();
            queue
                .attempts
                .front()
                .is_some_and(|attempt| !attempt.acting && attempt.tried >= 1)
        });
        if named {
            a.set(&[("s-a", Program::Codex)]);
        }
        let passes = a.passes.load(Ordering::SeqCst);
        until("two passes", || {
            ring();
            a.passes.load(Ordering::SeqCst) >= passes + 2
        });
        let seen = host
            .shared
            .restored
            .lock()
            .unwrap()
            .attempts
            .front()
            .map(|attempt| attempt.agent_seen);
        host.shutdown_and_join();
        assert_eq!(seen, Some(named), "named: {named}");
    }
}

#[test]
fn an_agent_ended_while_switched_off_is_forgotten() {
    const SID: &str = "s-ended-switched-off";
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert(SID.to_string());
    a.set(&[(SID, Program::Claude)]);
    let host = HostHandle::start(on(), false, false, a.hooks());
    a.attached(&host, SID);
    until("its snapshot published", || {
        published_snapshot(SID).is_some()
    });

    let mut claude_off = on();
    claude_off.off.push(Program::Claude);
    host.set_config(claude_off);
    until("its worker revoked", || host.live().is_empty());
    let kept = Arc::clone(&host.shared.kept.lock().unwrap()[SID]);
    a.passes();
    assert!(published_snapshot(SID).is_some(), "published while it runs");
    assert!(
        kept.lock().unwrap().snapshot.is_some(),
        "kept while it runs"
    );
    *a.holds.lock().unwrap() = Some(true);
    a.set(&[]);
    a.passes();
    assert!(
        published_snapshot(SID).is_some(),
        "a misread forgets nothing"
    );

    *a.holds.lock().unwrap() = Some(false);
    a.passes();
    assert!(published_snapshot(SID).is_none(), "unpublished");
    assert!(kept.lock().unwrap().snapshot.is_none(), "forgotten");

    host.set_config(on());
    a.passes();
    assert!(host.live().is_empty(), "no agent, no worker");
    assert!(published_snapshot(SID).is_none(), "still unpublished");
    assert!(
        a.relaunched.lock().unwrap().is_empty(),
        "not relaunched: {:?}",
        a.relaunched.lock().unwrap()
    );
    host.shutdown_and_join();
}

#[test]
fn the_hosts_admission_is_the_policys_agent_by_agent() {
    let agents: Vec<Program> = Program::ALL
        .into_iter()
        .filter(|p| p.harness_table())
        .collect();
    let mut every_off = on();
    every_off.off = agents.clone();
    let mut master_off = on();
    master_off.enabled = false;
    let alone = agents.iter().map(|agent| {
        let mut cfg = on();
        cfg.off.push(*agent);
        cfg
    });
    let cfgs: Vec<_> = [on(), every_off, master_off]
        .into_iter()
        .chain(alone)
        .collect();
    for cfg in cfgs {
        let admission = Admission::of(&cfg);
        for agent in Program::ALL {
            assert_eq!(
                admission.admits(agent),
                cfg.on_for(agent),
                "{agent:?} under off={:?} enabled={}",
                cfg.off,
                cfg.enabled
            );
        }
    }
    assert_eq!(
        Admission::default(),
        Admission::of(&{
            let mut off = on();
            off.enabled = false;
            off
        })
    );
}

#[test]
fn an_agent_that_left_for_a_switched_off_one_is_forgotten() {
    const SID: &str = "s-left-for-codex";
    let host = HostHandle::for_supervised_test(Vec::new(), Arc::new(|_| BodyEnd::Stopped));
    host.shutdown_and_join();
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert(SID.to_string());
    let hooks = a.hooks();
    let snapshot = snap(SID);
    publish_snapshot_for_test(SID, Some(&snapshot));
    let kept = Arc::new(Mutex::new(Kept {
        snapshot: Some(snapshot),
        ..Kept::default()
    }));
    host.shared
        .kept
        .lock()
        .unwrap()
        .insert(SID.to_string(), Arc::clone(&kept));
    let named = |agent: Program| HashMap::from([(SID.to_string(), agent)]);
    let forget = |named_off: &HashMap<String, Program>, off: Program| {
        forget_left_while_switched_off(
            &host.shared,
            &Workers::default(),
            &HashMap::new(),
            named_off,
            |agent| agent != off,
            &hooks,
        );
    };
    let standing = || kept.lock().unwrap().snapshot.is_some();

    forget(&named(Program::Claude), Program::Claude);
    assert!(standing(), "its own agent runs");
    *a.holds.lock().unwrap() = Some(false);
    forget(&HashMap::new(), Program::Codex);
    assert!(standing(), "Claude Code is admitted");
    kept.lock().unwrap().unwatched = Some(Unwatched {
        agent: Program::Claude,
        group: 4242,
    });
    forget(&named(Program::Codex), Program::Codex);
    assert!(standing(), "its exit is read first");
    kept.lock().unwrap().unwatched = None;
    let watch = |read: bool, hands: bool| ExitWatched {
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".to_string(),
        arming: 1,
        hands: hands.then_some(Unwatched {
            agent: Program::Claude,
            group: 4242,
        }),
        read: read.then_some(ExitRecord::Unread),
    };
    kept.lock().unwrap().exit_watch = Some(watch(false, false));
    forget(&named(Program::Codex), Program::Codex);
    assert!(standing(), "its process may live");
    kept.lock().unwrap().exit_watch = Some(watch(true, true));
    forget(&named(Program::Codex), Program::Codex);
    assert!(standing(), "its exit is handed first");
    kept.lock().unwrap().exit_watch = Some(watch(true, false));

    forget(&named(Program::Codex), Program::Codex);
    assert!(!standing(), "forgotten");
    assert!(published_snapshot(SID).is_none(), "unpublished");
}

#[test]
fn a_same_process_handoff_watch_armed_during_forget_is_kept() {
    forget_after_switch_edit(true);
}

#[test]
fn an_agent_enabled_during_forget_keeps_its_published_snapshot() {
    forget_after_switch_edit(false);
}

fn forget_after_switch_edit(arm_watch: bool) {
    for edit_during_probe in [false, true] {
        let sid = format!("s-forget-late-{arm_watch}-{edit_during_probe}");
        let world = Arc::new(World::default());
        let hooks = hooks(&world, Arc::new(|_| BodyEnd::Stopped));
        let mut off = on();
        off.off.push(Program::Claude);
        let host = HostHandle::start(off, false, true, hooks.clone());
        let snapshot = snap(&sid);
        let kept = Arc::new(Mutex::new(Kept {
            snapshot: Some(snapshot.clone()),
            ..Kept::default()
        }));
        host.shared
            .kept
            .lock()
            .unwrap()
            .insert(sid.clone(), Arc::clone(&kept));
        publish_snapshot_for_test(&sid, Some(&snapshot));
        let mut probing = hooks;
        let writer = host.clone();
        let handed = snapshot.clone();
        let watched = Arc::clone(&kept);
        probing.acts.holds = Arc::new(move |sid, _| {
            if edit_during_probe {
                writer.set_config(on());
                if arm_watch {
                    writer.watch_handed(vec![(sid.to_string(), handed.clone())]);
                    assert!(
                        watched
                            .lock()
                            .unwrap()
                            .exit_watch
                            .as_ref()
                            .is_some_and(|watch| watch.names(&handed)),
                        "the real handoff writer armed the same process"
                    );
                }
            }
            Some(false)
        });
        forget_left_while_switched_off(
            &host.shared,
            &Workers::default(),
            &HashMap::new(),
            &HashMap::new(),
            |agent| host.admits(agent),
            &probing,
        );
        let (snapshot_kept, watch_kept) = {
            let kept = kept.lock().unwrap();
            (
                kept.snapshot.is_some(),
                kept.exit_watch
                    .as_ref()
                    .is_some_and(|watch| watch.names(&snapshot)),
            )
        };
        let published = published_snapshot(&sid).is_some();
        host.shutdown_and_join();
        publish_snapshot_for_test(&sid, None);
        assert_eq!(
            (snapshot_kept, watch_kept, published),
            (
                edit_during_probe,
                edit_during_probe && arm_watch,
                edit_during_probe,
            ),
            "edit={edit_during_probe} handoff_watch={arm_watch}; the control with no edit must forget"
        );
    }
}
