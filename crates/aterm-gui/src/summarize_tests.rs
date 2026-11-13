// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::App;

#[test]
fn the_briefs_read_the_survey_once_and_end() {
    for (fleet, fabric, survey) in [
        (true, false, "`aterm ctl survey --fleet`"),
        (false, false, "`aterm ctl survey`"),
        (true, true, "`aterm ctl survey --fleet`"),
        (false, true, "`aterm ctl survey`"),
    ] {
        let order = summary_order(fleet, fabric);
        let brief = String::from_utf8(order.task.clone()).expect("UTF-8");
        assert!(brief.contains(survey), "{brief}");
        assert!(!brief.contains("ATERM-TASK-DONE"), "no output marker");
        assert!(!brief.contains("meta set role"), "no role");
        assert_eq!(order.caller, None, "from=ctl");
        assert!(!order.fact_line);
        assert_eq!(
            order.autopilot,
            aterm_agent::supervise::Autopilot::Land,
            "spawned under Land: its end is its landing"
        );
    }
}

/// The read the fabric's sentence names, in backticks: the Overview's own
/// (`overview_fabric::ChildRunner::brief`), the machine form, up to its
/// budget.
fn fabric_read_command() -> String {
    let args = crate::overview_fabric::ChildRunner::brief().args;
    let at = args
        .iter()
        .position(|a| a == "--budget")
        .unwrap_or(args.len());
    format!("`aterm {}`", args[..at].join(" "))
}

/// The marks the sentence must give the worker: which key says a session
/// is this node's own, and which names another's node (the keys the
/// Overview's own parse reads).
fn fabric_read_marks() -> [String; 2] {
    [
        format!("\"{}\":true", crate::overview_fabric::THIS_KEY),
        format!("\"{}\"", crate::overview_fabric::NODE_KEY),
    ]
}

#[test]
fn the_briefs_read_the_fabric_only_where_it_is_on() {
    let task = |fleet: bool, fabric: bool| {
        String::from_utf8(summary_order(fleet, fabric).task).expect("UTF-8")
    };
    let command = fabric_read_command();
    assert!(command.contains(" --json"), "{command}");
    for (fleet, today, survey) in [
        (true, FLEET_BRIEF, FLEET_SURVEY),
        (false, MAC_BRIEF, MAC_SURVEY),
    ] {
        let plain = task(fleet, false);
        assert_eq!(plain, today, "byte for byte without the fabric");
        assert!(!plain.contains("fabric"), "{plain}");
        assert_eq!(today.matches(survey).count(), 1, "{today}");
        let read = task(fleet, true);
        assert!(
            read.contains(&format!("{survey} {FABRIC_READ} Write,")),
            "{read}"
        );
        assert_eq!(read.matches("aterm fabric brief").count(), 1, "{read}");
        assert_eq!(read.matches(&command).count(), 1, "{command}: {read}");
        for mark in fabric_read_marks() {
            assert!(read.contains(&mark), "{mark}: {read}");
        }
        assert_eq!(read.replacen(&format!(" {FABRIC_READ}"), "", 1), plain);
    }
}

#[test]
fn this_macs_brief_reads_the_survey_then_the_local_brief() {
    for fabric in [false, true] {
        let brief = String::from_utf8(summary_order(false, fabric).task).expect("UTF-8");
        let at = |s: &str| brief.find(s).unwrap_or_else(|| panic!("{s}: {brief}"));
        let survey = at("`aterm ctl survey` once");
        let local = at("`aterm ctl survey --brief` once");
        assert!(survey < local, "{brief}");
        assert_eq!(brief.matches("aterm ctl survey").count(), 2, "{brief}");
        if fabric {
            assert!(local < at(&fabric_read_command()), "{brief}");
        }
        let fleet = String::from_utf8(summary_order(true, fabric).task).expect("UTF-8");
        assert!(!fleet.contains("--brief"), "{fleet}");
    }
}

#[test]
fn a_press_names_the_fabric_where_this_aterms_fabric_is_on() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let folder = summary_folder_under(root.path()).expect("made");
    let mut app = App::headless_for_test();
    for on in [false, true] {
        app.agents.overview_fabric.configured = on;
        let (order, booking) = app.summary_spawn_in(Some(&folder)).expect("books");
        assert_eq!(booking.cwd.as_deref(), folder.to_str());
        let task = String::from_utf8(order.task).expect("UTF-8");
        assert_eq!(task.contains(FABRIC_READ), on, "fabric {on}: {task}");
        assert_eq!(
            task.contains(&fabric_read_command()),
            on,
            "fabric {on}: {task}"
        );
        for mark in fabric_read_marks() {
            assert_eq!(task.contains(&mark), on, "{mark}, fabric {on}: {task}");
        }
    }
}

/// A SUMMARY STARTS IN ATERM'S OWN FOLDER ([`summary_folder`],
/// [`summary_booking`]), and the probe of a session's repository reads
/// nothing there (`harness_host::session_repo_work`), so its clean
/// defense lands under Land, a repository there or not.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_summary_starts_in_aterms_own_folder_where_no_repository_is_read() {
    let state = aterm_tempfile::tempdir().expect("state root");
    let folder = summary_folder_under(state.path()).expect("the summary folder");
    assert!(folder.is_dir() && in_summary_folder_under(&folder, state.path()));
    let booking = summary_booking(&folder);
    assert_eq!(booking.cwd.as_deref(), folder.to_str());
    assert!(booking.raise);
    let git = |dir: &std::path::Path, args: &[&str]| {
        let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
            .args([
                "-c",
                "user.name=Andrew Yates",
                "-c",
                "user.email=a@example.invalid",
            ])
            .args([
                "-c",
                "init.defaultBranch=main",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}");
    };
    let dirty = |dir: &std::path::Path| {
        git(dir, &["init", "-q", "."]);
        std::fs::write(dir.join("notes.md"), "x").expect("notes");
    };
    dirty(&folder);
    let home = aterm_tempfile::tempdir().expect("home");
    assert_eq!(
        crate::harness_host::session_repo_work_under(
            &folder,
            Some(home.path()),
            Some(state.path()),
            &aterm_agent::harness::repo_work::Used::default(),
        ),
        aterm_agent::harness::repo_work::RepoRead::Clean
    );
    let elsewhere = aterm_tempfile::tempdir().expect("tmp");
    dirty(elsewhere.path());
    assert!(!in_summary_folder_under(elsewhere.path(), state.path()));
    let found = crate::harness_host::session_repo_work_under(
        elsewhere.path(),
        Some(home.path()),
        Some(state.path()),
        &aterm_agent::harness::repo_work::Used::default(),
    );
    assert!(found.found().is_some_and(|w| w.untracked == 1));
}

#[test]
fn the_summary_folder_is_private_to_its_owner() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let there = root.path().join(SUMMARY_FOLDER);
        std::fs::create_dir(&there).expect("pre-made");
        std::fs::set_permissions(&there, std::fs::Permissions::from_mode(0o755))
            .expect("world-readable");
    }
    let folder = summary_folder_under(root.path()).expect("made");
    assert_eq!(folder, root.path().join(SUMMARY_FOLDER));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&folder)
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "owner-only");
    }
    assert_eq!(summary_folder_under(root.path()), Some(folder));
    let blocked = aterm_tempfile::tempdir().expect("tempdir");
    std::fs::write(blocked.path().join(SUMMARY_FOLDER), b"").expect("a file in its place");
    assert_eq!(
        summary_folder_under(blocked.path()),
        None,
        "CONTROL: no folder"
    );
}

#[test]
fn summarize_with_no_folder_of_its_own_refuses_in_plain_words() {
    let refused = summary_spawn(true, false, None).map(|_| ());
    assert_eq!(refused, Err(NO_SUMMARY_FOLDER.to_string()));
    assert!(!NO_SUMMARY_FOLDER.contains("ATERM-TASK-DONE"));
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let folder = summary_folder_under(root.path()).expect("made");
    let (order, booking) = summary_spawn(false, false, Some(&folder)).expect("CONTROL: books");
    assert_eq!(booking.cwd.as_deref(), folder.to_str(), "started there");
    assert!(booking.raise && booking.split.is_none());
    let canonical = std::fs::canonicalize(&folder).expect("canonical");
    let canonical = canonical.to_str().expect("a UTF-8 folder");
    // Said without the verbatim prefix Windows' canonical form carries.
    let canonical = canonical.strip_prefix(r"\\?\").unwrap_or(canonical);
    assert_eq!(
        order.trusted_cwd.as_deref(),
        Some(canonical),
        "trusted there"
    );
    assert_eq!(order.autopilot, aterm_agent::supervise::Autopilot::Land);
}

#[test]
fn summarize_refuses_with_the_block_before_booking_anything() {
    let mut app = crate::App::headless_for_test();
    let block = app.summarize_start_block().expect("no harness here");
    assert!(matches!(block, StartBlock::Worker(_)), "{block:?}");
    assert_eq!(app.summarize_in_new_tab(), Err(block.hint()));
}

fn workers_app() -> App {
    CLAUDE_ON_PATH_FOR_TEST.set(Some(true));
    let mut app = App::headless_for_test();
    app.harness = Some(crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    ));
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = true\nheadless = true\n"),
    );
    app.session_factory.integrate = true;
    app.session_factory.shell_override = Some("/bin/zsh".to_string());
    app
}

#[test]
fn a_summary_whose_tab_did_not_open_is_said_in_a_live_row() {
    let said = |app: &App| {
        app.messages
            .live_by_key("harness.summarize")
            .map(|live| live.msg.title.clone())
    };
    let mut app = workers_app();
    assert_eq!(app.summarize_in_new_tab(), Ok(()));
    assert!(!app.tab_spawns.summary_pending(), "released");
    assert_eq!(app.summarize_in_new_tab(), Ok(()));
    assert!(said(&app).is_some(), "the failed start is a live row");
    let mut app = workers_app();
    let ctx = app.pool.get(0).expect("the stub").ctx.clone();
    ctx.meta.lock().unwrap().role = Some("operator".to_string());
    assert_eq!(app.summarize_in_new_tab(), Ok(()));
    assert!(said(&app).is_some());
}

/// A SUMMARIZE A CONTROL CLIENT INVOKES (`invoke agents.summarize`) IS BOOKED BY THAT CLIENT,
/// whose session its tab's open names once it lands. CONTROL: the menu's own press is the
/// person's.
#[test]
fn a_summarize_a_control_client_invokes_is_booked_by_that_client() {
    use crate::session_store::ExitActor;
    let manager = ExitActor::Sid("s-0123456789abcdef0123".to_string());
    let mut app = workers_app();
    app.tab_spawns.unsent = true;
    let invoked = app.invoke_attributed(manager.clone(), |app| {
        app.summarize_in_new_tab().map(|()| String::new())
    });
    assert_eq!(invoked, Ok(String::new()));
    let booked = crate::tab_spawn::test_support::booked_act(&app).expect("booked");
    assert_eq!((booked.by, booked.why), (manager, Some("summarize")));
    let mut app = workers_app();
    app.tab_spawns.unsent = true;
    assert_eq!(app.summarize_in_new_tab(), Ok(()));
    let booked = crate::tab_spawn::test_support::booked_act(&app).expect("booked");
    assert_eq!(booked.by, ExitActor::Human, "CONTROL");
}

#[test]
fn summarize_is_grey_for_what_its_spawn_would_refuse() {
    use crate::control::control_media::WorkerBlock;
    type Case = (fn(&mut App), fn(&StartBlock) -> bool);
    let blocked: [Case; 5] = [
        (
            |app| app.harness = None,
            |b| *b == StartBlock::Worker(WorkerBlock::Unhosted),
        ),
        (
            |app| {
                app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                    crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
                );
            },
            |b| *b == StartBlock::Worker(WorkerBlock::Unsupervised("[harness] enabled = false")),
        ),
        (
            |app| app.session_factory.integrate = false,
            |b| *b == StartBlock::Worker(WorkerBlock::NoIntegration),
        ),
        (
            |app| app.session_factory.shell_override = Some("/bin/sh".to_string()),
            |b| matches!(b, StartBlock::Worker(WorkerBlock::Shell(_))),
        ),
        (
            |_| CLAUDE_ON_PATH_FOR_TEST.set(Some(false)),
            |b| *b == StartBlock::NoClaude,
        ),
    ];
    for (turn_off, is_block) in blocked {
        let mut app = workers_app();
        turn_off(&mut app);
        let sessions = app.pool.iter().count();
        let block = app.summarize_start_block().expect("the row is grey");
        assert!(is_block(&block), "{block:?}");
        assert_eq!(app.summarize_in_new_tab(), Err(block.hint()));
        assert_eq!(app.pool.iter().count(), sessions, "no tab was created");
    }
    let mut app = workers_app();
    assert_eq!(app.summarize_start_block(), None, "a worker can start here");
    let sessions = app.pool.iter().count();
    assert_eq!(app.summarize_in_new_tab(), Ok(()), "past every check");
    assert_eq!(app.pool.iter().count(), sessions, "no event loop, no tab");
    assert!(
        !app.tab_spawns.summary_pending(),
        "the reservation is released"
    );
}

/// A FISH THAT SAID IT IS OLDER THAN 3.4 GREYS SUMMARIZE: the first look
/// at a fish starts its `--version` off the main thread and blocks
/// nothing, and once it has answered `3.3.1` the worker's check refuses it
/// (`WorkerBlock::OldFish`) before a worker is created, so the grey row's
/// tip says so instead of a launcher that stalls.
#[cfg(unix)]
#[test]
fn a_fish_known_older_than_3_4_greys_summarize() {
    use crate::control::control_media::WorkerBlock;
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-old-fish")
        .tempdir()
        .expect("scratch dir");
    for (version, old) in [("3.3.1", true), ("3.7.1", false)] {
        let folder = dir.path().join(version);
        std::fs::create_dir(&folder).expect("a folder per fish");
        let fish = folder.join("fish");
        std::fs::write(
            &fish,
            format!("#!/bin/sh\necho 'fish, version {version}'\n"),
        )
        .expect("a stand-in fish");
        std::fs::set_permissions(&fish, std::fs::Permissions::from_mode(0o755))
            .expect("executable");
        aterm_spec::wait::first_exec(&fish, &["--version"]);
        let mut app = workers_app();
        app.session_factory.shell_override = Some(fish.to_string_lossy().into_owned());
        assert!(
            app.worker_shell(aterm_phase::Program::Claude).is_ok(),
            "{version}: not known yet"
        );
        let asked = std::time::Instant::now();
        let answered = loop {
            if let Some(answered) = crate::task_launch::fish_answered(&fish) {
                break answered;
            }
            assert!(
                asked.elapsed() < std::time::Duration::from_secs(60),
                "{version}: the fish was never asked"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(answered, Some(!old), "{version}: the version it named");
        if old {
            let block = StartBlock::Worker(WorkerBlock::OldFish);
            assert_eq!(app.summarize_start_block(), Some(block.clone()));
            assert_eq!(app.summarize_in_new_tab(), Err(block.hint()));
        } else {
            assert!(
                app.worker_shell(aterm_phase::Program::Claude).is_ok(),
                "{version} takes the operand"
            );
        }
    }
}

#[test]
fn a_stale_look_is_looked_at_again_off_the_asking_thread() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Condvar, PoisonError};
    static LOOK: Look = Look::new();
    static LOOKS: AtomicUsize = AtomicUsize::new(0);
    static ANSWER: AtomicBool = AtomicBool::new(false);
    static GATE: (Mutex<bool>, Condvar) = (Mutex::new(true), Condvar::new());
    fn look() -> bool {
        LOOKS.fetch_add(1, Ordering::SeqCst);
        let (open, bell) = &GATE;
        let open = open.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = bell
            .wait_timeout_while(open, Duration::from_secs(60), |open| !*open)
            .unwrap_or_else(PoisonError::into_inner);
        ANSWER.load(Ordering::SeqCst)
    }
    let ttl = Duration::from_secs(2);
    let t0 = Instant::now();
    assert!(
        !LOOK.answer(ttl, "aterm-test-look", look, t0),
        "the first, here"
    );
    assert_eq!(LOOKS.load(Ordering::SeqCst), 1);
    assert!(!LOOK.answer(ttl, "aterm-test-look", look, t0 + Duration::from_secs(1)));
    assert_eq!(LOOKS.load(Ordering::SeqCst), 1, "fresh: no look");
    *GATE.0.lock().unwrap_or_else(PoisonError::into_inner) = false;
    ANSWER.store(true, Ordering::SeqCst);
    let stale = t0 + ttl * 2;
    assert!(
        !LOOK.answer(ttl, "aterm-test-look", look, stale),
        "stale: the last answer, at once"
    );
    let began = Instant::now();
    while LOOKS.load(Ordering::SeqCst) < 2 {
        assert!(began.elapsed() < Duration::from_secs(60), "no look started");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!LOOK.answer(ttl, "aterm-test-look", look, stale));
    assert_eq!(LOOKS.load(Ordering::SeqCst), 2, "one look out at a time");
    *GATE.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
    GATE.1.notify_all();
    let began = Instant::now();
    while !LOOK.answer(ttl, "aterm-test-look", look, t0) {
        assert!(
            began.elapsed() < Duration::from_secs(60),
            "the look never ended"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(LOOKS.load(Ordering::SeqCst), 2);
}
