// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::PathBuf;
use std::sync::Arc;

use aterm_agent::harness::upgrade::Dialect;
use aterm_session::SessionId;
#[cfg(unix)]
use aterm_session::sink::SinkWriter;

#[cfg(unix)]
use super::Spawned;
use super::WorkerStart;
use crate::task_launch::{TaskOrder, TaskSeed, task_text};
use crate::task_record::TaskState;
use crate::{App, WindowId};

fn worker() -> WorkerStart {
    WorkerStart {
        order: TaskOrder {
            task: b"the task".to_vec(),
            agent: aterm_phase::Program::Claude,
            model: None,
            caller: None,
            manager_node: None,
            trusted_cwd: None,
            fact_line: true,
            autopilot: crate::task_launch::WORKER_AUTOPILOT,
            chrome: false,
            program: None,
        },
        from: crate::task_record::FROM_CTL.to_string(),
        caller: None,
        shell: PathBuf::from("/bin/zsh"),
        dialect: Dialect::Zsh,
    }
}

fn seeded(sid: &str) -> (TaskSeed, PathBuf) {
    let seed = TaskSeed::new(task_text(
        b"the task",
        Some(crate::task_record::FROM_CTL),
        crate::task_launch::WORKER_AUTOPILOT,
        aterm_phase::Program::Claude,
    ));
    let path = seed.write_for(&SessionId::new(sid)).expect("written");
    (seed, path)
}

/// A headless app that would start a worker of any supervised agent: the harness hosted and
/// on, shell integration on, zsh.
fn supervising_app() -> App {
    let mut app = App::headless_for_test();
    app.harness = Some(crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    ));
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = true\nheadless = true\n"),
    );
    app.session_factory.integrate = true;
    app.session_factory.shell_override = Some("/bin/zsh".to_string());
    app
}

/// `spawn agent=grok` IS REFUSED, BEFORE ANY TAB OPENS, WHILE NO READER SEES GROK BUILD'S
/// TURN END: its task could never be marked done and `await task` would never wake, so the
/// reply says why (fail closed) instead of starting a worker nobody can see end. Muse Code,
/// whose turn end aterm reads, passes the same check in the same app (the control). The test
/// does not follow `Program::supervisable`: flipping it for Grok Build lifts this refusal, so
/// the flip turns this red and is made on purpose
/// (`docs/DESIGN-harness-repos-2026-10-24.md` D40).
#[test]
fn a_grok_worker_is_refused_while_its_turn_end_is_unseen() {
    use super::{SpawnAim, WorkerBlock};
    let app = supervising_app();
    assert!(
        app.worker_shell(aterm_phase::Program::Muse).is_ok(),
        "PRECONDITION: this app starts a supervised agent's worker"
    );
    let block = app.worker_shell(aterm_phase::Program::Grok).expect_err(
        "refused: lifting this is a deliberate change that reads Grok Build's turn end and \
         keeps WorkerCli::signed_out on the sign-in gate (D40)",
    );
    assert!(
        matches!(&block, WorkerBlock::Unseen(why) if why.contains("Grok Build")),
        "{block:?}"
    );
    let refusal = block.refusal("agent=grok");
    assert!(refusal.starts_with("spawn agent=grok: "), "{refusal}");
    assert!(refusal.contains("await task"), "{refusal}");
    let order = TaskOrder::of_frame(
        Some(b"the task".to_vec()),
        aterm_phase::Program::Grok,
        None,
        None,
        None,
    )
    .expect("a well-formed order");
    let Err(why) = app.resolve_spawn(SpawnAim::Front, Some(order)) else {
        panic!("a Grok Build worker was planned");
    };
    assert_eq!(why, refusal);
}

/// A PROGRAM WORKER NEEDS THE WINDOW HARNESS AND SHELL INTEGRATION, NOT AN AGENT'S SWITCHES:
/// with `[harness] enabled = false` an agent's worker is refused (the control) and a program's
/// passes the check; with no harness, or no integration, it is refused naming its form; a line
/// no terminal takes is refused at the spawn, before any tab opens.
#[test]
fn a_program_worker_is_checked_for_its_harness_and_shell_alone() {
    use super::{SpawnAim, WorkerBlock};
    let mut app = supervising_app();
    app.harness_policy =
        crate::harness_policy_cell::HarnessPolicyCell::new(crate::app_config::HarnessPolicy::read(
            "[harness]\nenabled = true\nheadless = true\nautopilot = \"off\"\n\
             [harness.generic]\nenabled = false\n",
        ));
    assert!(
        matches!(
            app.worker_shell(aterm_phase::Program::Muse),
            Err(WorkerBlock::Unsupervised(_))
        ),
        "CONTROL: an agent's worker is refused under its harness's switches"
    );
    assert!(
        app.program_shell().is_ok(),
        "a program's reads no agent's switch and no Autopilot"
    );
    // NO HARNESS LOOKS AT ALL, NO DEADLINE JUDGES ITS START: a line that never ran would read
    // `working` for ever, so a program's worker is refused as an agent's is.
    for (policy, headless, switch) in [
        (
            "[harness]\nenabled = false\n",
            false,
            "[harness] enabled = false",
        ),
        (
            "[harness]\nenabled = true\nheadless = false\n",
            true,
            "[harness] headless = false",
        ),
    ] {
        let mut off = supervising_app();
        off.headless = headless;
        off.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read(policy),
        );
        assert_eq!(
            off.program_shell().err(),
            Some(WorkerBlock::Unsupervised(switch)),
            "{policy}"
        );
    }
    let order = |args: Vec<String>| {
        TaskOrder::of_program_frame(
            Some(b"exit 0".to_vec()),
            crate::program_worker::ProgramRun {
                program: "/bin/sh".to_string(),
                args,
            },
            None,
            None,
        )
        .expect("a well-formed order")
    };
    let Err(why) = app.resolve_spawn(SpawnAim::Front, Some(order(vec!["x".repeat(4096)]))) else {
        panic!("a line no terminal takes was planned");
    };
    assert!(
        why.starts_with("spawn program=/bin/sh: its line cannot be written: "),
        "{why}"
    );
    app.session_factory.integrate = false;
    let Err(why) = app.resolve_spawn(SpawnAim::Front, Some(order(Vec::new()))) else {
        panic!("a program worker with no prompt marks was planned");
    };
    assert!(
        why.starts_with("spawn program=/bin/sh: shell integration is off"),
        "{why}"
    );
    app.harness = None;
    assert_eq!(app.program_shell().err(), Some(WorkerBlock::Unhosted));
}

#[test]
fn a_grok_worker_is_refused_as_grok_before_this_macs_harness_is_asked() {
    use super::{SpawnAim, WorkerBlock};
    let unsupervised: [fn(&mut App); 3] = [
        |app| app.harness = None,
        |app| {
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
            );
        },
        |app| {
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read(
                    "[harness]\nenabled = true\nheadless = true\nautopilot = \"off\"\n",
                ),
            );
        },
    ];
    for turn_off in unsupervised {
        let mut app = supervising_app();
        turn_off(&mut app);
        let muse = app.worker_shell(aterm_phase::Program::Muse);
        assert!(
            matches!(
                muse,
                Err(WorkerBlock::Unhosted | WorkerBlock::Unsupervised(_))
            ),
            "PRECONDITION: this Mac supervises no worker: {muse:?}"
        );
        let block = app
            .worker_shell(aterm_phase::Program::Grok)
            .expect_err("refused");
        assert!(matches!(block, WorkerBlock::Unseen(_)), "{block:?}");
        let order = TaskOrder::of_frame(
            Some(b"the task".to_vec()),
            aterm_phase::Program::Grok,
            None,
            None,
            None,
        )
        .expect("a well-formed order");
        let Err(why) = app.resolve_spawn(SpawnAim::Front, Some(order)) else {
            panic!("a Grok Build worker was planned");
        };
        assert!(why.starts_with("spawn agent=grok: "), "{why}");
    }
}

fn sid_of(app: &App, local: u64) -> String {
    app.store
        .read()
        .unwrap()
        .by_local(local)
        .map(|h| h.sid.as_str().to_string())
        .expect("registered")
}

fn ctx_of(app: &App, local: u64) -> Arc<crate::SessionCtx> {
    app.pool.get(local).expect("live session").ctx.clone()
}

/// A WORKER SPAWNED OVER A FLEET DRIVE NAMES ITS MANAGER'S MAC, THE DRIVER'S, where the spawn
/// names a manager; with no drive, or no manager, it names none.
#[test]
fn a_spawn_over_a_fleet_drive_names_the_driving_mac_as_its_managers() {
    let driver = || crate::fabric::Driver {
        node: "n-cccccccccccccccc".to_string(),
        host: "beta".to_string(),
    };
    let managed = || TaskOrder {
        caller: Some(SessionId::new("s-0123456789abcdef0123")),
        ..worker().order
    };
    assert_eq!(
        super::under_drive(managed(), Some(driver())).manager_node,
        Some(driver().node)
    );
    assert_eq!(super::under_drive(managed(), None).manager_node, None);
    assert_eq!(
        super::under_drive(worker().order, Some(driver())).manager_node,
        None,
        "a worker with no manager has no manager's Mac"
    );
    // The spawn verb reads the drive it is served over.
    let _driving = crate::fabric::DrivingAs::enter(driver());
    assert_eq!(
        super::under_drive(managed(), crate::fabric::serving_driver()).manager_node,
        Some(driver().node)
    );
}

/// A WORKER'S RECORD KEEPS ITS MANAGER'S MAC only where the manager is no session here: a
/// fleet spawn whose `from=` names a session of this Mac has its manager here.
#[test]
fn a_started_workers_record_keeps_its_managers_mac_only_for_a_manager_not_here() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::Builder::new()
        .prefix("aterm-worker-manager")
        .tempdir()
        .expect("scratch state home");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(1));
        app.push_stub_tab(wid, crate::stub_session(2));
        let node = "n-cccccccccccccccc".to_string();
        let started = |app: &mut App, local: u64, caller: Option<u64>| {
            let sid = sid_of(app, local);
            let mut ask = worker();
            ask.order.manager_node = Some(node.clone());
            ask.caller = caller;
            let seed = ask.seed();
            let pending = app.ready_worker(&sid, ask, seed).expect("a worker");
            drop(pending);
            ctx_of(app, local)
                .timeline
                .lock()
                .unwrap()
                .task()
                .and_then(|task| task.manager_node.clone())
        };
        assert_eq!(
            started(&mut app, 1, None),
            Some(node.clone()),
            "a manager on the driving Mac"
        );
        assert_eq!(
            started(&mut app, 2, Some(0)),
            None,
            "a manager that is a session here"
        );
    });
}

/// A PROGRAM WORKER'S TAB IS NAMED BY ITS PROGRAM WHILE ITS SHELL TITLES IT: its shell titles
/// the tab by its folder at a prompt and by the line its launcher typed, which quotes the
/// program's path and words (and, for a store binary, opens with its shim's environment
/// words), so every message placing it (`in Workers ▸ …`) and "Autopilot set to land" named it
/// `Workers ▸ '/opt/lab/bench' '--quick' < "$ATERM_TAS`, its folder once its program ended,
/// and a worker whose line never ran had the row `the program`. Each names it by its
/// `program=`'s file name now, a busy terminal's as the strip last read it, never by its words,
/// its environment words or its folder. CONTROLS: a title its program gives the tab names it
/// while it stands; an agent's worker is named by its agent, and a program worker kept as a tab
/// and a person's tab running the same line by that line, as their chips read.
#[test]
fn a_program_workers_tab_is_named_by_its_program_while_its_shell_titles_it() {
    use crate::app_workers_tests::{land_worker, line_starts, prompt_in};
    use crate::program_worker::ProgramRun;
    use crate::task_record::{TaskMark, mark_session_task};
    use aterm_agent::supervise::Autopilot;
    let mut app = App::headless_for_test();
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    let started = |app: &mut App, local: u64, ask: WorkerStart| {
        let sid = sid_of(app, local);
        let seed = ask.seed();
        drop(app.ready_worker(&sid, ask, seed).expect("a worker"));
    };
    let program = |path: &str, args: &[&str]| {
        let run = ProgramRun {
            program: path.to_string(),
            args: args.iter().map(|word| (*word).to_string()).collect(),
        };
        let line = aterm_agent::harness::worker::program_line(
            Dialect::Zsh,
            std::path::Path::new(&run.program),
            &run.args,
        )
        .expect("a line");
        let mut ask = worker();
        ask.order = TaskOrder::of_program_frame(Some(b"exit 3".to_vec()), run, None, None)
            .expect("a well-formed order");
        (ask, line)
    };
    let typed = |app: &App, local: u64, bytes: &[u8]| {
        let term = app.pool.get(local).expect("pooled").term.clone();
        crate::term_lock(&term).process(bytes);
    };
    let record = |app: &App, key: &str| {
        app.messages
            .log()
            .records()
            .find(|r| r.key.as_deref() == Some(key))
            .map(|r| (r.title.clone(), r.detail.first().cloned()))
            .expect("its record")
    };

    let bench = land_worker(&mut app);
    let (ask, line) = program("/opt/lab/bench", &["--quick"]);
    started(&mut app, bench, ask);
    typed(&app, bench, &prompt_in("~/proj"));
    typed(&app, bench, &line_starts(&line));
    assert_eq!(
        app.tab_label_of_session(bench),
        "bench",
        "a program worker's tab is named by its program while its shell titles it by its line"
    );
    let sid = sid_of(&app, bench);
    assert_eq!(app.session_place(&sid), "in Workers \u{25b8} bench");
    assert!(
        crate::session_timeline::apply_autopilot(
            &ctx_of(&app, bench),
            Some("land".to_owned()),
            None,
            aterm_slog::Actor::Aterm,
            None,
        )
        .moved
    );
    app.on_meta_changed(bench);
    assert_eq!(
        record(&app, &format!("harness.autopilot.{sid}")),
        (
            "Autopilot set to land".to_owned(),
            Some(format!(
                "Workers \u{25b8} bench: {}",
                crate::autopilot::mode_words(Autopilot::Land).record
            )),
        ),
        "the Autopilot record names it by its program"
    );
    app.tab_titles(WindowId(0));
    {
        let busy = app.pool.get(bench).expect("pooled").term.clone();
        let _reading = crate::term_lock(&busy);
        assert_eq!(
            app.tab_label_of_session(bench),
            "bench",
            "a busy terminal's program worker is named as the strip last read its title"
        );
    }
    // Its program exits 3: the shell reports the status before it titles the next prompt.
    typed(&app, bench, b"\x1b]133;D;3\x07");
    let failed = TaskMark {
        why: Some(crate::task_record::WHY_EXIT_STATUS),
        exit: Some(3),
        ..TaskMark::new(TaskState::Failed)
    };
    mark_session_task(&ctx_of(&app, bench), bench, &failed).expect("unfenced");
    assert_eq!(
        app.tab_label_of_session(bench),
        "bench",
        "its program ended, its line still titling the tab"
    );
    typed(&app, bench, &prompt_in("~/proj"));
    assert_eq!(
        app.tab_label_of_session(bench),
        "bench",
        "at its next prompt"
    );

    // A store binary's line opens with its shim's environment words.
    let store = land_worker(&mut app);
    let (ask, _) = program("/Users//me/.atpkg/store/bench/3/bin/bench", &[]);
    started(&mut app, store, ask);
    typed(&app, store, &prompt_in("~/proj"));
    typed(
        &app,
        store,
        &line_starts(
            " ATPKG_HOME=/Users//me/.atpkg '/Users//me/.atpkg/store/bench/3/bin/bench' < \
             \"$ATERM_TASK_FILE\"",
        ),
    );
    assert_eq!(
        app.tab_label_of_session(store),
        "bench",
        "neither its environment words nor its folder name it"
    );
    typed(&app, store, b"\x1b]0;bench: 3 of 10\x07");
    assert_eq!(
        app.tab_label_of_session(store),
        "bench: 3 of 10",
        "CONTROL: a title its program gives the tab names it"
    );

    let never = land_worker(&mut app);
    let (ask, _) = program("/opt/lab/bench", &["--quick"]);
    started(&mut app, never, ask);
    typed(&app, never, &prompt_in("~/proj"));
    let stalled = TaskMark {
        why: Some("startup"),
        ..TaskMark::new(TaskState::Stalled)
    };
    mark_session_task(&ctx_of(&app, never), never, &stalled).expect("unfenced");
    let row = app
        .worker_rows(WindowId(0))
        .into_iter()
        .find(|row| row.session == never)
        .expect("its row");
    assert_eq!(row.title, "bench", "a program worker whose line never ran");

    let claude = land_worker(&mut app);
    started(&mut app, claude, worker());
    typed(&app, claude, &prompt_in("~/proj"));
    typed(
        &app,
        claude,
        &line_starts(" CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 'claude' \"$(cat task.md)\""),
    );
    assert_eq!(
        app.tab_label_of_session(claude),
        "Claude Code",
        "CONTROL: an agent's worker is named by its agent"
    );

    let kept = land_worker(&mut app);
    let (ask, line) = program("/opt/lab/bench", &[]);
    started(&mut app, kept, ask);
    typed(&app, kept, &prompt_in("~/proj"));
    typed(&app, kept, &line_starts(&line));
    assert!(app.keep_as_tab(kept), "PRECONDITION: kept as a tab");
    assert_eq!(
        app.tab_label_of_session(kept),
        line.trim(),
        "CONTROL: a program worker kept as a tab is named as its chip reads"
    );
    let person = crate::app_workers_tests::person_tab(&mut app);
    typed(&app, person, &prompt_in("~/proj"));
    typed(&app, person, &line_starts(&line));
    assert_eq!(
        app.tab_label_of_session(person),
        line.trim(),
        "CONTROL: a person's tab running the same line is named by it"
    );
    host.shutdown_and_join();
}

#[cfg(unix)]
#[test]
fn google_worker_preparation_enters_the_apps_scope_before_any_launcher() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::TempDir::new().unwrap();
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let app = App::headless_for_test();
        let sid = sid_of(&app, 0);
        let ctx = ctx_of(&app, 0);
        let before = ctx.timeline.lock().unwrap().task().cloned();
        let drained = app.account_work.pause().unwrap().try_wait().unwrap();
        let mut ask = worker();
        ask.order.agent = aterm_phase::Program::Agy;
        assert!(
            app.start_worker(&sid, ask)
                .is_err_and(|why| why.contains("paused"))
        );
        assert_eq!(ctx.timeline.lock().unwrap().task().cloned(), before);
        assert!(ctx.timeline.lock().unwrap().worker_launch().is_none());
        drained.resume().unwrap();

        let mut ask = worker();
        ask.order.agent = aterm_phase::Program::Agy;
        let launch = app.start_worker(&sid, ask).unwrap();
        assert!(launch.account_scope.is_some());
        crate::feed_session_for_test(&app, 0, b"\x1b]133;A\x07$ \x1b]133;B\x07");
        let prompt = crate::task_launch::await_prompt(&launch, std::time::Duration::ZERO).unwrap();
        let pause = app.account_work.pause().unwrap();
        assert!(launch.account_scope.as_ref().unwrap().cancelled());
        let pause = pause.try_wait().unwrap_err();
        let result = crate::task_launch::type_at_prompt(&launch, prompt, |prompt| {
            Ok::<_, std::convert::Infallible>(crate::task_launch::type_scoped_launch_line(
                &app.store,
                &launch.ctx.self_id,
                launch.ctx.nonce,
                prompt,
                "never typed",
                launch.account_scope.as_ref(),
            ))
        });
        assert!(result.is_none());
        let record = ctx.timeline.lock().unwrap().task().cloned().unwrap();
        assert_eq!(
            (record.state, record.why),
            (TaskState::Stalled, Some(crate::task_record::WHY_CANCELLED))
        );
        assert!(
            !record.attention,
            "shutdown cancellation is not provider failure"
        );
        // Dropping an unstarted preparation retires its launch captures before its scope.
        drop(launch);
        let drained = pause.try_wait().unwrap();
        drained.resume().unwrap();
        assert!(app.account_work.enter().is_some());
    });
}

/// A SHUTDOWN WHILE A REDRAWN PROMPT SETTLES BLAMES NO SHELL. The look again after a redraw
/// ends on the same cancellation as the launcher's first look, so it says the same word: a
/// dispatcher reading `why=shell` would hold a working member's shell at fault, and one reading
/// `why=held` would wait for a worker that never started.
#[cfg(unix)]
#[test]
fn a_shutdown_while_a_redrawn_prompt_settles_blames_no_shell() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::TempDir::new().unwrap();
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let app = App::headless_for_test();
        let sid = sid_of(&app, 0);
        let ctx = ctx_of(&app, 0);
        let mut ask = worker();
        ask.order.agent = aterm_phase::Program::Agy;
        let launch = app.start_worker(&sid, ask).unwrap();
        crate::feed_session_for_test(&app, 0, b"\x1b]133;A\x07$ \x1b]133;B\x07");
        let prompt = crate::task_launch::await_prompt(&launch, std::time::Duration::ZERO).unwrap();
        let pause = app.account_work.pause().unwrap();
        assert!(launch.account_scope.as_ref().unwrap().cancelled());
        let pause = pause.try_wait().unwrap_err();
        let typed = crate::task_launch::type_at_prompt(&launch, prompt, |_| {
            Ok::<Result<&'static str, _>, std::convert::Infallible>(Err(
                crate::task_launch::Untyped::Unsettled,
            ))
        });
        assert_eq!(typed, None);
        let record = ctx.timeline.lock().unwrap().task().cloned().unwrap();
        assert_eq!(
            (record.state, record.why),
            (TaskState::Stalled, Some(crate::task_record::WHY_CANCELLED))
        );
        assert!(!record.attention, "a shutdown raises no attention");
        drop(launch);
        pause.try_wait().unwrap().resume().unwrap();
    });
}

/// A SHUTDOWN STOPS A LAUNCH THE SAME AT EVERY STEP: before its launcher ran, and at a pool's
/// wait, whose gave-up word would otherwise be read as the shell's end ([`session_alive`] reads
/// the cancellation as one). Each says `why=cancelled` and raises nothing. CONTROL: at the same
/// wait with nothing cancelled, the pool's refusal is why.
#[cfg(unix)]
#[test]
fn a_launch_a_shutdown_stops_says_so_at_every_step() {
    if !crate::test_alone::alone() {
        return;
    }
    let cancelled = |stop: &dyn Fn(crate::task_launch::Launch)| {
        let state = aterm_tempfile::TempDir::new().unwrap();
        crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
            let app = App::headless_for_test();
            let sid = sid_of(&app, 0);
            let ctx = ctx_of(&app, 0);
            let mut ask = worker();
            ask.order.agent = aterm_phase::Program::Agy;
            let launch = app.start_worker(&sid, ask).unwrap();
            let pause = app.account_work.pause().unwrap();
            assert!(launch.account_scope.as_ref().unwrap().cancelled());
            let pause = pause.try_wait().unwrap_err();
            stop(launch);
            let record = ctx.timeline.lock().unwrap().task().cloned().unwrap();
            assert_eq!(
                (record.state, record.why),
                (TaskState::Stalled, Some(crate::task_record::WHY_CANCELLED))
            );
            assert!(!record.attention, "a shutdown raises no attention");
            pause.try_wait().unwrap().resume().unwrap();
        });
    };
    cancelled(&crate::task_launch::launch_worker);
    cancelled(&|launch| crate::task_launch::stall_gave_up(&launch, "no account of its pool"));

    let state = aterm_tempfile::TempDir::new().unwrap();
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let app = App::headless_for_test();
        let sid = sid_of(&app, 0);
        let ctx = ctx_of(&app, 0);
        let mut ask = worker();
        ask.order.agent = aterm_phase::Program::Agy;
        let launch = app.start_worker(&sid, ask).unwrap();
        crate::task_launch::stall_gave_up(&launch, "no account of its pool");
        let record = ctx.timeline.lock().unwrap().task().cloned().unwrap();
        assert_eq!(
            (record.state, record.why),
            (TaskState::Stalled, Some("startup"))
        );
        assert!(record.attention, "CONTROL: the pool's refusal is why");
    });
}

/// A QUIT DOES NOT WAIT OUT A HELD LAUNCH'S LOOK. The launch is parked on a look far longer than
/// this test may take, so only the quit's ring can end it, and it then says `why=cancelled` and
/// raises nothing. NEGATIVE CONTROL: a pause alone left it parked: a live quit took 50 s, until
/// the look ended, and a stop's 30 s clock could have killed it before its mark.
#[cfg(unix)]
#[test]
fn a_quit_wakes_a_launch_held_on_its_pool_and_it_says_why_cancelled() {
    if !crate::test_alone::alone() {
        return;
    }
    const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);
    let state = aterm_tempfile::TempDir::new().unwrap();
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        let sid = sid_of(&app, 0);
        let ctx = ctx_of(&app, 0);
        let mut ask = worker();
        ask.order.agent = aterm_phase::Program::Agy;
        let launch = app.start_worker(&sid, ask).unwrap();
        let watchers = launch.ctx.watchers.clone();
        let watching = watchers.count();
        let (ended, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut wait = crate::task_launch::pool_wait_for(&launch, "no account can take it");
            let went_on = wait(&crate::task_launch::PoolWait::Hold {
                until: 1_794_170_393,
                why: "no account can take it".to_string(),
                look: std::time::Duration::from_secs(3600),
            });
            drop(wait);
            crate::task_launch::stall_gave_up(&launch, "no account can take it");
            drop(launch);
            let _ = ended.send(went_on);
        });
        let started = std::time::Instant::now();
        while watchers.count() == watching {
            assert!(started.elapsed() < HANG_DETECTOR, "the launch never parked");
            aterm_uds::deadline::poll_yield();
        }
        // Time for it to park rather than read the quit at its first look, which would prove
        // nothing. A launch not yet parked passes as well, so this budgets nothing.
        std::thread::sleep(std::time::Duration::from_millis(250));
        assert!(!app.accounts_drained_before(crate::app_account_work::Continuation::Quit));
        let went_on = heard
            .recv_timeout(HANG_DETECTOR)
            .expect("the quit woke the launch");
        assert!(!went_on, "the quit ended its wait");
        let record = ctx.timeline.lock().unwrap().task().cloned().unwrap();
        assert_eq!(
            (record.state, record.why),
            (TaskState::Stalled, Some(crate::task_record::WHY_CANCELLED))
        );
        assert!(!record.attention, "a shutdown raises no attention");
    });
}

#[test]
fn a_worker_that_cannot_start_is_closed_again_and_leaves_no_task_file() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::Builder::new()
        .prefix("aterm-worker-start")
        .tempdir()
        .expect("scratch state home");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(1));
        app.push_stub_tab(wid, crate::stub_session(2));

        let ghost = "s-0123dead";
        let (seed, path) = seeded(ghost);
        assert_eq!(
            app.ready_worker(ghost, worker(), seed).err().as_deref(),
            Some("the worker did not register")
        );
        assert!(!path.exists(), "the task file of a worker that is not");

        let busy = sid_of(&app, 1);
        let fenced = ctx_of(&app, 1);
        let _ = fenced.fabric.fence_hold(false);
        let (seed, path) = seeded(&busy);
        assert_eq!(
            app.ready_worker(&busy, worker(), seed).err().as_deref(),
            Some("busy update-committing")
        );
        fenced.fabric.lift_hold_fence();
        assert!(app.pool.get(1).is_none(), "the newborn is closed again");
        assert!(!path.exists(), "and its task file removed");

        let ready = sid_of(&app, 2);
        let (seed, path) = seeded(&ready);
        let pending = app.ready_worker(&ready, worker(), seed).expect("a worker");
        assert!(app.pool.get(2).is_some(), "the worker stands");
        assert!(path.exists(), "with its task file");
        let record = ctx_of(&app, 2)
            .timeline
            .lock()
            .unwrap()
            .task()
            .cloned()
            .expect("a record");
        assert_eq!(
            (record.state, record.from.as_str()),
            (TaskState::Working, "ctl")
        );
        drop(pending);
        app.push_stub_tab(wid, crate::stub_session(3));
        app.close_confirm = crate::app_window::CloseConfirm::Programmatic;
        assert!(app.close_session_by_id(2).is_ok(), "it closes");
        assert!(!path.exists(), "a closed worker's task file goes with it");

        let sid = sid_of(&app, 3);
        let requested = worker();
        let seed = requested.seed();
        let task = seed.write_for(&SessionId::new(sid.as_str())).unwrap();
        let pending = app.ready_worker(&sid, requested, seed).unwrap();
        assert!(task.exists(), "the task file precedes the launcher");
        let prompt = {
            let mut terminal = crate::term_lock(&pending.launch.term);
            terminal.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
            crate::task_launch::prompt_fence(&terminal, pending.launch.human).unwrap()
        };
        assert!(app.close_session_by_id(3).is_ok());
        assert!(
            crate::task_launch::type_launch_line(
                &app.store,
                &pending.launch.ctx.self_id,
                pending.launch.ctx.nonce,
                prompt,
                "worker",
            )
            .is_err(),
            "close wins final admission"
        );
        drop(pending);
        assert!(!task.exists(), "the task file does not reappear");
    });
}

#[test]
fn a_spawns_autopilot_word_is_its_workers_word_and_fact_line() {
    if !crate::test_alone::alone() {
        return;
    }
    use aterm_agent::supervise::Autopilot;
    let state = aterm_tempfile::Builder::new()
        .prefix("aterm-worker-word")
        .tempdir()
        .expect("scratch state home");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        for (local, args, word) in [
            (1, "agent=claude autopilot=cruise len=8", Autopilot::Cruise),
            (2, "agent=claude len=8", Autopilot::Land),
            (3, "agent=muse autopilot=cruise len=8", Autopilot::Cruise),
            (4, "agent=agy len=8", Autopilot::Land),
            (5, "agent=muse len=8", Autopilot::Land),
            (6, "agent=codex autopilot=cruise len=8", Autopilot::Cruise),
        ] {
            app.push_stub_tab(wid, crate::stub_session(local));
            let Ok(super::SpawnForm::Plain {
                agent:
                    Some(super::AgentSpec::Worker {
                        agent,
                        model,
                        autopilot,
                        from,
                    }),
                cwd,
                ..
            }) = super::parse_spawn_args(args)
            else {
                panic!("{args}: a worker's spawn");
            };
            let order = super::worker_order(
                Some(b"the task".to_vec()),
                agent,
                model,
                autopilot,
                from,
                cwd.as_deref(),
            )
            .expect("an order");
            let start = WorkerStart { order, ..worker() };
            let sid = sid_of(&app, local);
            let seed = start.seed();
            let path = seed.write_for(&SessionId::new(&sid)).expect("written");
            assert_eq!(
                std::fs::read(&path).expect("the task file"),
                task_text(b"the task", Some(crate::task_record::FROM_CTL), word, agent),
                "{args}: the fact line under {word:?}"
            );
            let pending = app.ready_worker(&sid, start, seed).expect("a worker");
            assert_eq!(pending.launch.agent, agent, "{args}: the line's agent");
            assert_eq!(
                ctx_of(&app, local).meta.lock().unwrap().autopilot_word(),
                Some(word),
                "{args}: the newborn's own word"
            );
            let record_agent = |local| {
                ctx_of(&app, local)
                    .timeline
                    .lock()
                    .unwrap()
                    .task()
                    .and_then(|t| t.agent)
            };
            assert_eq!(
                record_agent(local),
                Some(agent),
                "{args}: the agent its startup is read by"
            );
        }
        app.push_stub_tab(wid, crate::stub_session(7));
        assert_eq!(
            ctx_of(&app, 7)
                .timeline
                .lock()
                .unwrap()
                .task()
                .and_then(|t| t.agent),
            None,
            "CONTROL: a plain session"
        );
    });
}

#[cfg(unix)]
fn pipe() -> [i32; 2] {
    use std::os::fd::IntoRawFd;
    let pipe = aterm_uds::pipe()
        .map(|(r, w)| [r.into_raw_fd(), w.into_raw_fd()])
        .expect("pipe");
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    pipe
}

#[cfg(unix)]
fn drained(fd: i32) -> Vec<u8> {
    let mut bytes = [0u8; 4096];
    let read = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
    usize::try_from(read).map_or_else(|_| Vec::new(), |n| bytes[..n].to_vec())
}

#[cfg(unix)]
fn launch_note(app: &App, local: u64) -> String {
    let ctx = ctx_of(app, local);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let said = ctx
            .timeline
            .lock()
            .unwrap()
            .worker_launch()
            .map(|(_, words)| words.clone());
        if let Some(words) = said {
            return words;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the launcher said nothing within a minute"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A PAUSE BETWEEN A POOL'S TRIES ENDS AS A HOLD DOES: on the same subscription, with the task
/// or the session. A launcher that slept through a stall would go on to try again, and a try can
/// claim a row, or install a program, for a task the lead was told did not start. The pause is
/// far longer than this test's hang detector, so only the stall can have ended it. CONTROL: a
/// pause nothing ends runs out and says go on.
#[test]
fn a_startup_stall_ends_a_launchers_retry_pause() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::Builder::new()
        .prefix("aterm-worker-retry-ends")
        .tempdir()
        .expect("scratch state home");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        app.push_stub_tab(WindowId(0), crate::stub_session(1));
        app.push_stub_tab(WindowId(0), crate::stub_session(2));
        let sid = sid_of(&app, 1);
        let ctx = ctx_of(&app, 1);
        let (seed, _path) = seeded(&sid);
        let launch = app.ready_worker(&sid, worker(), seed).expect("a worker");
        let launch = launch.launch;
        let long = crate::task_launch::PoolWait::Retry(std::time::Duration::from_secs(600));

        let mut wait = crate::task_launch::pool_wait_for(&launch, "held");
        assert!(
            wait(&crate::task_launch::PoolWait::Retry(
                std::time::Duration::from_millis(1)
            )),
            "CONTROL: a pause nothing ended"
        );
        drop(wait);

        let (said, heard) = std::sync::mpsc::channel();
        let pausing = std::thread::spawn(move || {
            let _ = said.send(crate::task_launch::pool_wait_for(&launch, "held")(&long));
            (launch, long)
        });
        let mut stalled = crate::task_record::TaskMark::new(TaskState::Stalled);
        stalled.why = Some("startup");
        crate::task_record::mark_session_task(&ctx, 1, &stalled).expect("unfenced");
        assert_eq!(
            heard.recv_timeout(std::time::Duration::from_secs(60)),
            Ok(false),
            "the stall ended the pause, and no further try follows"
        );
        drop(pausing.join().expect("the launcher's pause"));

        let sid = sid_of(&app, 2);
        let (seed, _path) = seeded(&sid);
        let launch = app
            .ready_worker(&sid, worker(), seed)
            .expect("a worker")
            .launch;
        app.exit_session_logical(2);
        assert!(
            !crate::task_launch::pool_wait_for(&launch, "held")(
                &crate::task_launch::PoolWait::Retry(std::time::Duration::from_secs(600))
            ),
            "its session's end ends the pause"
        );
    });
}

/// A launcher holding a launch for a pool keeps no clock either: the deadline's stall ends the
/// hold, its session's mark goes with it, and nothing is tried again when the row opens, so a
/// dispatcher that placed the worker elsewhere never gets a second run days on.
#[test]
fn a_startup_stall_ends_a_launchers_hold_and_nothing_starts_later() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::Builder::new()
        .prefix("aterm-worker-hold-ends")
        .tempdir()
        .expect("scratch state home");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        app.push_stub_tab(WindowId(0), crate::stub_session(1));
        let sid = sid_of(&app, 1);
        let ctx = ctx_of(&app, 1);
        let (seed, _path) = seeded(&sid);
        let launch = app.ready_worker(&sid, worker(), seed).expect("a worker");
        let launch = launch.launch;
        let tries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tried = tries.clone();

        let until = 4_000_000_000_i64;
        let (said, heard) = std::sync::mpsc::channel();
        let holding = std::thread::spawn(move || {
            let got: Result<(), String> = crate::task_launch::pool_attempts(
                || {
                    tried.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err("codex account pool has no usable account".to_string())
                },
                || Some(until),
                || Some(until - 3 * 86_400),
                crate::task_launch::pool_wait_for(
                    &launch,
                    "no account of its pool can take it now",
                ),
            );
            let _ = said.send(got);
            launch
        });
        let hold = || ctx.timeline.lock().unwrap().worker_pool_hold().cloned();
        let waited = std::time::Instant::now();
        while hold().is_none() {
            assert!(
                waited.elapsed() < std::time::Duration::from_secs(60),
                "the launcher never held"
            );
            aterm_uds::deadline::poll_yield();
        }
        assert_eq!(hold().map(|hold| hold.until), Some(until));

        let mut stalled = crate::task_record::TaskMark::new(TaskState::Stalled);
        stalled.why = Some("startup");
        crate::task_record::mark_session_task(&ctx, 1, &stalled).expect("unfenced");
        let refused = heard
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the stall ended its hold");
        assert_eq!(
            refused,
            Err("codex account pool has no usable account".to_string()),
            "nothing is started for a row that opens later"
        );
        let launch = holding.join().expect("the launcher's hold");
        drop(launch);
        assert_eq!(hold(), None, "the mark went with the hold");
        assert_eq!(
            tries.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "it never tried again"
        );
    });
}

/// A LAUNCHER THAT GIVES UP SAYS WHY: its refusal is `stalled why=startup` at once, with the cause
/// in `attention=`, where a dispatcher can read it. NEGATIVE CONTROL: before, it was `why=shell`,
/// which says a shell ended or a person used it, with the cause only in this Mac's log. A record
/// that moved on already says what became of the task, and raises nothing.
#[test]
fn a_launchers_refusal_names_its_cause_where_a_dispatcher_reads_it() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::Builder::new()
        .prefix("aterm-worker-refused")
        .tempdir()
        .expect("scratch state home");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        app.push_stub_tab(WindowId(0), crate::stub_session(1));
        let ctx = ctx_of(&app, 1);
        let attention = || {
            ctx.meta
                .lock()
                .unwrap()
                .attention_owners
                .get(crate::harness_host::ATTENTION_OWNER)
                .map(str::to_owned)
        };
        let record = || {
            let record = ctx.timeline.lock().unwrap().task().cloned();
            record.map(|record| (record.state, record.why))
        };
        let why = "the default login names another account";
        let codex = aterm_phase::Program::Codex;

        crate::task_record::mark_session_task(
            &ctx,
            1,
            &crate::task_record::TaskMark::new(TaskState::Done),
        )
        .expect("unfenced");
        crate::task_launch::stall_refused_task(&ctx, 1, codex, why);
        assert_eq!(
            attention(),
            None,
            "CONTROL: a record that moved on raises nothing"
        );

        crate::task_record::mark_session_task(
            &ctx,
            1,
            &crate::task_record::TaskMark::new(TaskState::Working),
        )
        .expect("unfenced");
        crate::task_launch::stall_refused_task(&ctx, 1, codex, why);
        assert_eq!(record(), Some((TaskState::Stalled, Some("startup"))));
        let said = attention().expect("the cause");
        assert!(said.contains(why), "{said}");
        assert!(
            said.starts_with(crate::harness_host::STARTUP_HEAD),
            "{said}"
        );
    });
}

/// A REFUSED ADMISSION NAMES THE MEMBER AND THE CAUSE where a dispatcher reads them, under its
/// own `why=`, raised with the mark, so the next placement can keep that program off that
/// member. NEGATIVE CONTROL: before, a sign-out or a failed install marked its `why=` with no
/// attention at all, its cause only in this Mac's log. A record that moved on raises nothing.
#[test]
fn a_refused_admission_names_its_member_and_cause_under_its_own_why() {
    if !crate::test_alone::alone() {
        return;
    }
    use aterm_agent::harness::worker::Refused;
    let state = aterm_tempfile::Builder::new()
        .prefix("aterm-worker-admission")
        .tempdir()
        .expect("scratch state home");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        app.push_stub_tab(WindowId(0), crate::stub_session(1));
        let ctx = ctx_of(&app, 1);
        let attention = || {
            ctx.meta
                .lock()
                .unwrap()
                .attention_owners
                .get(crate::harness_host::ATTENTION_OWNER)
                .map(str::to_owned)
        };
        let mark = |to: TaskState| {
            crate::task_record::mark_session_task(&ctx, 1, &crate::task_record::TaskMark::new(to))
                .expect("unfenced");
        };
        let refuse = |refused: Refused| {
            let taken = crate::task_launch::take_admission(
                &ctx,
                1,
                aterm_phase::Program::Codex,
                Err::<(aterm_agent::harness::worker::Admitted, ()), _>(refused),
                None,
                || Some("m22".to_owned()),
            );
            assert!(taken.is_none(), "a refused worker goes no further");
        };

        mark(TaskState::Done);
        refuse(Refused::SignedOut("signed out".to_owned()));
        assert_eq!(
            attention(),
            None,
            "CONTROL: a record that moved on raises nothing"
        );

        for (refused, why, cause) in [
            (
                Refused::SignedOut(
                    "Grok Build is signed out for this worker (no sign-in at \
                     /Users//someone/.grok/auth.json), so no worker was started"
                        .to_owned(),
                ),
                crate::task_record::WHY_SIGNIN,
                "signed out, a person must sign in",
            ),
            (
                Refused::Program(
                    "atpkg's install of codex failed: vendor head unreachable".to_owned(),
                ),
                crate::task_record::WHY_INSTALL,
                "install failed: vendor head unreachable",
            ),
            (
                Refused::Preparation("Native Google pool location is unavailable".to_owned()),
                "startup",
                "Native Google pool location is unavailable",
            ),
        ] {
            mark(TaskState::Working);
            refuse(refused);
            let record = ctx.timeline.lock().unwrap().task().cloned();
            assert_eq!(
                record.map(|record| (record.state, record.why)),
                Some((TaskState::Stalled, Some(why))),
            );
            let said = attention().expect("the cause");
            assert_eq!(
                said,
                format!(
                    "{}no Codex started on m22: {cause} (its task is marked stalled why={why})",
                    crate::harness_host::STARTUP_HEAD
                )
            );
        }
    });
}

/// A SHELL THAT ENDS DURING A POOL'S HOLD ENDS THAT HOLD, and the pool is then no reason: the
/// record says its shell ended, not that an account could not take it, so no dispatcher waits
/// for a window to open. CONTROL: while it is alive the pool's words are the cause.
#[cfg(unix)]
#[test]
fn a_shell_gone_during_a_pool_hold_is_named_not_the_pool() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::Builder::new()
        .prefix("aterm-worker-shell-gone")
        .tempdir()
        .expect("scratch state home");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        app.push_stub_tab(WindowId(0), crate::stub_session(1));
        app.push_stub_tab(WindowId(0), crate::stub_session(2));
        let why = "no account of its pool can take it now";
        for (local, gone, word) in [(1, false, "startup"), (2, true, "shell")] {
            let sid = sid_of(&app, local);
            let ctx = ctx_of(&app, local);
            let (seed, _path) = seeded(&sid);
            let launch = app
                .ready_worker(&sid, worker(), seed)
                .expect("a worker")
                .launch;
            crate::task_record::mark_session_task(
                &ctx,
                local,
                &crate::task_record::TaskMark::new(TaskState::Working),
            )
            .expect("unfenced");
            if gone {
                app.exit_session_logical(local);
            }
            crate::task_launch::stall_gave_up(&launch, why);
            let record = ctx.timeline.lock().unwrap().task().cloned().unwrap();
            assert_eq!(
                (record.state, record.why),
                (TaskState::Stalled, Some(word)),
                "gone={gone}"
            );
            let said = ctx
                .meta
                .lock()
                .unwrap()
                .attention_owners
                .get(crate::harness_host::ATTENTION_OWNER)
                .map(str::to_owned);
            assert_eq!(
                said.as_deref().is_some_and(|said| said.contains(why)),
                !gone,
                "gone={gone}: {said:?}"
            );
        }
    });
}

/// A LINE THE SESSION WOULD NOT TAKE SAYS WHY TOO: no shell ended and no person came, so the
/// record says `why=startup`, not `why=shell`, with the cause in `attention=`.
#[cfg(unix)]
#[test]
fn a_line_nothing_took_names_why_where_a_dispatcher_reads_it() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::Builder::new()
        .prefix("aterm-worker-blocked")
        .tempdir()
        .expect("scratch state home");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        app.push_stub_tab(WindowId(0), crate::stub_session(1));
        let sid = sid_of(&app, 1);
        let ctx = ctx_of(&app, 1);
        let (seed, _path) = seeded(&sid);
        let launch = app
            .ready_worker(&sid, worker(), seed)
            .expect("a worker")
            .launch;
        crate::task_record::mark_session_task(
            &ctx,
            1,
            &crate::task_record::TaskMark::new(TaskState::Working),
        )
        .expect("unfenced");
        crate::feed_session_for_test(&app, 1, b"\x1b]133;A\x07$ \x1b]133;B\x07");
        let seen = crate::task_launch::await_prompt(&launch, std::time::Duration::ZERO)
            .expect("its first prompt");
        let typed = crate::task_launch::type_at_prompt(&launch, seen, |_| {
            Ok::<Result<&'static str, _>, std::convert::Infallible>(Err(
                crate::task_launch::Untyped::Blocked("its input sink is busy".to_string()),
            ))
        });
        assert_eq!(typed, None);
        let record = ctx.timeline.lock().unwrap().task().cloned().unwrap();
        assert_eq!(
            (record.state, record.why),
            (TaskState::Stalled, Some("startup"))
        );
        let said = ctx
            .meta
            .lock()
            .unwrap()
            .attention_owners
            .get(crate::harness_host::ATTENTION_OWNER)
            .map(str::to_owned)
            .expect("the cause");
        assert!(said.contains("its input sink is busy"), "{said}");
    });
}

/// A launcher waiting for its worker's first prompt keeps no clock, so a mark on the task is
/// what ends the wait, and the stall it then reports leaves that mark standing: a deadline's
/// word and attention, or a lead's, are the record of what became of the task.
#[cfg(unix)]
#[test]
fn a_task_that_moves_on_ends_its_launchers_wait_and_keeps_its_mark() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::Builder::new()
        .prefix("aterm-worker-moved-on")
        .tempdir()
        .expect("scratch state home");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        app.push_stub_tab(WindowId(0), crate::stub_session(1));
        let sid = sid_of(&app, 1);
        let ctx = ctx_of(&app, 1);
        let (seed, _path) = seeded(&sid);
        let launch = app.ready_worker(&sid, worker(), seed).expect("a worker");
        let launch = launch.launch;
        let record = || {
            let record = ctx.timeline.lock().unwrap().task().cloned().unwrap();
            (record.state, record.why)
        };

        let (said, heard) = std::sync::mpsc::channel();
        let waiting = std::thread::spawn(move || {
            let _ = said.send(
                crate::task_launch::await_prompt(&launch, std::time::Duration::ZERO).map(|_| ()),
            );
            launch
        });
        let mut stalled = crate::task_record::TaskMark::new(TaskState::Stalled);
        stalled.why = Some("startup");
        crate::task_record::mark_session_task(&ctx, 1, &stalled).expect("unfenced");
        let why = heard
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the mark ended its wait")
            .expect_err("no prompt was drawn");
        assert!(why.contains("stalled why=startup now"), "{why}");
        let launch = waiting.join().expect("the launcher's wait");

        crate::task_launch::stall_task_shell(&ctx, 1, &why);
        assert_eq!(record(), (TaskState::Stalled, Some("startup")));

        let working = crate::task_record::TaskMark::new(TaskState::Working);
        crate::task_record::mark_session_task(&ctx, 1, &working).expect("unfenced");
        crate::feed_session_for_test(&app, 1, b"\x1b]133;A\x07$ \x1b]133;B\x07");
        assert!(
            crate::task_launch::await_prompt(&launch, std::time::Duration::ZERO).is_ok(),
            "CONTROL: a working task's prompt takes its line"
        );
        crate::task_launch::stall_task_shell(&ctx, 1, "CONTROL");
        assert_eq!(
            record(),
            (TaskState::Stalled, Some("shell")),
            "CONTROL: a working task takes the launcher's stall"
        );
    });
}

/// A PROMPT REDRAWN BEFORE ITS LINE IS NO STALL: a worker's shell repaints its first prompt
/// (the new tab's grid settling, an async prompt) between its launcher's look and the main
/// thread's fenced write, which then writes nothing. The launcher looks again and types once
/// that prompt settles, once; a person's use of the session stays final, before the first
/// admission and during the look again alike.
#[cfg(unix)]
#[test]
fn a_prompt_redrawn_before_its_launch_line_takes_the_line_once_it_settles() {
    if !crate::test_alone::alone() {
        return;
    }
    use crate::task_launch::Untyped;
    #[derive(Clone, Copy, Debug)]
    enum Before {
        Redrawn,
        Person,
        RedrawnThenPerson,
    }
    let state = aterm_tempfile::Builder::new()
        .prefix("aterm-worker-redrawn")
        .tempdir()
        .expect("scratch state home");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        let cases = [
            (1, Before::Redrawn, pipe()),
            (2, Before::Person, pipe()),
            (3, Before::RedrawnThenPerson, pipe()),
        ];
        let mut launches = Vec::new();
        for &(local, before, pipe) in &cases {
            // A write end that takes the immediate sink write, as a PTY master does.
            aterm_pty::set_nonblocking(pipe[1], true).expect("nonblocking write end");
            let sink = Arc::new(SinkWriter::new(pipe[1]));
            sink.note_master_nonblocking(true);
            app.push_stub_tab(WindowId(0), crate::stub_session_with_sink(local, sink));
            crate::feed_session_for_test(&app, local, b"\x1b]133;A\x07$ \x1b]133;B\x07");
            let sid = sid_of(&app, local);
            let (seed, _path) = seeded(&sid);
            let launch = app.ready_worker(&sid, worker(), seed).expect("a worker");
            launches.push((local, before, pipe, launch.launch));
        }
        for (local, before, pipe, launch) in launches {
            let ctx = ctx_of(&app, local);
            let store = app.store.clone();
            let (said, heard) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let seen = crate::task_launch::await_prompt(&launch, std::time::Duration::ZERO)
                    .expect("its first prompt");
                let person = || {
                    launch
                        .ctx
                        .human_input
                        .note(crate::metrics::now_us(), crate::human_input::Gesture::Other);
                };
                let mut admitted = Vec::new();
                let typed = crate::task_launch::type_at_prompt(&launch, seen, |prompt| {
                    let first = admitted.is_empty();
                    if first {
                        match before {
                            Before::Redrawn | Before::RedrawnThenPerson => {
                                crate::term_lock(&launch.term).process(b"\r\x1b[K$ ");
                            }
                            Before::Person => person(),
                        }
                    }
                    let result = crate::task_launch::type_launch_line(
                        &store,
                        &launch.ctx.self_id,
                        launch.ctx.nonce,
                        prompt,
                        "worker",
                    );
                    if first && matches!(before, Before::RedrawnThenPerson) {
                        person();
                    }
                    admitted.push(result.clone());
                    Ok::<_, std::convert::Infallible>(result)
                });
                let _ = said.send((typed, admitted));
            });
            let (typed, admitted) = heard
                .recv_timeout(std::time::Duration::from_secs(60))
                .expect("the launcher ended within a minute");
            let record = ctx.timeline.lock().unwrap().task().cloned().unwrap();
            let written = drained(pipe[0]);
            match before {
                Before::Redrawn => {
                    assert_eq!(
                        admitted,
                        [Err(Untyped::Unsettled), Ok("written whole")],
                        "the redraw refused the first admission, the settled prompt took the line"
                    );
                    assert_eq!(typed, Some("written whole"));
                    assert_eq!(written, b"worker\r", "typed once");
                    assert_eq!((record.state, record.why), (TaskState::Working, None));
                }
                Before::Person | Before::RedrawnThenPerson => {
                    assert!(
                        match before {
                            Before::Person => {
                                matches!(admitted.as_slice(), [Err(Untyped::Refused(_))])
                            }
                            _ => admitted == [Err::<&str, _>(Untyped::Unsettled)],
                        },
                        "{before:?}: admitted once: {admitted:?}"
                    );
                    assert_eq!(typed, None, "{before:?}");
                    assert_eq!(written, b"", "{before:?}: nothing typed");
                    assert_eq!(
                        (record.state, record.why),
                        (TaskState::Stalled, Some("shell")),
                        "{before:?}"
                    );
                }
            }
        }
        drop(app);
        for fd in cases.into_iter().flat_map(|(_, _, pipe)| pipe) {
            unsafe { libc::close(fd) };
        }
    });
}

#[cfg(unix)]
#[test]
fn a_workers_launcher_starts_only_once_its_reply_is_taken() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::Builder::new()
        .prefix("aterm-worker-reply")
        .tempdir()
        .expect("scratch state home");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let (dropped, taken) = (pipe(), pipe());
        for (local, pipe) in [(1, dropped), (2, taken)] {
            app.push_stub_tab(
                wid,
                crate::stub_session_with_sink(local, Arc::new(SinkWriter::new(pipe[1]))),
            );
            crate::feed_session_for_test(&app, local, b"\x1b]133;A\x07$ \x1b]133;B\x07");
            assert!(matches!(
                app.pool
                    .get(local)
                    .unwrap()
                    .term
                    .lock()
                    .unwrap()
                    .all_blocks()
                    .last()
                    .map(|b| b.state),
                Some(aterm_core::terminal::BlockState::EnteringCommand)
            ));
        }

        let sid = sid_of(&app, 1);
        let dropped_ctx = ctx_of(&app, 1);
        let (seed, path) = seeded(&sid);
        let pending = app.ready_worker(&sid, worker(), seed).expect("a worker");
        let (tx, rx) = std::sync::mpsc::channel();
        drop(rx);
        app.answer_spawn(
            Ok(Spawned {
                sid,
                worker: Some(pending),
            }),
            tx.into(),
        );
        assert!(
            app.pool.get(1).is_none(),
            "an unowned worker is closed again"
        );
        assert!(!path.exists(), "and its task file removed");
        assert_eq!(drained(dropped[0]), b"", "nothing was typed into it");
        assert!(
            dropped_ctx
                .timeline
                .lock()
                .unwrap()
                .worker_launch()
                .is_none(),
            "and no launcher started for it"
        );

        let sid = sid_of(&app, 2);
        let (seed, path) = seeded(&sid);
        let pending = app.ready_worker(&sid, worker(), seed).expect("a worker");
        let (tx, rx) = std::sync::mpsc::channel();
        app.answer_spawn(
            Ok(Spawned {
                sid: sid.clone(),
                worker: Some(pending),
            }),
            tx.into(),
        );
        assert_eq!(rx.recv().expect("answered"), Ok(sid));
        let said = launch_note(&app, 2);
        assert_eq!(drained(taken[0]), b"", "nothing typed: {said}");
        assert!(app.pool.get(2).is_some(), "the worker stands");
        assert!(path.exists(), "with its task file");
        drop(app);
        for fd in dropped.into_iter().chain(taken) {
            unsafe { libc::close(fd) };
        }
    });
}
