// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::bind;
use crate::items::defaults::tests::ctx;
use crate::model::{Mode, Outcome};
use crate::port::{Fake, Ran, Stat};
use aterm_converge::{Class, Layer, NoJournal, Scope};
use aterm_types::clt_mark::{Asked, Asker};
use std::path::Path;

const ATERM: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

const CC: &str = "/Library/Developer/CommandLineTools/usr/bin/cc";

/// The person's own run at this Mac's console (alice, 501), with a terminal to ask in, as
/// this run's process ([`asker`]) runs.
fn console() -> Fake {
    let sys = unread_console();
    sys.running(&[]);
    sys
}

fn unread_console() -> Fake {
    let mut sys = Fake::root();
    sys.euid = 501;
    sys.tty = true;
    sys.clt_askers = vec![(PID, BIRTH)];
    sys.stat_of(
        "/dev/console",
        Stat {
            uid: 501,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 0,
        },
    );
    sys
}

/// The item over a probe that finds the tools only once [`CC`] is there, its line before
/// `sudo` said to nobody (a test that reads it says it through [`CommandLineTools::saying`]).
fn item(found: Found) -> CommandLineTools {
    CommandLineTools::new(
        Box::new(move |sys: &dyn System| {
            if sys.read_file(std::path::Path::new(CC)).is_ok() {
                Found::Present(CC.into())
            } else {
                found.clone()
            }
        }),
        PathBuf::from(ATERM),
    )
    .saying(Arc::new(|_: &str| {}))
}

/// The root half's argv on the terminal, in the tab and outside it alike.
fn argv() -> String {
    format!("tty /usr/bin/sudo -k -p 'Mac password: ' {ATERM} machines here apply --plain")
}

fn missing() -> CommandLineTools {
    item(Found::Missing("no developer directory".into()))
}

fn run(sys: &Fake, item: &CommandLineTools, mode: Mode) -> aterm_converge::ItemReport {
    let ctx = ctx(&[]);
    let bound = bind([(item as &dyn Item, place())], sys, &ctx);
    let mut rows =
        aterm_converge::run_scoped(&bound, mode, Scope::EVERY, &mut NoJournal, &mut |_| {})
            .unwrap();
    rows.remove(0)
}

/// The tools' row and [`RootSteps`]' beside it, in one run.
fn with_steps(
    sys: &Fake,
    item: &CommandLineTools,
    mode: Mode,
) -> (aterm_converge::ItemReport, aterm_converge::ItemReport) {
    let ctx = ctx(&[]);
    let steps = item.steps();
    let bound = bind(
        [(item as &dyn Item, place()), (&steps as &dyn Item, place())],
        sys,
        &ctx,
    );
    let mut rows =
        aterm_converge::run_scoped(&bound, mode, Scope::EVERY, &mut NoJournal, &mut |_| {})
            .unwrap();
    let steps = rows.remove(1);
    (rows.remove(0), steps)
}

fn asked(sys: &Fake) -> Vec<String> {
    sys.calls()
        .into_iter()
        .filter(|call| call.starts_with("tty "))
        .collect()
}

/// The tools missing, a person's own run at the console plans the root half `--plain`, and
/// its apply runs it on the terminal: one password prompt, and no baseline file written for
/// root to read (the root half applies its own).
#[test]
fn missing_tools_plan_the_machines_here_argv_and_apply_runs_it_on_the_terminal() {
    let sys = console();
    let item = missing();

    let row = run(&sys, &item, Mode::Plan);
    assert_eq!(row.outcome, Outcome::Planned, "{row:?}");
    assert_eq!(row.layer, Some(Layer::Base));
    assert_eq!(row.class, Some(Class::Elevate));
    assert!(
        asked(&sys).is_empty(),
        "a plan runs nothing: {:?}",
        sys.calls()
    );

    let argv = argv();
    sys.on(&argv, Ran::ok(""))
        .on_write(&argv, std::path::Path::new(CC), b"");
    let row = run(&sys, &item, Mode::Apply);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
    let calls = sys.calls();
    assert_eq!(asked(&sys), [argv], "one password prompt: {calls:?}");
    assert!(
        !calls
            .iter()
            .any(|call| call.starts_with("write ") || call.starts_with("scratch")),
        "nothing written for the root half to read: {calls:?}"
    );
    let parsed = crate::baseline::parse(&CommandLineTools::baseline()).unwrap();
    assert_eq!(
        parsed.developer.and_then(|table| table.command_line_tools),
        Some(true)
    );
}

#[test]
fn an_installation_plan_is_narrowed_when_tools_change_before_apply() {
    for (later, owed, suffix, failed) in [
        (
            Found::Missing("still missing".into()),
            false,
            Some(""),
            false,
        ),
        (Found::Present("Xcode works".into()), false, None, false),
        (
            Found::Present("Xcode works".into()),
            true,
            Some(" --root-steps-only"),
            false,
        ),
        (Found::Refuses("compiler refuses".into()), false, None, true),
        (Found::Unknown("compiler unread".into()), false, None, true),
        (Found::NotHere("not applicable".into()), false, None, false),
        (
            Found::Refuses("compiler refuses".into()),
            true,
            Some(" --root-steps-only"),
            false,
        ),
        (
            Found::Unknown("compiler unread".into()),
            true,
            Some(" --root-steps-only"),
            false,
        ),
    ] {
        let state = Arc::new(Mutex::new(Found::Missing("missing at plan".into())));
        let reader = state.clone();
        let item = CommandLineTools::new(
            Box::new(move |_| reader.lock().unwrap().clone()),
            ATERM.into(),
        )
        .with_owed(Box::new(move |_| {
            if owed {
                vec![crate::root_steps::Owed::Debugger]
            } else {
                Vec::new()
            }
        }))
        .saying(Arc::new(|_| {}));
        let sys = console();
        assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
        *state.lock().unwrap() = later;
        let expected: Vec<String> = suffix
            .map(|suffix| format!("{}{suffix}", argv()))
            .into_iter()
            .collect();
        for command in &expected {
            sys.on(command, Ran::ok(""));
        }
        let result = item.apply(&sys, &ctx(&[]));
        assert_eq!(result.is_err(), failed, "{result:?}");
        assert_eq!(asked(&sys), expected);
    }
    // Losing tools after an independent plan cannot admit installation.
    let state = Arc::new(Mutex::new(Found::Present("Xcode works".into())));
    let reader = state.clone();
    let item = CommandLineTools::new(
        Box::new(move |_| reader.lock().unwrap().clone()),
        ATERM.into(),
    )
    .with_owed(Box::new(|_| vec![crate::root_steps::Owed::Debugger]))
    .saying(Arc::new(|_| {}));
    let sys = console();
    assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
    *state.lock().unwrap() = Found::Missing("lost before apply".into());
    let root = format!("{} --root-steps-only", argv());
    sys.on(&root, Ran::ok(""));
    assert!(item.apply(&sys, &ctx(&[])).is_ok());
    assert_eq!(asked(&sys), [root]);

    // Another setup question can own stdin after apply has captured its plan. Change
    // the world only after that capture, while holding the real shared terminal lease.
    for tabbed in [false, true] {
        for (later, owed, suffix, failed) in [
            (
                Found::Present("Xcode arrived while queued".into()),
                true,
                Some(" --root-steps-only"),
                false,
            ),
            (Found::Present("already done".into()), false, None, false),
            (
                Found::Missing("still missing".into()),
                false,
                Some(""),
                false,
            ),
            (
                Found::Refuses("compiler now refuses".into()),
                true,
                Some(" --root-steps-only"),
                false,
            ),
            (
                Found::Refuses("compiler now refuses".into()),
                false,
                None,
                true,
            ),
            (
                Found::Unknown("compiler now unread".into()),
                true,
                Some(" --root-steps-only"),
                false,
            ),
            (
                Found::Unknown("compiler now unread".into()),
                false,
                None,
                true,
            ),
            (
                Found::NotHere("no longer applicable".into()),
                false,
                None,
                false,
            ),
        ] {
            let state = Arc::new(Mutex::new(Found::Missing("initially missing".into())));
            let reader = state.clone();
            let (captured, ready) = std::sync::mpsc::sync_channel(1);
            let reads = std::sync::atomic::AtomicUsize::new(0);
            let said = Arc::new(Mutex::new(String::new()));
            let sink = said.clone();
            let mut item = CommandLineTools::new(
                Box::new(move |_| {
                    let found = reader.lock().unwrap().clone();
                    if reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                        captured.send(()).unwrap();
                    }
                    found
                }),
                ATERM.into(),
            )
            .with_owed(Box::new(move |_| {
                if owed {
                    vec![crate::root_steps::Owed::Debugger]
                } else {
                    Vec::new()
                }
            }))
            .unbidden(Unbidden::new(Some(MARK.into()), Some(asker()), || {
                crate::init::Holds::NONE
            }))
            .saying(Arc::new(move |text| sink.lock().unwrap().push_str(text)));
            let tab = Tab::new({
                let said = said.clone();
                Arc::new(move |text| said.lock().unwrap().push_str(text))
            });
            if tabbed {
                item = item.in_tab(tab.clone());
            }
            let sys = console();
            sys.on(&argv(), Ran::ok(""));
            sys.on(&format!("{} --root-steps-only", argv()), Ran::ok(""));
            assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
            let result = std::thread::scope(|scope| {
                let worker = crate::port::owning_terminal(|| {
                    let worker = scope.spawn(|| item.apply(&sys, &ctx(&[])));
                    ready
                        .recv_timeout(std::time::Duration::from_secs(60))
                        .expect("apply captured its pre-terminal plan");
                    assert!(asked(&sys).is_empty());
                    assert!(said.lock().unwrap().is_empty());
                    assert!(!marked(&sys));
                    *state.lock().unwrap() = later;
                    worker
                });
                worker.join().expect("the queued apply joins")
            });
            let expected: Vec<_> = suffix
                .map(|suffix| format!("{}{suffix}", argv()))
                .into_iter()
                .collect();
            assert_eq!(asked(&sys), expected, "tabbed={tabbed}, {result:?}");
            assert_eq!(result.is_err(), failed, "tabbed={tabbed}, {result:?}");
            assert_eq!(marked(&sys), !tabbed && suffix.is_some());
            assert_eq!(
                tab.entered.load(std::sync::atomic::Ordering::Relaxed),
                tabbed && suffix.is_some()
            );
            if suffix.is_none() {
                assert!(said.lock().unwrap().is_empty() || failed && tabbed);
            }
        }
    }

    // Admission can change while another question owns the real terminal lease too.
    // Capture the old plan before changing console ownership or installer custody.
    for tabbed in [false, true] {
        for (console_uid, installer, tools_arrive, code, withdraw_root) in [
            (Some(0), None, false, 7, false),
            (Some(502), None, false, 7, false),
            (None, Some("/usr/sbin/softwareupdate"), false, 8, false),
            (
                None,
                Some(
                    "/System/Library/CoreServices/Install Command Line Developer Tools.app/Contents/MacOS/Install Command Line Developer Tools",
                ),
                false,
                8,
                false,
            ),
            // A narrow plan owes only independent steps, which a tools installer cannot hold.
            (None, Some("/usr/sbin/softwareupdate"), true, 0, false),
            // A constrained download can enter for definite independent work, but
            // loses that exception if the work completes while the terminal is held.
            (None, None, false, 3, true),
        ] {
            let state = Arc::new(Mutex::new(Found::Missing("initially missing".into())));
            let reader = state.clone();
            let (captured, ready) = std::sync::mpsc::sync_channel(1);
            let reads = std::sync::atomic::AtomicUsize::new(0);
            let said = Arc::new(Mutex::new(String::new()));
            let sink = said.clone();
            let owed = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let owe = owed.clone();
            let mut item = CommandLineTools::new(
                Box::new(move |_| {
                    let found = reader.lock().unwrap().clone();
                    if reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                        captured.send(()).unwrap();
                    }
                    found
                }),
                ATERM.into(),
            )
            .with_owed(Box::new(move |_| {
                if owe.load(std::sync::atomic::Ordering::SeqCst) {
                    vec![crate::root_steps::Owed::Debugger]
                } else {
                    Vec::new()
                }
            }))
            .unbidden(Unbidden::new(Some(MARK.into()), Some(asker()), || {
                crate::init::Holds::NONE
            }))
            .saying(Arc::new(move |text| sink.lock().unwrap().push_str(text)));
            let tab = Tab::new({
                let said = said.clone();
                Arc::new(move |text| said.lock().unwrap().push_str(text))
            });
            if tabbed {
                item = item.in_tab(tab.clone());
            }
            let mut sys = console();
            if withdraw_root {
                sys.low_data = Some(true);
            }
            sys.on(&argv(), Ran::ok(""));
            let root = format!("{} --root-steps-only", argv());
            sys.on(&root, Ran::ok(""));
            assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
            let result = std::thread::scope(|scope| {
                let worker = crate::port::owning_terminal(|| {
                    let worker = scope.spawn(|| item.apply(&sys, &ctx(&[])));
                    ready
                        .recv_timeout(std::time::Duration::from_secs(60))
                        .expect("apply captured its plan before the terminal lease");
                    assert!(asked(&sys).is_empty());
                    assert!(!marked(&sys));
                    assert!(said.lock().unwrap().is_empty());
                    if let Some(uid) = console_uid {
                        sys.stat_of(
                            "/dev/console",
                            Stat {
                                uid,
                                gid: 20,
                                mode: 0o600,
                                dir: false,
                                symlink: false,
                                len: 0,
                            },
                        );
                    }
                    if let Some(installer) = installer {
                        sys.file(crate::items::developer::PLACEHOLDER, b"")
                            .running(&["/sbin/launchd", installer]);
                    }
                    if tools_arrive {
                        *state.lock().unwrap() = Found::Present("tools arrived".into());
                    }
                    if withdraw_root {
                        owed.store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                    worker
                });
                worker.join().expect("the queued apply joins")
            });
            if code == 0 {
                assert!(result.is_ok(), "{result:?}");
                assert_eq!(asked(&sys), [root]);
            } else {
                assert!(result.is_err(), "{tabbed}: admission changed: {result:?}");
                assert!(asked(&sys).is_empty(), "{tabbed}: {:?}", asked(&sys));
                assert!(!marked(&sys));
                assert!(!tab.entered.load(std::sync::atomic::Ordering::Relaxed));
                assert!(said.lock().unwrap().is_empty());
                if tabbed {
                    assert_eq!(tab.exit(tab.unfinished().unwrap()), code);
                }
            }
        }
    }
}

#[test]
fn independent_work_completed_before_apply_spends_no_password_episode() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for found in [
        Found::Present("healthy".into()),
        Found::Refuses("broken".into()),
        Found::Unknown("unread".into()),
    ] {
        let owed = Arc::new(AtomicBool::new(true));
        let reader = owed.clone();
        let said = Arc::new(Mutex::new(String::new()));
        let sink = said.clone();
        let item = item(found)
            .with_owed(Box::new(move |_| {
                if reader.load(Ordering::Relaxed) {
                    vec![crate::root_steps::Owed::Debugger]
                } else {
                    Vec::new()
                }
            }))
            .unbidden(Unbidden::new(Some(MARK.into()), Some(asker()), || {
                crate::init::Holds::NONE
            }))
            .saying(Arc::new(move |text| sink.lock().unwrap().push_str(text)));
        let sys = console();
        assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
        owed.store(false, Ordering::Relaxed);
        assert!(item.apply(&sys, &ctx(&[])).is_ok());
        assert!(asked(&sys).is_empty());
        assert!(said.lock().unwrap().is_empty());
        assert!(!marked(&sys));
        // Skipping obsolete work leaves the one episode available for genuinely owed work.
        owed.store(true, Ordering::Relaxed);
        assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
        let root = format!("{} --root-steps-only", argv());
        sys.on(&root, Ran::ok(""));
        assert!(item.apply(&sys, &ctx(&[])).is_ok());
        assert_eq!(asked(&sys), [root]);
        assert!(marked(&sys));
    }

    for tabbed in [false, true] {
        for still_owed in [false, true] {
            let owed = Arc::new(AtomicBool::new(true));
            let owing = owed.clone();
            let found = Arc::new(Mutex::new(Found::Present("healthy at plan".into())));
            let reader = found.clone();
            let (captured, ready) = std::sync::mpsc::sync_channel(1);
            let reads = std::sync::atomic::AtomicUsize::new(0);
            let said = Arc::new(Mutex::new(String::new()));
            let sink = said.clone();
            let mut item = CommandLineTools::new(
                Box::new(move |_| reader.lock().unwrap().clone()),
                ATERM.into(),
            )
            .with_owed(Box::new(move |_| {
                let pending = owing.load(Ordering::SeqCst);
                if reads.fetch_add(1, Ordering::SeqCst) == 1 {
                    captured.send(()).unwrap();
                }
                if pending {
                    vec![crate::root_steps::Owed::Debugger]
                } else {
                    Vec::new()
                }
            }))
            .unbidden(Unbidden::new(Some(MARK.into()), Some(asker()), || {
                crate::init::Holds::NONE
            }))
            .saying(Arc::new(move |text| sink.lock().unwrap().push_str(text)));
            let tab = Tab::new({
                let said = said.clone();
                Arc::new(move |text| said.lock().unwrap().push_str(text))
            });
            if tabbed {
                item = item.in_tab(tab.clone());
            }
            let sys = console();
            let root = format!("{} --root-steps-only", argv());
            sys.on(&root, Ran::ok(""));
            assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
            let result = std::thread::scope(|scope| {
                let worker = crate::port::owning_terminal(|| {
                    let worker = scope.spawn(|| item.apply(&sys, &ctx(&[])));
                    ready
                        .recv_timeout(std::time::Duration::from_secs(60))
                        .expect("apply captured its independent work");
                    assert!(asked(&sys).is_empty());
                    assert!(said.lock().unwrap().is_empty());
                    assert!(!marked(&sys));
                    owed.store(still_owed, Ordering::SeqCst);
                    if still_owed {
                        *found.lock().unwrap() = Found::Missing("lost while queued".into());
                        // A concurrent probe can plan a new full installation. It cannot
                        // expand the independent command already waiting on this lease.
                        assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
                        assert_eq!(item.root_plan(), RootPlan::WithTools);
                    }
                    worker
                });
                worker.join().expect("the queued independent apply joins")
            });
            assert!(result.is_ok(), "tabbed={tabbed}, {result:?}");
            let expected: Vec<_> = still_owed.then_some(root).into_iter().collect();
            assert_eq!(asked(&sys), expected);
            assert_eq!(marked(&sys), !tabbed && still_owed);
            assert_eq!(tab.entered.load(Ordering::Relaxed), tabbed && still_owed);
            assert_eq!(said.lock().unwrap().is_empty(), !still_owed);
        }
    }
}

#[test]
fn completed_root_work_needs_no_terminal() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for tabbed in [false, true] {
        let pending = Arc::new(AtomicBool::new(true));
        let reader = pending.clone();
        let mut item = item(Found::Present("healthy".into())).with_owed(Box::new(move |_| {
            if reader.load(Ordering::Relaxed) {
                vec![crate::root_steps::Owed::Debugger]
            } else {
                Vec::new()
            }
        }));
        let tab = Tab::new(Arc::new(|_| {}));
        if tabbed {
            item = item.in_tab(tab.clone());
        }
        let mut sys = console();
        assert!(matches!(item.read(&sys).0, Probe::Change { .. }));
        pending.store(false, Ordering::Relaxed);
        sys.tty = false;
        assert!(item.apply(&sys, &ctx(&[])).is_ok());
        assert!(asked(&sys).is_empty());
        assert_eq!(tab.unfinished(), None);
        pending.store(true, Ordering::Relaxed);
        assert!(item.apply(&sys, &ctx(&[])).is_err());
        assert!(asked(&sys).is_empty());
    }
}

#[test]
fn independent_root_work_preserves_a_refusing_or_unread_compiler() {
    use crate::root_steps::Owed;
    for found in [
        Found::Refuses("broken compiler".into()),
        Found::Unknown("unread compiler".into()),
    ] {
        let mut away = console();
        away.tty = false;
        let held = item(found.clone()).with_owed(Box::new(|_| vec![Owed::Debugger]));
        assert!(!matches!(held.read(&away).0, Probe::Change { .. }));
        let _ = run(&away, &held, Mode::Apply);
        assert!(asked(&away).is_empty());
        let settled = item(found.clone());
        assert!(!matches!(settled.read(&console()).0, Probe::Change { .. }));
        let sys = console();
        let item = item(found.clone()).with_owed(Box::new(|_| {
            vec![Owed::Debugger, Owed::NameRule, Owed::LanBroker]
        }));
        let root = format!("{} --root-steps-only", argv());
        let marker = Path::new("/independent-root-step");
        sys.on(&root, Ran::ok("")).on_write(&root, marker, b"done");
        assert_eq!(run(&sys, &item, Mode::Plan).outcome, Outcome::Planned);
        // A successful independent root command is not a compiler repair.
        let _ = run(&sys, &item, Mode::Apply);
        assert_eq!(asked(&sys), std::slice::from_ref(&root));
        assert_eq!(sys.read_file(marker).unwrap(), b"done");
        assert!(sys.read_file(Path::new(CC)).is_err());
        let expected = match found {
            Found::Refuses(_) => matches!(item.read(&sys).0, Probe::Blocked { .. }),
            Found::Unknown(_) => matches!(item.read(&sys).0, Probe::Unusable { .. }),
            _ => unreachable!(),
        };
        assert!(expected);
        let _ = run(&sys, &item, Mode::Apply);
        assert_eq!(
            asked(&sys),
            [root],
            "independent work consumes only one root episode"
        );
    }
    let parsed =
        crate::baseline::parse(&CommandLineTools::baseline_named(RootPlan::StepsOnly, None))
            .unwrap();
    let developer = parsed.developer.unwrap();
    assert_eq!(developer.command_line_tools, Some(false));
    assert_eq!(developer.debugger, Some(true));
    assert_eq!(parsed.names.unwrap().admitted_name_rule, Some(true));
    assert_eq!(parsed.lan.unwrap().broker, Some(true));
}

/// On a Mac that has the tools, the elevation still runs where its other root steps are
/// owed: a person's run at the console asks for the one password, saying the setup's own
/// line, and its read-back reads the tools in place whatever the root half left owed; the
/// root half's baseline turns on developer mode and puts the naming rule in place. A refused
/// password there is no success. NEGATIVE CONTROLS: with nothing owed nothing is asked, and in
/// a run outside the tab that may not ask (no terminal) the tools' row reads in place while
/// [`RootSteps`]' row is deferred, naming what is owed and why, and nothing is asked.
#[test]
fn a_mac_with_the_tools_still_gets_the_one_password_for_what_is_owed() {
    use crate::root_steps::Owed;
    let present = || item(Found::Present("installed".into()));
    let owed_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let owing = |item: CommandLineTools| {
        let owed_reads = Arc::clone(&owed_reads);
        item.with_owed(Box::new(move |_: &dyn System| {
            owed_reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            vec![Owed::Debugger, Owed::NameRule]
        }))
    };
    let said = Arc::new(Mutex::new(String::new()));
    let sink = said.clone();
    let item = owing(present()).saying(Arc::new(move |text: &str| {
        sink.lock().unwrap().push_str(text);
    }));
    let sys = console();
    assert_eq!(run(&sys, &item, Mode::Plan).outcome, Outcome::Planned);
    let root = format!("{} --root-steps-only", argv());
    sys.on(&root, Ran::ok(""));
    let reads_at_start = Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
    sys.tty_model({
        let owed_reads = Arc::clone(&owed_reads);
        let reads_at_start = Arc::clone(&reads_at_start);
        move |_, _| {
            reads_at_start.store(
                owed_reads.load(std::sync::atomic::Ordering::Relaxed),
                std::sync::atomic::Ordering::Relaxed,
            );
            None
        }
    });
    let row = run(&sys, &item, Mode::Apply);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
    assert_eq!(
        owed_reads.load(std::sync::atomic::Ordering::Relaxed),
        reads_at_start.load(std::sync::atomic::Ordering::Relaxed),
        "read-back does not re-probe root work after its command starts"
    );
    assert_eq!(
        asked(&sys),
        std::slice::from_ref(&root),
        "one password prompt: {:?}",
        sys.calls()
    );
    assert_eq!(*said.lock().unwrap(), format!("{INTRO_SETUP}\n"));
    let parsed = crate::baseline::parse(&CommandLineTools::baseline()).unwrap();
    assert_eq!(parsed.developer.and_then(|t| t.debugger), Some(true));
    assert_eq!(parsed.names.and_then(|t| t.admitted_name_rule), Some(true));
    assert_eq!(parsed.lan.and_then(|t| t.broker), Some(true));

    let refused = console();
    refused.on(&root, Ran::exit(1, "Sorry, try again."));
    let row = run(&refused, &owing(present()), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Failed, "{row:?}");

    let settled = console();
    let row = run(&settled, &present(), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
    assert!(asked(&settled).is_empty(), "{:?}", settled.calls());

    // H17: where this run may not ask (no terminal), the tools' row says the tools alone and
    // the root steps' row says what is owed and why, deferred: never in place.
    let mut away = console();
    away.tty = false;
    let (row, steps) = with_steps(&away, &owing(present()), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
    assert_eq!(steps.outcome, Outcome::Deferred, "{steps:?}");
    assert!(deferred(&steps), "{steps:?}");
    assert!(
        steps
            .why
            .as_deref()
            .is_some_and(|why| why.contains("still owed: ") && why.contains(NO_TTY)),
        "{steps:?}"
    );
    assert!(asked(&away).is_empty(), "{:?}", away.calls());

    // A person's retry repairs the actual compiled permission, even when an old
    // safe file made the automatic, metadata-only root inventory read current.
    // The LAN image and plist are current, so no unrelated root work is owed.
    let ready = |rule: &str| {
        use aterm_uds::lan::{self, install};
        let mut sys = console();
        sys.logged_in(&crate::items::defaults::tests::user("alice", 501));
        sys.on(
            "/usr/sbin/DevToolsSecurity -status",
            Ran::ok("Developer mode is currently enabled.\n"),
        );
        let source = Path::new("/Applications/aterm.app");
        sys.running_app = Some(source.to_owned());
        let image = install::test_image(0x0100_000c, &[b"same current code"]);
        sys.file("/Applications/aterm.app/Contents/MacOS/aterm", &image)
            .file(lan::IMAGE_EXECUTABLE, &image)
            .file(lan::PLIST_PATH, install::plist_text(source).as_bytes());
        assert!(crate::items::lan::reading(&sys).unwrap().unwrap().current());
        let wanted = crate::root_steps::name_rule("alice").unwrap();
        if rule != "missing" {
            if rule != "opaque" {
                sys.file(
                    crate::root_steps::ADMITTED_NAME_RULE,
                    if rule == "current" {
                        wanted.as_bytes()
                    } else {
                        b"old bare-mN rule"
                    },
                );
            }
            sys.stat_of(
                crate::root_steps::ADMITTED_NAME_RULE,
                Stat {
                    uid: 0,
                    gid: 0,
                    mode: 0o440,
                    dir: false,
                    symlink: false,
                    len: wanted.len() as u64,
                },
            );
        }
        sys
    };
    let composed = |decider: crate::init::Decider| {
        decider
            .compose(
                crate::init::Base {
                    clt: Some(present().with_owed(Box::new(crate::root_steps::owed))),
                    ..crate::init::Base::none()
                },
                || {
                    Unbidden::new(Some(PathBuf::from(MARK)), Some(asker()), || {
                        crate::init::Holds::NONE
                    })
                },
            )
            .clt
            .unwrap()
    };
    let permission = format!("{} --name-rule-only", argv());
    for rule in ["old", "opaque", "missing"] {
        let sys = ready(rule);
        // The fake Mac's owed work, read by `owed` on any host: `owed_here` reads none off macOS.
        let ordinary = crate::root_steps::owed(&sys);
        assert_eq!(
            ordinary,
            if rule == "missing" {
                vec![Owed::NameRule]
            } else {
                Vec::new()
            }
        );
        assert!(!matches!(
            crate::root_steps::name_rule_current(&sys),
            Ok(true)
        ));
        aterm_types::clt_mark::explicit(sys.clt_mark(), Path::new(MARK), &asker(), sys.now)
            .unwrap();
        let item = composed(crate::init::Decider::Person);
        let planned = run(&sys, &item, Mode::Plan);
        assert_eq!(
            planned.outcome,
            Outcome::Planned,
            "explicit permission repair: {rule}"
        );
        assert!(
            planned
                .to
                .as_deref()
                .is_some_and(|to| to.contains("--name-rule-only")),
            "the plan names the command it runs: {planned:?}"
        );
        assert_eq!(item.root_plan(), RootPlan::NameRuleOnly);
        sys.on(&permission, Ran::ok(""));
        assert_eq!(run(&sys, &item, Mode::Apply).outcome, Outcome::Changed);
        assert_eq!(asked(&sys), std::slice::from_ref(&permission));
        assert!(
            marked(&sys),
            "an explicit repair consumes the same durable episode"
        );
        let row = run(&sys, &composed(crate::init::Decider::Aterm), Mode::Apply);
        assert!(matches!(row.outcome, Outcome::Ok | Outcome::Deferred));
        assert_eq!(
            asked(&sys),
            std::slice::from_ref(&permission),
            "a later automatic pass never repeats the password"
        );
    }
    let current = ready("current");
    assert_eq!(crate::root_steps::name_rule_current(&current), Ok(true));
    assert_eq!(
        run(
            &current,
            &composed(crate::init::Decider::Person),
            Mode::Apply
        )
        .outcome,
        Outcome::Ok
    );
    assert!(asked(&current).is_empty());

    let completed = ready("old");
    let item = composed(crate::init::Decider::Person);
    assert_eq!(run(&completed, &item, Mode::Plan).outcome, Outcome::Planned);
    completed.file(
        crate::root_steps::ADMITTED_NAME_RULE,
        crate::root_steps::name_rule("alice").unwrap().as_bytes(),
    );
    assert!(item.apply(&completed, &ctx(&[])).is_ok());
    assert!(
        asked(&completed).is_empty(),
        "fresh currency retires a queued repair"
    );

    for stopped in [false, true] {
        let mut sys = ready("old");
        if stopped {
            sys.stop_now();
        } else {
            sys.admin = Some(false);
        }
        let _ = run(&sys, &composed(crate::init::Decider::Person), Mode::Apply);
        assert!(
            asked(&sys).is_empty(),
            "permission debt bypassed live admission"
        );
    }
}

/// H10: the root steps never wait on the compiler's verdict. Where the tools are there but
/// refuse to build (an unaccepted license) or the compiler did not answer, a run that may ask
/// still plans and runs the one password for the owed steps; the read-back then says the
/// tools' own verdict, and what is still owed after the password reads deferred, never in
/// place, in [`RootSteps`]' row.
/// NEGATIVE CONTROL: with no terminal, the same reading asks nothing and the tools' row keeps
/// its verdict.
#[test]
fn owed_root_steps_are_asked_for_beside_tools_that_refuse_or_do_not_answer() {
    use crate::root_steps::Owed;
    let owing =
        |found: Found| item(found).with_owed(Box::new(|_: &dyn System| vec![Owed::LanBroker]));
    let root = format!("{} --root-steps-only", argv());
    for (found, verdict) in [
        (Found::Refuses("license".into()), Outcome::Blocked),
        (Found::Unknown("cc did not answer".into()), Outcome::Unknown),
    ] {
        let sys = console();
        sys.on(&root, Ran::exit(0, ""));
        let (row, steps) = with_steps(&sys, &owing(found.clone()), Mode::Plan);
        assert_eq!(row.outcome, Outcome::Planned, "{found:?}: {row:?}");
        assert_eq!(steps.outcome, Outcome::Deferred, "{steps:?}");
        let (row, steps) = with_steps(&sys, &owing(found.clone()), Mode::Apply);
        assert_eq!(
            asked(&sys),
            std::slice::from_ref(&root),
            "{found:?}: {:?}",
            sys.calls()
        );
        // The fake's root half leaves the step owed: the tools' verdict, and the step left.
        assert_ne!(row.outcome, Outcome::Ok, "{found:?}: {row:?}");
        assert_eq!(steps.outcome, Outcome::Deferred, "{steps:?}");
        assert!(
            steps
                .why
                .as_deref()
                .is_some_and(|why| why.starts_with("still owed after this run's Mac password")),
            "{steps:?}"
        );

        let mut away = console();
        away.tty = false;
        let (row, steps) = with_steps(&away, &owing(found.clone()), Mode::Apply);
        assert_eq!(row.outcome, verdict, "{found:?}: {row:?}");
        assert_eq!(steps.outcome, Outcome::Deferred, "{steps:?}");
        assert!(asked(&away).is_empty(), "{:?}", away.calls());
    }
}

/// H17: [`RootSteps`]' row reads in place only where nothing is owed, and is a step left
/// (deferred) otherwise, so `aterm init --check` never says setup is done while the one
/// password's steps are owed; it changes nothing itself.
#[test]
fn the_root_steps_row_is_in_place_only_where_nothing_is_owed() {
    use crate::root_steps::Owed;
    let mut away = console();
    away.tty = false;
    let present = item(Found::Present(CC.into()));
    let (_, steps) = with_steps(&away, &present, Mode::Plan);
    assert_eq!(steps.outcome, Outcome::Ok, "{steps:?}");
    let owing =
        item(Found::Present(CC.into())).with_owed(Box::new(|_: &dyn System| vec![Owed::Debugger]));
    for mode in [Mode::Plan, Mode::Apply] {
        let (row, steps) = with_steps(&away, &owing, mode);
        assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
        assert_eq!(steps.outcome, Outcome::Deferred, "{mode:?}: {steps:?}");
        assert!(deferred(&steps), "{steps:?}");
    }
    assert!(owing.steps().apply(&away, &ctx(&[])).is_err());
}

/// A swarm label known when the root half runs rides the same prompt: the one argv gains
/// `--name` and optional typed `--nickname`; the root baseline derives all three slots
/// from those values. No target leaves the argv and baseline unnamed.
#[test]
fn a_known_swarm_label_rides_the_one_password_prompt() {
    use aterm_types::machine_name::{GeneratedNames, Nickname, Number};
    let target = GeneratedNames::number_only(Number::parse("m42").unwrap());
    let sys = console();
    let item = missing().with_name(Box::new(|| {
        Some(GeneratedNames::number_only(Number::parse("m42").unwrap()))
    }));
    let named = format!("{} --name m42", argv());
    sys.on(&named, Ran::ok(""))
        .on_write(&named, std::path::Path::new(CC), b"");
    let row = run(&sys, &item, Mode::Apply);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
    assert_eq!(
        asked(&sys),
        [named],
        "one password prompt: {:?}",
        sys.calls()
    );
    let parsed = crate::baseline::parse(&CommandLineTools::baseline_named(
        RootPlan::WithTools,
        Some(&target),
    ))
    .unwrap();
    let names = parsed.names.expect("a [names] table");
    assert_eq!(names.computer_name.as_deref(), Some("m42"));
    assert_eq!(names.local_host_name.as_deref(), Some("m42"));
    assert_eq!(names.host_name.as_deref(), Some("m42"));
    assert_eq!(names.admitted_name_rule, Some(true));

    let target = GeneratedNames::new(
        Number::parse("m85").unwrap(),
        &Nickname::parse("Dog").unwrap(),
    );
    let sys = console();
    let owned = target.clone();
    let item = missing().with_name(Box::new(move || Some(owned.clone())));
    let named = format!("{} --name m85 --nickname Dog", argv());
    sys.on(&named, Ran::ok(""))
        .on_write(&named, std::path::Path::new(CC), b"");
    let row = run(&sys, &item, Mode::Apply);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
    assert_eq!(asked(&sys), [named]);
    let parsed = crate::baseline::parse(&CommandLineTools::baseline_named(
        RootPlan::WithTools,
        Some(&target),
    ))
    .unwrap();
    let names = parsed.names.unwrap();
    assert_eq!(names.computer_name.as_deref(), Some("m85 Dog"));
    assert_eq!(names.local_host_name.as_deref(), Some("m85-dog"));
    assert_eq!(names.host_name.as_deref(), Some("m85-dog"));
    let sys = console();
    let item = missing().with_name(Box::new(|| None));
    sys.on(&argv(), Ran::ok(""))
        .on_write(&argv(), std::path::Path::new(CC), b"");
    run(&sys, &item, Mode::Apply);
    assert_eq!(asked(&sys), [argv()]);

    let permission = crate::baseline::parse(&CommandLineTools::baseline_named(
        RootPlan::NameRuleOnly,
        Some(&target),
    ))
    .unwrap();
    assert!(permission.developer.is_none() && permission.lan.is_none());
    let names = permission.names.unwrap();
    assert_eq!(names.admitted_name_rule, Some(true));
    assert!(
        names.computer_name.is_none()
            && names.local_host_name.is_none()
            && names.host_name.is_none()
    );
    let item = CommandLineTools::new(
        Box::new(|_| panic!("permission repair reads no developer-tool state")),
        PathBuf::from(ATERM),
    )
    .with_name(Box::new(|| {
        panic!("permission repair reads no saved target")
    }));
    *item.root_plan.lock().unwrap() = RootPlan::NameRuleOnly;
    let fixed = format!("{} --name-rule-only", argv());
    let sys = console();
    sys.on(&fixed, Ran::ok(""));
    let out = Tab::new(Arc::new(|_| {}));
    assert!(item.apply_in_tab(&sys, &out).is_ok());
    assert_eq!(asked(&sys), std::slice::from_ref(&fixed));
    // CLT already present cannot forgive this endpoint's own failed result.
    let failed = console();
    failed.on(&fixed, Ran::exit(87, "permission failed"));
    assert!(item.apply_in_tab(&failed, &out).is_err());
    assert_eq!(asked(&failed), [fixed]);
}

/// A run outside the tab says its one line right before `sudo` asks, whatever ran on the
/// terminal before it, and asks with the plain prompt under `sudo -k` (design §6.3): a
/// password cached for an earlier `sudo` never authorizes the root half, and none is kept
/// once it ends, so no `sudo -K` follows it, whether the install finished or not.
#[test]
fn a_run_outside_the_tab_says_why_right_before_sudo_asks_afresh() {
    let argv = argv();
    for (ran, outcome) in [
        (Ran::ok(""), Outcome::Changed),
        (Ran::exit(82, ""), Outcome::Failed),
    ] {
        let sys = Arc::new(console());
        let said = Arc::new(Mutex::new(Vec::new()));
        let item = missing().saying({
            let said = Arc::clone(&said);
            let sys = Arc::clone(&sys);
            Arc::new(move |text: &str| {
                assert!(
                    crate::port::terminal_free().is_none(),
                    "the prelude owns the terminal"
                );
                said.lock()
                    .unwrap()
                    .push((text.to_owned(), sys.calls().len()));
            })
        });
        let installs = ran.success();
        sys.on(&argv, ran);
        if installs {
            sys.on_write(&argv, std::path::Path::new(CC), b"");
        }
        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, outcome, "{row:?}");
        let calls = sys.calls();
        let ran = calls
            .iter()
            .position(|seen| *seen == argv)
            .unwrap_or_else(|| panic!("no root half: {calls:?}"));
        assert_eq!(
            *said.lock().unwrap(),
            [(format!("{INTRO}\n"), ran)],
            "said once under the same terminal lease immediately before sudo: {calls:?}"
        );
        let deferred = calls.iter().position(|call| call == "defer stops").unwrap();
        let restored = calls
            .iter()
            .position(|call| call == "stops restored")
            .unwrap();
        assert!(deferred < ran && ran < restored, "{calls:?}");
        assert_eq!(asked(&sys), std::slice::from_ref(&argv), "{calls:?}");
        assert!(
            !calls.iter().any(|call| call.contains("sudo -K")),
            "nothing cached to forget: {calls:?}"
        );
    }
    // A vault question may own the terminal first. The root pass must not print its
    // prelude, claim its episode or start sudo while that question still owns stdin.
    for stopped_while_queued in [false, true] {
        let sys = Arc::new(console());
        sys.on(&argv, Ran::ok(""));
        sys.on_write(&argv, std::path::Path::new(CC), b"");
        let (probed, ready) = std::sync::mpsc::sync_channel(1);
        let once = std::sync::atomic::AtomicBool::new(false);
        let said = Arc::new(Mutex::new(String::new()));
        let question_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let item = CommandLineTools::new(
            Box::new(move |sys| {
                if !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    let _ = probed.send(());
                }
                if sys.read_file(std::path::Path::new(CC)).is_ok() {
                    Found::Present(CC.into())
                } else {
                    Found::Missing("no developer directory".into())
                }
            }),
            ATERM.into(),
        )
        .unbidden(Unbidden::new(Some(MARK.into()), Some(asker()), || {
            crate::init::Holds::NONE
        }))
        .saying({
            let (said, question_done) = (Arc::clone(&said), Arc::clone(&question_done));
            Arc::new(move |text| {
                assert!(question_done.load(std::sync::atomic::Ordering::SeqCst));
                assert!(crate::port::terminal_free().is_none());
                said.lock().unwrap().push_str(text);
            })
        });
        let row = std::thread::scope(|scope| {
            let worker = crate::port::owning_terminal(|| {
                let worker = scope.spawn(|| run(&sys, &item, Mode::Apply));
                ready
                    .recv_timeout(std::time::Duration::from_secs(60))
                    .expect("the root item probes");
                assert!(said.lock().unwrap().is_empty());
                assert!(asked(&sys).is_empty());
                assert!(matches!(record(&sys), Asked::Available));
                if stopped_while_queued {
                    sys.stop_now();
                }
                question_done.store(true, std::sync::atomic::Ordering::SeqCst);
                worker
            });
            worker.join().expect("the owned root pass joins")
        });
        if stopped_while_queued {
            assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
            assert!(asked(&sys).is_empty());
            assert!(said.lock().unwrap().is_empty());
            assert!(
                matches!(record(&sys), Asked::Available),
                "no root child or password episode was started"
            );
        } else {
            assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
            assert_eq!(asked(&sys), std::slice::from_ref(&argv));
            assert!(!matches!(record(&sys), Asked::Available));
            assert_eq!(*said.lock().unwrap(), format!("{INTRO}\n"));
        }
    }
}

/// The root half's failure says why in plain words on a person's own run too, never the argv
/// it ran.
#[test]
fn a_person_s_own_run_says_why_in_plain_words_never_the_argv() {
    let sys = console();
    sys.on(&argv(), Ran::exit(82, ""));
    let row = run(&sys, &missing(), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
    assert_eq!(row.why.as_deref(), Some(Unfinished::Download.reason()));
}

/// In Low Data Mode a download alone asks no password; definite independent root work
/// can enter the original batch, which retains the download behind its own network gate.
#[test]
fn low_data_mode_asks_no_password_and_says_how_to_go_on() {
    let mut sys = console();
    sys.low_data = Some(true);
    let item = missing();
    for mode in [Mode::Plan, Mode::Apply] {
        let row = run(&sys, &item, mode);
        assert_eq!(row.outcome, Outcome::Blocked, "{mode:?}: {row:?}");
        let why = row.why.unwrap_or_default();
        assert!(
            why.starts_with("no developer directory; Low Data Mode is on"),
            "{why}"
        );
    }
    assert!(asked(&sys).is_empty(), "{:?}", sys.calls());
    sys.low_data = Some(false);
    assert_eq!(run(&sys, &item, Mode::Plan).outcome, Outcome::Planned);

    // An authorized built-in batch does its independent steps before retaining that
    // same root process while the tools' download waits for an unconstrained path.
    for step in [
        crate::root_steps::Owed::Debugger,
        crate::root_steps::Owed::NameRule,
        crate::root_steps::Owed::LanBroker,
    ] {
        for tabbed in [false, true] {
            let mut sys = console();
            sys.low_data = Some(true);
            let mut item = missing().with_owed(Box::new(move |_| vec![step]));
            let tab = Tab::new(Arc::new(|_| {}));
            if tabbed {
                item = item.in_tab(tab.clone());
            }
            assert_eq!(run(&sys, &item, Mode::Plan).outcome, Outcome::Planned);
            sys.on(&argv(), Ran::ok(""))
                .on_write(&argv(), std::path::Path::new(CC), b"");
            let row = run(&sys, &item, Mode::Apply);
            assert_eq!(row.outcome, Outcome::Changed, "{step:?}: {row:?}");
            assert_eq!(asked(&sys), [argv()], "one full batch: {:?}", sys.calls());
            assert_eq!(
                tab.entered.load(std::sync::atomic::Ordering::Relaxed),
                tabbed
            );
        }
    }

    // Unknown root state is no independent work that may admit the ask.
    let mut unknown = console();
    unknown.low_data = Some(true);
    unknown
        .on(
            "/usr/sbin/DevToolsSecurity -status",
            Ran::ok("Developer mode rights are in an unknown state.\n"),
        )
        .stat_fails(
            crate::root_steps::ADMITTED_NAME_RULE,
            std::io::ErrorKind::PermissionDenied,
        );
    let item = missing().with_owed(Box::new(crate::root_steps::owed));
    assert_eq!(run(&unknown, &item, Mode::Apply).outcome, Outcome::Blocked);
    assert!(asked(&unknown).is_empty());
}

/// An account that is not one of the Mac's administrators asks for no password (macOS's
/// `sudo` would refuse it, which read as a wrong password): the row says only an
/// administrator can install them, and the tab says so as its own class.
#[test]
fn an_account_that_is_no_administrator_asks_no_password() {
    let mut sys = console();
    sys.admin = Some(false);
    let row = run(&sys, &missing(), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
    assert!(
        row.why
            .as_deref()
            .is_some_and(|why| why.ends_with(NOT_ADMIN)),
        "{row:?}"
    );
    assert!(asked(&sys).is_empty(), "{:?}", sys.calls());

    let sys = Arc::new(sys);
    let (item, said, tab) = in_tab(&sys, Found::Missing("no developer directory".into()));
    let _ = run(&sys, &item, Mode::Apply);
    assert_eq!(tab.unfinished(), Some(Unfinished::NotAdmin));
    assert!(
        said.lock().unwrap().is_empty(),
        "nothing said before the end"
    );
    assert!(asked(&sys).is_empty(), "{:?}", sys.calls());

    let mut sys = console();
    sys.admin = Some(true);
    assert_eq!(run(&sys, &missing(), Mode::Plan).outcome, Outcome::Planned);
}

/// An existing repair may temporarily break a compiler that was already installed. Its
/// custody defers both refusing and unanswered probes; losing custody restores the actual
/// failure, and a later ready probe recovers the row without another password attempt.
#[cfg(target_os = "macos")]
#[test]
fn an_active_repair_defers_a_refusing_or_unanswered_compiler_until_it_recovers() {
    let apple = format!(
        "/System/Library/CoreServices/{}",
        aterm_types::macos_setup::APPLE_TOOLS_INSTALLER
    );
    for (found, unheld, unfinished) in [
        (
            Found::Refuses("compiler mid replacement".into()),
            Outcome::Blocked,
            Unfinished::Refuses,
        ),
        (
            Found::Unknown("compiler did not answer during replacement".into()),
            Outcome::Unknown,
            Unfinished::Unread,
        ),
    ] {
        for (owner, held) in [
            ("/usr/sbin/softwareupdate", Unfinished::Busy),
            ("/usr/sbin/installer", Unfinished::Busy),
            (apple.as_str(), Unfinished::AppleWindow),
        ] {
            let sys = Arc::new(console());
            sys.file(crate::items::developer::PLACEHOLDER, b"")
                .running(&["/sbin/launchd", owner]);
            let (item, said, tab) = in_tab(&sys, found.clone());
            let waiting = run(&sys, &item, Mode::Apply);
            assert_eq!(waiting.outcome, Outcome::Deferred, "{owner}: {waiting:?}");
            assert!(deferred(&waiting));
            assert_eq!(tab.unfinished(), Some(held));
            assert!(asked(&sys).is_empty());
            assert!(said.lock().unwrap().is_empty());

            sys.running(&["/sbin/launchd"]);
            assert_eq!(run(&sys, &item, Mode::Apply).outcome, unheld);
            assert_eq!(tab.unfinished(), Some(unfinished));

            sys.file(CC, b"repaired compiler");
            assert_eq!(run(&sys, &item, Mode::Apply).outcome, Outcome::Ok);
            assert!(asked(&sys).is_empty());
            assert!(said.lock().unwrap().is_empty());
        }
        let sys = Arc::new(unread_console());
        let (item, said, tab) = in_tab(&sys, found);
        assert_eq!(run(&sys, &item, Mode::Apply).outcome, unheld);
        assert_eq!(tab.unfinished(), Some(unfinished));
        assert!(asked(&sys).is_empty());
        assert!(said.lock().unwrap().is_empty());
    }
}

/// With an install of the tools under way (the placeholder held by `softwareupdate`, Apple's
/// package `installer`, or a live run of the root half), the item asks no password and fails
/// nothing: it is deferred to that install. A placeholder left behind beside other programs,
/// one merely called `installer` included, is no install.
#[test]
fn an_install_under_way_asks_no_password_and_fails_nothing() {
    for installer in ["/usr/sbin/softwareupdate", "/usr/sbin/installer"] {
        let sys = console();
        sys.file(crate::items::developer::PLACEHOLDER, b"")
            .running(&["/sbin/launchd", installer]);
        let row = run(&sys, &missing(), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Deferred, "{installer}: {row:?}");
        assert!(deferred(&row));
        assert!(
            row.why
                .as_deref()
                .is_some_and(|why| why.ends_with(INSTALLING))
        );
        assert!(asked(&sys).is_empty(), "{:?}", sys.calls());
    }

    let stale = console();
    stale
        .file(crate::items::developer::PLACEHOLDER, b"")
        .running(&["/sbin/launchd", "/tmp/installer"]);
    assert_eq!(
        run(&stale, &missing(), Mode::Plan).outcome,
        Outcome::Planned
    );

    // A run of the root half that is still alive holds it, though no installer process
    // runs this moment (it waits on Apple's installer window): a second `aterm init` defers
    // to it, asking no password.
    let held = console();
    held.file(crate::items::developer::PLACEHOLDER, b"aterm 4242 1000\n")
        .running(&["/sbin/launchd"])
        .born(4242, 1000);
    let row = run(&held, &missing(), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
    assert!(asked(&held).is_empty(), "{:?}", held.calls());
}

/// Apple's installer window is Apple's own flow, at any age, with the placeholder beside it
/// (it made the placeholder as it opened, before any press, so a press hours later leaves no
/// trace aterm reads) or with none (it opened beside another install's, which has ended
/// since): the item asks no password and fails nothing, deferred to that window with the
/// person's step there named (finish in it, or quit it so aterm can install them), and the
/// tab says it as the window's class.
/// NEGATIVE CONTROL: the placeholder with no window beside it (left behind) plans the install.
#[test]
fn apples_window_defers_at_any_age_with_or_without_the_placeholder() {
    let apple = "/System/Library/CoreServices/Install Command Line Developer Tools.app/\
                 Contents/MacOS/Install Command Line Developer Tools";
    let placeholder = crate::items::developer::PLACEHOLDER;
    let day = 24 * 60 * 60;
    for beside in [true, false] {
        for age in [Some(60), Some(3 * 60 * 60), Some(10 * day), None] {
            let sys = Arc::new(console());
            if beside {
                sys.file(placeholder, b"");
            }
            sys.running(&["/sbin/launchd", apple]);
            if let Some(age) = age {
                sys.born(Fake::PIDS + 1, sys.now - age);
            }
            let row = run(&sys, &missing(), Mode::Apply);
            assert_eq!(row.outcome, Outcome::Deferred, "{beside} {age:?}: {row:?}");
            assert!(deferred(&row), "{beside} {age:?}");
            assert!(
                row.why
                    .as_deref()
                    .is_some_and(|why| why.ends_with(APPLE_WINDOW_OPEN)),
                "{beside} {age:?}: {row:?}"
            );
            assert!(
                asked(&sys).is_empty(),
                "{beside} {age:?}: {:?}",
                sys.calls()
            );
            let (item, said, tab) = in_tab(&sys, Found::Missing("no developer directory".into()));
            assert_eq!(
                run(&sys, &item, Mode::Apply).outcome,
                Outcome::Deferred,
                "{beside} {age:?}"
            );
            assert_eq!(
                tab.unfinished(),
                Some(Unfinished::AppleWindow),
                "{beside} {age:?}"
            );
            assert!(said.lock().unwrap().is_empty(), "{beside} {age:?}");
            assert!(
                asked(&sys).is_empty(),
                "{beside} {age:?}: {:?}",
                sys.calls()
            );
        }
    }

    let left = console();
    left.file(placeholder, b"").running(&["/sbin/launchd"]);
    assert_eq!(run(&left, &missing(), Mode::Plan).outcome, Outcome::Planned);
}

/// A process the kernel names no executable for (one whose file a replacement deleted while
/// it ran) is none of Apple's installers: beside a stale placeholder whose stamped run is
/// gone, `aterm init` plans the install, as the root half then takes the placeholder's place.
/// NEGATIVE CONTROL: with `softwareupdate` named beside it, the item defers to that install.
#[test]
fn a_process_the_kernel_names_no_file_for_defers_nothing() {
    let unnamed = crate::port::Process {
        pid: Fake::PIDS + 7,
        ppid: crate::port::Process::LAUNCHD,
        terminal: false,
        exe: None,
        comm: "deleted".into(),
    };
    for (beside, outcome) in [
        ("/sbin/launchd", Outcome::Planned),
        ("/usr/sbin/softwareupdate", Outcome::Deferred),
    ] {
        let sys = console();
        sys.file(crate::items::developer::PLACEHOLDER, b"aterm 4242 1000\n")
            .running(&["/sbin/launchd", beside])
            .process(unnamed.clone(), None);
        assert_eq!(
            run(&sys, &missing(), Mode::Plan).outcome,
            outcome,
            "{beside}"
        );
    }
}

/// Only a person at this Mac's console types the Mac password: an agent's tab (a terminal
/// a coding agent started), a run with no terminal, and a remote shell each defer the tools,
/// asking nothing, so the run fails nothing for them.
#[test]
fn off_the_console_the_tools_wait_for_a_person_and_nothing_is_asked() {
    let mut agent = console();
    agent.agent = Some("Claude Code");
    let mut script = console();
    script.terminal = false;
    script.tty = false;
    let mut remote = console();
    remote.remote_login = true;
    for (name, sys) in [("agent", agent), ("script", script), ("remote", remote)] {
        for mode in [Mode::Plan, Mode::Apply] {
            let row = run(&sys, &missing(), mode);
            assert_eq!(row.outcome, Outcome::Deferred, "{name} {mode:?}: {row:?}");
            assert!(deferred(&row), "{name}");
            assert!(
                row.why
                    .as_deref()
                    .is_some_and(|why| why.contains("a person at this Mac's console")),
                "{name}: {row:?}"
            );
        }
        assert!(asked(&sys).is_empty(), "{name}: {:?}", sys.calls());
    }
}

/// At the console with no terminal to ask in (no `/dev/tty`), pending work stays blocked,
/// running nothing.
#[test]
fn without_a_terminal_pending_work_is_blocked() {
    let mut sys = console();
    sys.tty = false;
    let item = missing();
    for mode in [Mode::Plan, Mode::Apply] {
        let row = run(&sys, &item, mode);
        assert_eq!(row.outcome, Outcome::Blocked, "{mode:?}: {row:?}");
    }
    assert!(asked(&sys).is_empty(), "{:?}", sys.calls());
}

#[test]
fn the_probe_maps_each_finding() {
    let sys = console();
    for (found, outcome) in [
        (Found::Present("cc".into()), Outcome::Ok),
        (Found::Refuses("license".into()), Outcome::Blocked),
        (Found::Unknown("no answer".into()), Outcome::Unknown),
        (Found::NotHere("not macOS".into()), Outcome::NotApplicable),
    ] {
        let row = run(&sys, &item(found.clone()), Mode::Apply);
        assert_eq!(row.outcome, outcome, "{found:?}: {row:?}");
    }
    assert!(asked(&sys).is_empty(), "{:?}", sys.calls());
}

#[test]
fn unread_installer_custody_asks_nothing_and_recovers_on_a_later_read() {
    for placeholder in [false, true] {
        let sys = Arc::new(unread_console());
        if placeholder {
            sys.file(crate::items::developer::PLACEHOLDER, b"");
        }
        let item = missing();
        for mode in [Mode::Plan, Mode::Apply] {
            let row = run(&sys, &item, mode);
            assert_eq!(row.outcome, Outcome::Unknown, "{placeholder}: {row:?}");
            assert!(asked(&sys).is_empty(), "no password with unread custody");
        }
        let (tab_item, said, tab) = in_tab(&sys, Found::Missing("no developer directory".into()));
        assert_eq!(run(&sys, &tab_item, Mode::Apply).outcome, Outcome::Unknown);
        assert_eq!(tab.unfinished(), Some(Unfinished::Unread));
        assert!(said.lock().unwrap().is_empty());
        assert!(asked(&sys).is_empty());

        sys.running(&[]);
        assert_eq!(run(&sys, &item, Mode::Plan).outcome, Outcome::Planned);
        sys.on(&argv(), Ran::ok(""))
            .on_write(&argv(), std::path::Path::new(CC), b"");
        assert_eq!(run(&sys, &item, Mode::Apply).outcome, Outcome::Changed);
        assert_eq!(asked(&sys), [argv()], "one prompt after known idle");
    }
}

#[test]
fn an_unread_placeholder_does_not_admit_an_install_and_can_recover() {
    use std::io::ErrorKind;
    let placeholder = crate::items::developer::PLACEHOLDER;
    for kind in [
        ErrorKind::PermissionDenied,
        ErrorKind::NotADirectory,
        ErrorKind::Other,
    ] {
        for processes in [vec![], vec!["/usr/sbin/softwareupdate"]] {
            let sys = Arc::new(console());
            sys.running(&processes).stat_fails(placeholder, kind);
            let (item, said, tab) = in_tab(&sys, Found::Missing("no developer directory".into()));
            assert_eq!(run(&sys, &item, Mode::Apply).outcome, Outcome::Unknown);
            assert_eq!(tab.unfinished(), Some(Unfinished::Unread));
            assert!(asked(&sys).is_empty());
            assert!(said.lock().unwrap().is_empty());

            sys.running(&[])
                .stat_fails(placeholder, ErrorKind::NotFound);
            sys.on(&argv(), Ran::ok(""))
                .on_write(&argv(), std::path::Path::new(CC), b"");
            assert_eq!(run(&sys, &item, Mode::Apply).outcome, Outcome::Changed);
            assert_eq!(asked(&sys), [argv()]);
        }
    }
}

type TabMessages = Arc<Mutex<Vec<(usize, String)>>>;

/// The tab's item over a probe that finds the tools only once [`CC`] is there, speaking
/// into `said`, each entry with how many calls `sys` had seen when it was said.
fn in_tab(sys: &Arc<Fake>, found: Found) -> (CommandLineTools, TabMessages, Tab) {
    let said = Arc::new(Mutex::new(Vec::new()));
    let tab = Tab::new({
        let (said, sys) = (Arc::clone(&said), Arc::clone(sys));
        Arc::new(move |text: &str| {
            said.lock()
                .unwrap()
                .push((sys.calls().len(), text.to_owned()));
        })
    });
    (item(found).in_tab(tab.clone()), said, tab)
}

/// In the tab, the apply says its one intro line right before `sudo` asks for the Mac
/// password with the plain prompt under `sudo -k`, as a run outside it does, runs the root
/// half `--plain`, and holds off a stop until it ended.
#[test]
fn in_the_tab_the_intro_comes_right_before_sudo_asks_with_the_plain_prompt() {
    let sys = Arc::new(console());
    let (item, said, tab) = in_tab(&sys, Found::Missing("no developer directory".into()));
    let argv = argv();
    sys.on(&argv, Ran::ok(""))
        .on_write(&argv, std::path::Path::new(CC), b"");
    let row = run(&sys, &item, Mode::Apply);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
    let calls = sys.calls();
    let at = |call: &str| {
        calls
            .iter()
            .position(|c| c == call)
            .unwrap_or_else(|| panic!("{call}: {calls:?}"))
    };
    let ran = at(&argv);
    assert_eq!(
        *said.lock().unwrap(),
        [(ran, format!("{TAB_INTRO}\n"))],
        "said once, right before sudo ({ran}): {calls:?}"
    );
    assert!(at("catch stops") < ran, "{calls:?}");
    assert!(
        ran < at("stops restored"),
        "stops are held until the root half ended: {calls:?}"
    );
    assert!(
        !calls.iter().any(|call| call.contains("sudo -K")),
        "nothing cached to forget: {calls:?}"
    );
    assert_eq!(tab.unfinished(), None);
}

/// A run outside the tab, a person's own `aterm init` at the console and a run aterm decided
/// alike, runs the root half in the person's terminal's foreground, so a Ctrl-C there (which
/// the through-installer wait invites: "Ctrl-C stops waiting") reaches this run too. It is
/// held until the root half ended and, in a run aterm decided, its ask was kept or taken back,
/// then sent again: the run still stops as it would have, and the record says what the stop
/// left it. A person's run writes no record at all.
#[test]
fn a_run_on_the_persons_terminal_holds_a_stop_until_the_root_half_ended_and_the_ask_settled() {
    let argv = argv();
    for decider in [crate::init::Decider::Person, crate::init::Decider::Aterm] {
        for status in [Some(0), Some(82)] {
            let sys = console();
            sys.tty_model(|sys, _| {
                sys.stop_now();
                None
            });
            sys.on(
                &argv,
                Ran {
                    status,
                    ..Ran::default()
                },
            );
            let installs = status == Some(0);
            if installs {
                sys.on_write(&argv, std::path::Path::new(CC), b"");
            }
            let row = run(&sys, &decided(decider), Mode::Apply);
            let calls = sys.calls();
            let at = |call: &str| calls.iter().position(|seen| seen == call);
            let ran = at(&argv).expect("the root half ran");
            let restored = at("stops restored").expect("stops restored");
            assert!(
                at("defer stops").is_some_and(|held| held < ran),
                "{decider:?}: no stop is held around the root half: {calls:?}"
            );
            assert!(restored > ran, "{decider:?}: {calls:?}");
            assert!(
                calls
                    .iter()
                    .position(|seen| seen.starts_with("stop sent again"))
                    .is_some_and(|sent| sent > restored),
                "{decider:?}: the stop is sent again once the run settled: {calls:?}"
            );
            // A stop during an install that did not finish is the person's decline, which
            // no automatic ask overrides; one that finished is done.
            assert_eq!(
                row.outcome,
                if installs {
                    Outcome::Changed
                } else {
                    Outcome::Failed
                },
                "{decider:?} {status:?}: {row:?}"
            );
            if !installs {
                assert_eq!(row.why.as_deref(), Some(Unfinished::Stopped.reason()));
            }
            assert!(
                if installs {
                    matches!(record(&sys), Asked::Done { .. })
                } else {
                    matches!(record(&sys), Asked::Declined { .. })
                },
                "{decider:?} {status:?}: {:?} {calls:?}",
                record(&sys)
            );
            assert!(
                !calls.iter().any(|call| call == &format!("remove {MARK}")),
                "the attempt's end survives stop delivery: {calls:?}"
            );
        }
    }
}

/// A stop this run caught while its root half ran says whose it was. A key the person typed
/// at this terminal (Ctrl-C's `SIGINT`, Ctrl-\'s `SIGQUIT`, which the terminal sends the
/// run's foreground group) is their decline, held for its long pause. The terminal's hangup
/// (it closed, aterm quit with it, a logout ended its login) and a `SIGTERM` (a logout,
/// launchd, an update's handoff) cut the attempt off: an unfinished try, asked again by
/// itself on [`aterm_types::clt_mark::ASK_AGAIN`], never a decline. So is a root half that
/// says it stopped while this run caught nothing: no key reached this terminal.
#[test]
fn only_a_key_at_the_terminal_declines_and_a_hangup_or_sigterm_is_asked_again() {
    let argv = argv();
    let aterm = decided(crate::init::Decider::Aterm);
    let first = aterm_types::clt_mark::ASK_AGAIN.after(1).as_secs();
    // `sudo` ends on its own 1 for a stop at the password prompt; the root half on its
    // stop's code for one during the install.
    let stopped = i32::from(Unfinished::Stopped.code().expect("a root class"));
    for (signal, status, declines) in [
        (Some(libc::SIGINT), 1, true),
        (Some(libc::SIGQUIT), stopped, true),
        (Some(libc::SIGHUP), 1, false),
        (Some(libc::SIGTERM), stopped, false),
        (None, stopped, false),
    ] {
        let sys = console();
        sys.tty_model(move |sys, _| {
            sys.stop_by(signal);
            None
        });
        sys.on(&argv, Ran::exit(status, ""));
        let row = run(&sys, &aterm, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed, "{signal:?}: {row:?}");
        let read = record(&sys);
        if declines {
            assert!(
                matches!(read, Asked::Declined { declines: 1, .. }),
                "{signal:?}: a person's key declines: {read:?}"
            );
        } else {
            assert!(
                matches!(read, Asked::Unfinished { due, tries: 1 } if due == sys.now + first),
                "{signal:?} ({status}): cut off, asked again by itself: {read:?}"
            );
        }
    }
}

/// This account's record of aterm's own ask, in the test world.
const MARK: &str = "/Users//alice/Library/Application Support/aterm/command-line-tools-tab";

/// The build that asks.
const BUILD: &str = "0.9.0+gabc1234";

/// This run's process id and birth, which [`console`]'s world runs.
const PID: i32 = 4242;
const BIRTH: u64 = 7;

/// This run's process of [`BUILD`], which asks.
fn asker() -> Asker {
    Asker {
        build: BUILD.to_owned(),
        pid: PID,
        birth: BIRTH,
    }
}

/// The missing tools' item as `decider`'s run composes it, over [`MARK`] and [`BUILD`], the
/// person's choices read through `holds` each time the item reads them; a person's run reads
/// neither.
fn decided_with(
    decider: crate::init::Decider,
    holds: impl Fn() -> crate::init::Holds + Send + Sync + 'static,
) -> CommandLineTools {
    decider
        .compose(
            crate::init::Base {
                clt: Some(missing()),
                ..crate::init::Base::none()
            },
            || Unbidden::new(Some(PathBuf::from(MARK)), Some(asker()), holds),
        )
        .clt
        .expect("base.clt")
}

/// [`decided_with`] where the person's choices hold nothing.
fn decided(decider: crate::init::Decider) -> CommandLineTools {
    decided_with(decider, || crate::init::Holds::NONE)
}

/// The person's Stop of setup in System State.
fn stop() -> crate::init::Holds {
    crate::init::Holds {
        stopped: Some("you stopped setup in System State".to_owned()),
        ..crate::init::Holds::NONE
    }
}

/// What the record says now.
fn record(sys: &Fake) -> Asked {
    aterm_types::clt_mark::reading(sys, std::path::Path::new(MARK), sys.now())
}

/// Whether an attempt claimed the ask in the record and nothing gave it back: anything but a
/// free record ([`Asked::Available`]), which is what a run that asked nothing leaves.
fn marked(sys: &Fake) -> bool {
    !matches!(record(sys), Asked::Available)
}

/// H8: setup's administrator ask is never spent for good. In a run aterm decided (the
/// follow-up of a person's own package install or update), an attempt that did not finish
/// (here Apple's download failed) is claimed before `sudo` and recorded unfinished; the next
/// automatic run defers, naming when it asks again and the person's step, until
/// [`aterm_types::clt_mark::ASK_AGAIN`]'s pause passed, then asks by itself, whatever build
/// it is (an update included), and the next pause doubles. An attempt cut off with its end
/// unwritten (its asker's process gone) and a historical record are asked again at once;
/// one whose asker still runs is not asked beside.
/// NEGATIVE CONTROL: a person's decline (a stop at the password prompt) holds every later
/// automatic ask, past the longest pause, while a person's own retry still asks and records
/// its end.
#[test]
fn an_unfinished_or_cut_off_ask_comes_again_by_itself_and_a_decline_holds_it() {
    let mut sys = console();
    let argv = argv();
    sys.on(&argv, Ran::exit(82, ""));
    let aterm = decided(crate::init::Decider::Aterm);

    let row = run(&sys, &aterm, Mode::Apply);
    assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
    let calls = sys.calls();
    let claimed = calls
        .iter()
        .position(|call| *call == format!("write {MARK} 600"))
        .unwrap_or_else(|| panic!("the ask is claimed: {calls:?}"));
    let ran = calls.iter().position(|call| *call == argv).unwrap();
    assert!(claimed < ran, "claimed before sudo asks: {calls:?}");
    let first = aterm_types::clt_mark::ASK_AGAIN.after(1).as_secs();
    assert!(
        matches!(record(&sys), Asked::Unfinished { due, tries: 1 } if due == sys.now + first),
        "{:?}",
        record(&sys)
    );

    let row = run(&sys, &aterm, Mode::Plan);
    assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
    assert!(deferred(&row));
    let why = row.why.unwrap_or_default();
    assert!(
        why.contains(ASKS_AGAIN)
            && why.contains(&format!("in about {} min", first / 60))
            && why.ends_with(PERSONS_STEP),
        "{why}"
    );
    assert_eq!(asked(&sys).len(), 1, "{:?}", sys.calls());

    // The pause passed: an update's run, another build in another process, asks by itself.
    sys.now += first;
    sys.clt_askers.push((PID + 1, BIRTH));
    let update = crate::init::Decider::Aterm.compose(
        crate::init::Base {
            clt: Some(missing()),
            ..crate::init::Base::none()
        },
        || {
            Unbidden::new(
                Some(PathBuf::from(MARK)),
                Some(Asker {
                    build: "0.9.1+gdef5678".to_owned(),
                    pid: PID + 1,
                    birth: BIRTH,
                }),
                || crate::init::Holds::NONE,
            )
        },
    );
    let update = update.clt.as_ref().unwrap();
    assert_eq!(run(&sys, update, Mode::Plan).outcome, Outcome::Planned);
    assert_eq!(run(&sys, update, Mode::Apply).outcome, Outcome::Failed);
    assert_eq!(
        asked(&sys).len(),
        2,
        "asked again by itself: {:?}",
        sys.calls()
    );
    let second = aterm_types::clt_mark::ASK_AGAIN.after(2).as_secs();
    assert!(second > first);
    assert!(
        matches!(record(&sys), Asked::Unfinished { due, tries: 2 } if due == sys.now + second),
        "{:?}",
        record(&sys)
    );

    // Cut off: an asker whose process is gone left `asking`; asked again at once, adding no
    // try. One whose process runs is not asked beside.
    for (runs, outcome) in [(false, Outcome::Planned), (true, Outcome::Deferred)] {
        let mut cut = console();
        let other = Asker {
            build: BUILD.to_owned(),
            pid: 9,
            birth: 1,
        };
        if runs {
            cut.clt_askers.push((9, 1));
        }
        aterm_types::clt_mark::claim(&cut, std::path::Path::new(MARK), &other, cut.now)
            .unwrap()
            .unwrap();
        let row = run(&cut, &aterm, Mode::Plan);
        assert_eq!(row.outcome, outcome, "{runs}: {row:?}");
        if runs {
            assert!(
                row.why
                    .as_deref()
                    .is_some_and(|why| why.contains(ASKING_ELSEWHERE)),
                "{row:?}"
            );
        } else {
            assert!(
                matches!(record(&cut), Asked::Unfinished { tries: 0, .. }),
                "{:?}",
                record(&cut)
            );
        }
    }

    // A record an older aterm wrote, whose end nobody wrote, is asked again at once.
    let historical = console();
    aterm_types::clt_mark::Files::write(
        &historical,
        std::path::Path::new(MARK),
        &format!("opened {BUILD}\n"),
    )
    .unwrap();
    assert_eq!(
        run(&historical, &aterm, Mode::Plan).outcome,
        Outcome::Planned
    );

    // NEGATIVE CONTROL: a person's decline holds every automatic ask from then on.
    // Their Ctrl-C at the password prompt (`sudo` ends on its own 1), then the person's own
    // retry's download fails.
    let mut declined = console();
    let keyed = Arc::new(std::sync::atomic::AtomicBool::new(true));
    declined
        .tty_model({
            let keyed = Arc::clone(&keyed);
            move |sys, _| {
                if keyed.swap(false, std::sync::atomic::Ordering::Relaxed) {
                    sys.stop_now();
                }
                None
            }
        })
        .on(&argv, Ran::exit(1, ""))
        .on(&argv, Ran::exit(82, ""));
    assert_eq!(run(&declined, &aterm, Mode::Apply).outcome, Outcome::Failed);
    assert!(
        matches!(record(&declined), Asked::Declined { .. }),
        "{:?}",
        record(&declined)
    );
    declined.stop_by(None);
    declined.now += aterm_types::clt_mark::ASK_AGAIN.longest().as_secs() * 4;
    for mode in [Mode::Plan, Mode::Apply] {
        let row = run(&declined, &aterm, mode);
        assert_eq!(row.outcome, Outcome::Deferred, "{mode:?}: {row:?}");
        let why = row.why.unwrap_or_default();
        assert!(
            why.contains(ASK_DECLINED) && why.ends_with(PERSONS_STEP),
            "{why}"
        );
    }
    assert_eq!(asked(&declined).len(), 1, "{:?}", declined.calls());
    let typed = decided_with(crate::init::Decider::Person, || {
        panic!("an explicit retry never reads automatic holds")
    });
    assert_eq!(run(&declined, &typed, Mode::Apply).outcome, Outcome::Failed);
    assert_eq!(asked(&declined).len(), 2, "{:?}", declined.calls());
    assert!(
        matches!(record(&declined), Asked::Unfinished { tries: 1, .. }),
        "the person's retry records its own end: {:?}",
        record(&declined)
    );
}

/// A run nobody typed (a run aterm decided, the Mac Setup tab) never re-applies developer mode
/// someone turned off after setup's elevation finished once: it owes nothing for it, so with
/// nothing else owed it asks nothing, and where another step is owed its one password tells
/// the root half to leave developer mode as it reads. The tab's window can say the same of a
/// person's own switch before any finish ([`CommandLineTools::leaving_developer_mode`]).
/// NEGATIVE CONTROLS: before any finish the same run asks for developer mode and its root half
/// turns it on; a person's own typed run sets it up whatever the record says.
#[test]
fn developer_mode_turned_off_after_setup_finished_is_left_as_it_reads() {
    use crate::root_steps::Owed;
    // The tools are installed: the one password runs the root steps alone.
    let root = format!("{} --root-steps-only", argv());
    let leave = format!("{root} {LEAVE_DEVELOPER_MODE}");
    let item = |decider: crate::init::Decider, steps: Vec<Owed>| {
        decider
            .compose(
                crate::init::Base {
                    clt: Some(
                        item(Found::Present("installed".into()))
                            .with_owed(Box::new(move |_: &dyn System| steps.clone())),
                    ),
                    ..crate::init::Base::none()
                },
                || {
                    Unbidden::new(Some(PathBuf::from(MARK)), Some(asker()), || {
                        crate::init::Holds::NONE
                    })
                },
            )
            .clt
            .expect("base.clt")
    };
    let finished = || {
        let sys = console();
        aterm_types::clt_mark::Files::write(
            &sys,
            std::path::Path::new(MARK),
            &format!("done {BUILD} {}\n", sys.now - 100_000),
        )
        .unwrap();
        sys.on(&root, Ran::ok("")).on(&leave, Ran::ok(""));
        sys
    };
    let aterm = crate::init::Decider::Aterm;

    // Developer mode alone is owed, after a finish: nothing to ask for.
    let sys = finished();
    let row = run(&sys, &item(aterm, vec![Owed::Debugger]), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
    assert!(asked(&sys).is_empty(), "{:?}", sys.calls());

    // With the LAN broker owed too, the one password leaves developer mode.
    let sys = finished();
    let row = run(
        &sys,
        &item(aterm, vec![Owed::Debugger, Owed::LanBroker]),
        Mode::Apply,
    );
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(
        asked(&sys),
        std::slice::from_ref(&leave),
        "{:?}",
        sys.calls()
    );

    // The tab says the same where its window read the switch as the person's.
    let tab_sys = Arc::new(console());
    tab_sys.on(&leave, Ran::ok(""));
    let (tab_item, _, _) = in_tab(&tab_sys, Found::Present("installed".into()));
    let tab_item = tab_item
        .with_owed(Box::new(|_: &dyn System| {
            vec![Owed::Debugger, Owed::LanBroker]
        }))
        .leaving_developer_mode(true);
    let row = run(&tab_sys, &tab_item, Mode::Apply);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(
        asked(&tab_sys),
        std::slice::from_ref(&leave),
        "{:?}",
        tab_sys.calls()
    );

    // NEGATIVE CONTROL: no finish yet, so developer mode is setup's to turn on.
    let sys = console();
    sys.on(&root, Ran::ok(""));
    let row = run(&sys, &item(aterm, vec![Owed::Debugger]), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(
        asked(&sys),
        std::slice::from_ref(&root),
        "{:?}",
        sys.calls()
    );

    // NEGATIVE CONTROL: a person's typed run sets it up whatever the record says.
    let sys = finished();
    let row = run(
        &sys,
        &item(crate::init::Decider::Person, vec![Owed::Debugger]),
        Mode::Apply,
    );
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(asked(&sys), [root], "{:?}", sys.calls());
}

/// A run aterm decided asks for no password while the person's choices hold System State's
/// own Mac Setup tab (their Stop, setup records that cannot be read, a newer
/// aterm's records) or where the record has no place or cannot be read: deferred, naming why
/// and the person's step, writing nothing; a record that cannot say is named as such, never
/// as this build's ask. A group turned off holds no tab, so it holds nothing here.
#[test]
fn a_run_aterm_decided_asks_nothing_under_a_stop() {
    let newer = crate::init::Holds {
        newer: true,
        ..crate::init::Holds::NONE
    };
    assert_eq!(held_by(&crate::init::Holds::NONE), None);
    assert_eq!(held_by(&newer).as_deref(), Some(NEWER));
    assert!(held_by(&stop()).is_some());
    let sys = console();
    sys.on(&argv(), Ran::exit(88, ""));
    let row = run(
        &sys,
        &decided_with(crate::init::Decider::Aterm, stop),
        Mode::Apply,
    );
    assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
    let why = row.why.unwrap_or_default();
    assert!(
        why.contains("because you stopped setup in System State") && why.ends_with(PERSONS_STEP),
        "{why}"
    );

    let nowhere = crate::init::Decider::Aterm.compose(
        crate::init::Base {
            clt: Some(missing()),
            ..crate::init::Base::none()
        },
        || Unbidden::new(None, Some(asker()), || crate::init::Holds::NONE),
    );
    let unborn = crate::init::Decider::Aterm.compose(
        crate::init::Base {
            clt: Some(missing()),
            ..crate::init::Base::none()
        },
        || Unbidden::new(Some(PathBuf::from(MARK)), None, || crate::init::Holds::NONE),
    );
    let row = run(&sys, unborn.clt.as_ref().unwrap(), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
    let why = row.why.unwrap_or_default();
    assert!(
        why.contains(NOT_NOTED) && why.ends_with(PERSONS_STEP),
        "{why}"
    );
    let unread = console();
    unread.link(MARK, "/Users//alice/elsewhere");
    for (sys, item) in [
        (&sys, nowhere.clt.as_ref().unwrap()),
        (&unread, &decided(crate::init::Decider::Aterm)),
    ] {
        let row = run(sys, item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
        let why = row.why.unwrap_or_default();
        assert!(
            why.contains(UNREAD_RECORD) && !why.contains(ASKS_AGAIN) && why.ends_with(PERSONS_STEP),
            "{why}"
        );
    }
    let why = run(&unread, &decided(crate::init::Decider::Aterm), Mode::Plan)
        .why
        .unwrap_or_default();
    assert!(why.contains(MARK), "names the record it cannot read: {why}");

    for sys in [&sys, &unread] {
        assert!(asked(sys).is_empty(), "{:?}", sys.calls());
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.starts_with("write ") || call.starts_with("remove ")),
            "{:?}",
            sys.calls()
        );
    }
}

/// The person's Stop holds a run aterm decided whenever it is pressed before `sudo` asks:
/// composing the run reads none of their choices, its probe reads them, so a Stop recorded
/// after the run began (while the base layer or the vault's join ran) defers it, and its apply
/// reads them once more right before it claims the ask, so a Stop recorded between the probe
/// and the apply asks nothing either (the row fails in the Stop's words, naming the person's
/// step). Neither asks or writes the record.
/// NEGATIVE CONTROL: with no Stop at either read, the same run asks once.
#[test]
fn a_stop_pressed_after_the_run_began_holds_its_ask() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let reads = Arc::new(AtomicUsize::new(0));
    let now = Arc::new(Mutex::new(crate::init::Holds::NONE));
    let item = decided_with(crate::init::Decider::Aterm, {
        let (reads, now) = (Arc::clone(&reads), Arc::clone(&now));
        move || {
            reads.fetch_add(1, Ordering::SeqCst);
            now.lock().unwrap().clone()
        }
    });
    assert_eq!(reads.load(Ordering::SeqCst), 0, "composing reads no holds");
    *now.lock().unwrap() = stop();
    let sys = console();
    let row = run(&sys, &item, Mode::Apply);
    assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
    assert!(
        row.why
            .as_deref()
            .is_some_and(|why| why.contains("because you stopped setup in System State")),
        "{row:?}"
    );
    assert!(reads.load(Ordering::SeqCst) > 0);

    for stop_at_apply in [true, false] {
        let reads = Arc::new(AtomicUsize::new(0));
        let item = decided_with(crate::init::Decider::Aterm, {
            let reads = Arc::clone(&reads);
            move || {
                // The probe's read finds no Stop; the apply's, the read after it, finds one.
                if reads.fetch_add(1, Ordering::SeqCst) > 0 && stop_at_apply {
                    stop()
                } else {
                    crate::init::Holds::NONE
                }
            }
        });
        let sys = console();
        sys.on(&argv(), Ran::exit(88, ""));
        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed, "{stop_at_apply}: {row:?}");
        assert_eq!(reads.load(Ordering::SeqCst), 2, "{stop_at_apply}");
        let writes = sys
            .calls()
            .into_iter()
            .filter(|call| call.starts_with("write ") || call.starts_with("remove "))
            .count();
        if stop_at_apply {
            let why = row.why.unwrap_or_default();
            assert!(
                why.contains("because you stopped setup in System State")
                    && why.ends_with(PERSONS_STEP),
                "{why}"
            );
            assert!(asked(&sys).is_empty(), "{:?}", sys.calls());
            assert_eq!(writes, 0, "{:?}", sys.calls());
        } else {
            assert_eq!(asked(&sys).len(), 1, "{:?}", sys.calls());
            assert!(
                matches!(record(&sys), Asked::Unfinished { tries: 1, .. }),
                "{:?}",
                record(&sys)
            );
        }
    }
}

/// A run aterm decided and System State's tab claim one record: where the tab's landing
/// claimed the episode after this run's probe read the record, this run's claim right before
/// `sudo` loses, so it asks nothing (the row fails naming the other asker and the person's
/// step) and never takes back the tab's line, which it did not add.
/// NEGATIVE CONTROL: with the record left alone, the same run's claim wins and it asks.
#[test]
fn a_run_whose_claim_lost_to_the_tab_asks_nothing_and_keeps_the_tabs_line() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for tab_lands in [true, false] {
        // The window that claims for the tab runs beside this run.
        let tab = Asker {
            build: BUILD.to_owned(),
            pid: 5151,
            birth: 9,
        };
        let mut world = console();
        world.clt_askers.push((tab.pid, tab.birth));
        let sys = Arc::new(world);
        sys.on(&argv(), Ran::exit(82, ""));
        let reads = Arc::new(AtomicUsize::new(0));
        let item = decided_with(crate::init::Decider::Aterm, {
            let (reads, sys) = (Arc::clone(&reads), Arc::clone(&sys));
            move || {
                // The tab lands between this run's probe and its apply: the apply's read of the
                // holds, the last before its claim, is when.
                if reads.fetch_add(1, Ordering::SeqCst) == 1 && tab_lands {
                    assert!(
                        aterm_types::clt_mark::claim(
                            &*sys,
                            std::path::Path::new(MARK),
                            &tab,
                            sys.now
                        )
                        .unwrap()
                        .is_some()
                    );
                }
                crate::init::Holds::NONE
            }
        });
        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed, "{tab_lands}: {row:?}");
        let calls = sys.calls();
        if tab_lands {
            let why = row.why.unwrap_or_default();
            assert!(
                why.contains(ASKING_ELSEWHERE) && why.ends_with(PERSONS_STEP),
                "{why}"
            );
            assert!(asked(&sys).is_empty(), "{calls:?}");
            assert!(
                matches!(record(&sys), Asked::Asking),
                "the tab's line stays: {calls:?}"
            );
            assert_eq!(
                calls
                    .iter()
                    .filter(|call| call.starts_with("write ") || call.starts_with("remove "))
                    .collect::<Vec<_>>(),
                [&format!("write {MARK} 600")],
                "the tab's write alone: {calls:?}"
            );
        } else {
            assert_eq!(asked(&sys).len(), 1, "{calls:?}");
            assert!(
                matches!(record(&sys), Asked::Unfinished { tries: 1, .. }),
                "a failed install is recorded unfinished: {calls:?}"
            );
        }
    }
}

/// A pending automatic claim must not release a later explicit person's real attempt,
/// even when the automatic sudo was proven never started afterward: B's attempt asks on while
/// its process runs, the person's end that follows stands, and the next automatic ask waits
/// for its pause. NEGATIVE CONTROL: where B's process is gone too, B's attempt was cut off, and
/// A's release leaves it so: due again at once, a cut-off adding no try.
#[test]
fn an_explicit_attempt_survives_an_older_automatic_no_start_result() {
    use std::sync::atomic::{AtomicBool, Ordering};
    const B: (i32, u64) = (9, 1);
    for b_runs in [true, false] {
        let mut sys = console();
        if b_runs {
            sys.clt_askers.push(B);
        }
        let first = AtomicBool::new(true);
        sys.tty_model(move |sys, _| {
            if first.swap(false, Ordering::SeqCst) {
                // B is another process: record its explicit attempt while A owns this
                // process's terminal. Nesting B's fake tty here deadlocks that same mutex.
                let other = Asker {
                    build: BUILD.to_owned(),
                    pid: B.0,
                    birth: B.1,
                };
                aterm_types::clt_mark::explicit(sys.clt_mark(), Path::new(MARK), &other, sys.now)
                    .unwrap();
                None
            } else {
                Some(Ran::exit(1, ""))
            }
        });
        assert_eq!(
            run(&sys, &decided(crate::init::Decider::Aterm), Mode::Apply).outcome,
            Outcome::Failed
        );
        if !b_runs {
            assert!(
                matches!(record(&sys), Asked::Unfinished { tries: 0, due } if due <= sys.now),
                "CONTROL: a cut-off B is asked again at once: {:?}",
                record(&sys)
            );
            continue;
        }
        assert!(
            matches!(record(&sys), Asked::Asking),
            "A's no-start leaves B's attempt asking: {:?}",
            record(&sys)
        );
        // B's process ends, its ask cut off. A's proven no-start gave back only a claim A
        // held: the person's attempt that follows is recorded, and the next automatic ask
        // waits for its pause.
        sys.clt_askers.retain(|asker| *asker != B);
        let person = decided(crate::init::Decider::Person);
        assert_eq!(run(&sys, &person, Mode::Apply).outcome, Outcome::Failed);
        assert!(
            matches!(record(&sys), Asked::Unfinished { .. }),
            "the person's end stands: {:?}",
            record(&sys)
        );
        assert_eq!(
            run(&sys, &decided(crate::init::Decider::Aterm), Mode::Apply).outcome,
            Outcome::Deferred
        );
        assert_eq!(
            asked(&sys).len(),
            2,
            "no third unsolicited attempt: {:?}",
            sys.calls()
        );
    }
}

/// Every end after `sudo` started may have asked a person, and is recorded: a refused
/// password, a download or installer failure, Apple's window closed (by a button nothing
/// tells), a stop no key at this terminal made and an unread end are unfinished (asked again
/// by itself after the pause); only a person's key declines
/// (`only_a_key_at_the_terminal_declines_and_a_hangup_or_sigterm_is_asked_again`). Only a
/// proven failure to start gives this exact claim back, so the next run plans at once.
#[test]
fn each_end_of_a_password_attempt_is_recorded_and_only_proven_no_start_releases_it() {
    use crate::port::RunFailed;
    fn unfinished(asked: &Asked) -> bool {
        matches!(asked, Asked::Unfinished { tries: 1, .. })
    }
    fn available(asked: &Asked) -> bool {
        matches!(asked, Asked::Available)
    }
    for (reply, recorded, plans) in [
        (
            Some(Ok(Ran::exit(82, ""))),
            unfinished as fn(&Asked) -> bool,
            false,
        ),
        (Some(Ok(Ran::exit(83, ""))), unfinished, false),
        (Some(Ok(Ran::exit(1, ""))), unfinished, false),
        (Some(Ok(Ran::exit(89, ""))), unfinished, false),
        // The root helper's stop with no key caught here: a hangup or a logout's SIGTERM.
        (Some(Ok(Ran::exit(85, ""))), unfinished, false),
        // Apple's window closed: which button closed it nothing tells, so no decline.
        (Some(Ok(Ran::exit(88, ""))), unfinished, false),
        // A lost wait result or a timeout may follow a person's typing: no proof that `sudo`
        // never ran, so unfinished, never given back.
        (
            Some(Err(RunFailed::AfterStart("lost wait result".into()))),
            unfinished,
            false,
        ),
        (
            Some(Err(RunFailed::TimedOut(std::time::Duration::from_secs(60)))),
            unfinished,
            false,
        ),
        (
            Some(Err(RunFailed::Spawn("exec failed".into()))),
            available,
            true,
        ),
        (
            Some(Err(RunFailed::Refused("before child start".into()))),
            available,
            true,
        ),
        (None, available, true), // The port's actual unscripted Spawn failure.
    ] {
        let sys = console();
        let said = format!("{reply:?}");
        if let Some(reply) = reply {
            sys.on_result(&argv(), reply);
        }
        let aterm = decided(crate::init::Decider::Aterm);
        let row = run(&sys, &aterm, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        assert!(
            recorded(&record(&sys)),
            "{said}: {:?} {:?}",
            record(&sys),
            sys.calls()
        );
        assert_eq!(
            run(&sys, &aterm, Mode::Plan).outcome,
            if plans {
                Outcome::Planned
            } else {
                Outcome::Deferred
            },
            "{said}"
        );
        // A kept attempt's pause holds the next automatic run: no second password.
        if !plans {
            let attempts = asked(&sys);
            let next = decided(crate::init::Decider::Aterm);
            assert_eq!(run(&sys, &next, Mode::Apply).outcome, Outcome::Deferred);
            assert_eq!(asked(&sys), attempts);
        }
    }
}

/// `sudo`'s exit says why the tab's install did not finish: 0 done, `sudo`'s own 1 the Mac
/// password, each root class's code that class, a signal after a stop the person sent
/// stopped, and anything else (a usage 2, a panic's 101, a failed exec's 126 or 127, a
/// signal nobody sent) the other class. The apply's error is the plain reason alone, never
/// the argv or a path.
#[test]
fn the_root_halfs_exit_says_why_the_tab_install_did_not_finish() {
    assert_eq!(Unfinished::after_root(Some(0)), Ok(()));
    assert_eq!(Unfinished::after_root(Some(1)), Err(Unfinished::Password));
    for (code, why) in [
        (80, Unfinished::Catalog),
        (81, Unfinished::NoneOffered),
        (82, Unfinished::Download),
        (83, Unfinished::Installer),
        (84, Unfinished::Busy),
        (85, Unfinished::Stopped),
        (86, Unfinished::AppleWindow),
        (87, Unfinished::Other),
        (88, Unfinished::WindowClosed),
        (89, Unfinished::Unread),
    ] {
        assert_eq!(why.code(), Some(code), "{why:?}");
        assert_eq!(
            Unfinished::after_root(Some(i32::from(code))),
            Err(why),
            "{code}"
        );
    }
    for code in [
        Some(2),
        Some(79),
        Some(90),
        Some(101),
        Some(126),
        Some(127),
        Some(-1),
        None,
    ] {
        assert_eq!(
            Unfinished::after_root(code),
            Err(Unfinished::Other),
            "{code:?}"
        );
    }

    for (status, stopped, why) in [
        (Some(1), false, Unfinished::Password),
        (Some(84), false, Unfinished::Busy),
        (Some(82), false, Unfinished::Download),
        (None, false, Unfinished::Other),
        (None, true, Unfinished::Stopped),
        // A Ctrl-C at `sudo`'s password prompt: `sudo` ends on its own 1.
        (Some(1), true, Unfinished::Stopped),
        (Some(82), true, Unfinished::Stopped),
    ] {
        let fake = console();
        if stopped {
            fake.tty_model(|sys, _| {
                sys.stop_now();
                None
            });
        }
        let sys = Arc::new(fake);
        let (item, said, tab) = in_tab(&sys, Found::Missing("no developer directory".into()));
        sys.on(
            &argv(),
            Ran {
                status,
                ..Ran::default()
            },
        );
        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed, "{status:?}: {row:?}");
        assert_eq!(row.why.as_deref(), Some(why.reason()), "{status:?}");
        assert_eq!(tab.unfinished(), Some(why), "{status:?}");
        for after in [
            Unfinished::LowData,
            Unfinished::NotOwnLogin,
            Unfinished::Busy,
            Unfinished::AppleWindow,
        ] {
            assert_eq!(
                tab.exit(after),
                1,
                "a post-sudo {after:?} diagnosis cannot release the episode"
            );
        }
        assert_eq!(said.lock().unwrap().len(), 1, "the intro alone: {status:?}");
    }

    // Completed tools cannot hide a failed root helper transaction. This
    // follows the actual tab producer through its installed-tools fallback.
    let mut fake = console();
    fake.running_app = Some("/Applications/aterm.app".into());
    let sys = Arc::new(fake);
    let image = aterm_uds::lan::install::test_image(0x0100_000c, &[b"this build"]);
    sys.file("/Applications/aterm.app/Contents/MacOS/aterm", &image)
        .file(aterm_uds::lan::IMAGE_EXECUTABLE, &image)
        .file(
            aterm_uds::lan::PLIST_PATH,
            aterm_uds::lan::install::plist_text(Path::new("/Applications/aterm.app")).as_bytes(),
        )
        .on(
            "/bin/launchctl print system/systems.alab.aterm-lan",
            Ran::exit(113, "Could not find service"),
        );
    let (item, _, tab) = in_tab(&sys, Found::Missing("no developer directory".into()));
    sys.on(
        &argv(),
        Ran::exit(i32::from(Unfinished::Other.code().unwrap()), ""),
    )
    .on_write(&argv(), Path::new(CC), b"");
    let row = run(&sys, &item, Mode::Apply);
    assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
    assert_eq!(tab.unfinished(), Some(Unfinished::Other));
}

#[test]
fn a_successful_installer_still_needs_a_working_compiler_and_can_recover() {
    for (unready, unfinished) in [
        (
            Found::Refuses("SDK cannot link".into()),
            Unfinished::Refuses,
        ),
        (
            Found::Unknown("compiler did not answer".into()),
            Unfinished::Unread,
        ),
    ] {
        let sys = Arc::new(console());
        let (mut item, _, tab) = in_tab(&sys, Found::Missing("not installed".into()));
        item.found = Box::new(move |sys: &dyn System| match sys.read_file(Path::new(CC)) {
            Ok(bytes) if bytes == b"ready" => Found::Present("compiler links".into()),
            Ok(_) => unready.clone(),
            Err(_) => Found::Missing("not installed".into()),
        });
        sys.on(&argv(), Ran::ok(""))
            .on_write(&argv(), Path::new(CC), b"unready");

        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        assert_eq!(tab.unfinished(), Some(unfinished));
        assert_eq!(asked(&sys), [argv()]);

        // Installing the files and exiting successfully is no compiler health proof.
        let row = run(&sys, &item, Mode::Apply);
        assert!(
            matches!(row.outcome, Outcome::Blocked | Outcome::Unknown),
            "{row:?}"
        );
        assert_eq!(tab.unfinished(), Some(unfinished));
        assert_eq!(
            asked(&sys),
            [argv()],
            "an unready read-back starts no second prompt"
        );

        sys.file(CC, b"ready");
        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
        assert_eq!(asked(&sys), [argv()], "recovery needs no new root episode");
    }
}

/// A stop the person sent while the root half ran (a Ctrl-C in the tab, which reaches the
/// root half too) leaves the tab's run waiting for it: the root half finishes its step, and
/// tools that arrived anyway end the tab on installed, whatever its exit said.
#[test]
fn in_the_tab_a_stop_waits_for_the_root_half_and_tools_that_arrived_are_installed() {
    let fake = console();
    fake.tty_model(|sys, _| {
        sys.stop_now();
        None
    });
    let sys = Arc::new(fake);
    let (item, _, tab) = in_tab(&sys, Found::Missing("no developer directory".into()));
    let argv = argv();
    sys.on(
        &argv,
        Ran {
            status: None,
            ..Ran::default()
        },
    )
    .on_write(&argv, std::path::Path::new(CC), b"");
    let row = run(&sys, &item, Mode::Apply);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
    assert_eq!(tab.unfinished(), None);
}

/// Each root code identifies only its own class and avoids generic process failures.
/// The tab preserves the distinct codes used by its parent for download waits and stops.
#[test]
fn root_codes_are_distinct_and_tab_exit_codes_preserve_their_classes() {
    let mut codes = Vec::new();
    for why in Unfinished::ALL {
        match why {
            Unfinished::LowData => {
                assert_eq!(
                    why.tab_exit(),
                    aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA
                );
            }
            Unfinished::NotAdmin => {
                assert_eq!(
                    why.tab_exit(),
                    aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN
                );
            }
            Unfinished::Stopped | Unfinished::WindowClosed => {
                assert_eq!(
                    why.tab_exit(),
                    if why == Unfinished::Stopped {
                        aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED
                    } else {
                        aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_WINDOW_CLOSED
                    }
                );
            }
            _ => {
                assert_eq!(why.tab_exit(), 1, "{why:?}");
            }
        }
        if let Some(code) = why.code() {
            assert!(![0, 1, 2, 101, 126, 127].contains(&code), "{why:?}");
            assert!(!codes.contains(&code), "{why:?}");
            codes.push(code);
        }
    }
}

/// In the tab, what the probe finds that no install fixes is noted as its class: tools that
/// refuse to build, a probe that could not tell, and no terminal to ask in. Nothing runs.
#[test]
fn in_the_tab_the_probe_notes_what_no_install_fixes() {
    for (found, why) in [
        (Found::Refuses("license".into()), Unfinished::Refuses),
        (Found::Unknown("no answer".into()), Unfinished::Unread),
        (
            Found::Missing("no developer directory".into()),
            Unfinished::Other,
        ),
    ] {
        let mut fake = console();
        fake.tty = false;
        let sys = Arc::new(fake);
        let (item, said, tab) = in_tab(&sys, found.clone());
        let row = run(&sys, &item, Mode::Apply);
        assert!(
            row.outcome.is_failure() || row.outcome == Outcome::Blocked,
            "{row:?}"
        );
        assert_eq!(tab.unfinished(), Some(why), "{found:?}");
        assert!(said.lock().unwrap().is_empty());
        assert!(asked(&sys).is_empty(), "{:?}", sys.calls());
    }
}
