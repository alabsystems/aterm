// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A parked settings pass must hear its account returning to the console.

use super::*;
use crate::macos_doctor::{Host, NativeHost};
use aterm_machines::model::{PValue, User};
use aterm_machines::port::{Fake as Machine, Ran, Stat};
use std::cell::Cell;

fn scrolling(console: u32) -> NativeHost {
    let user = User {
        name: "alice".into(),
        uid: 501,
        gid: 20,
        home: "/Users//alice".into(),
        shell: "/bin/zsh".into(),
        admin: false,
    };
    let mut sys = Machine::root();
    sys.euid = user.uid;
    sys.logged_in(&user);
    sys.stat_of(
        "/dev/console",
        Stat {
            uid: console,
            gid: user.gid,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 0,
        },
    );
    sys.scroll_state(false);
    sys.on(
        "alice$ /usr/bin/defaults export -g -",
        Ran::ok(&aterm_machines::plist::document(&PValue::Dict(vec![(
            "com.apple.swipescrolldirection".into(),
            PValue::Bool(false),
        )]))),
    );
    let mut host = NativeHost::over(
        Box::new(sys),
        Box::new(aterm_machines::init::prepare_workstation_selected),
    );
    host.select(selected(&[Setting::NaturalScrolling]).selection);
    host
}

fn keyboard(attached: bool) -> NativeHost {
    let mut host = NativeHost::over(
        Box::new(InitialSetupAdmissionStopTest::keyboard_system(attached)),
        Box::new(aterm_machines::init::prepare_workstation_selected),
    );
    host.select(selected(&[Setting::CapsLock]).selection);
    host
}

#[test]
fn a_parked_first_caps_pass_resumes_when_a_new_keyboard_arrives_at_the_same_console() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    let plan = selected(&[Setting::CapsLock]);
    let revision = authorize_at(root, &plan).unwrap().unwrap();
    let mut host = Fake::default();
    for _ in 0..10 {
        host.native_workstation = Some(keyboard(false));
        let (report, next) =
            execute_initial_for_revision(&mut host, root, revision, &mut None, SetupHooks::NONE)
                .unwrap()
                .unwrap();
        assert!(report.needs_attention(), "{}", report.to_text());
        assert_eq!(next, SetupNext::Retry);
    }
    let saved = progress(root).unwrap().unwrap();
    assert_eq!(saved.done & Stage::Workstation.bit(), 0);
    assert!(saved.only.is_none());
    assert!(!saved.completed);
    assert!(Automatic::load(root, revision).unwrap().failed.is_empty());
    assert_eq!(
        input_setup_scope_at(root).unwrap(),
        InputSetupScope::Pending(revision)
    );
    let attached = Cell::new(false);
    let mark = || {
        initial_setup_park_mark_with(Some(root), None, 1_800_000_600, || {
            panic!("Caps-only setup reads no network")
        })
        .with_console(|| Some(true))
        .with_keyboard_services(|| keyboard(attached.get()).sys.keyboard_service_ids())
    };
    let parked = mark();
    assert_eq!(parked.next_due(), None);
    assert!(parked.watches().none());
    assert!(!parked.waits_on_settings());
    assert!(
        !mark().facts_changed_since(&parked),
        "an unchanged first sample grants no retry"
    );

    attached.set(true);
    let recovered = keyboard(attached.get());
    assert_eq!(keyboard(false).sys.keyboard_service_ids().unwrap(), [41]);
    assert_eq!(recovered.sys.keyboard_service_ids().unwrap(), [41, 42]);
    host.native_workstation = Some(recovered);
    let (report, next) =
        execute_initial_for_revision(&mut host, root, revision, &mut None, SetupHooks::NONE)
            .unwrap()
            .unwrap();
    assert!(rows_ready(&report.rows), "{}", report.to_text());
    assert_eq!(next, SetupNext::Done);
    assert!(finished_at(root).unwrap());
    assert_eq!(
        input_setup_scope_at(root).unwrap(),
        InputSetupScope::Applied(revision)
    );
    save_progress(root, saved).unwrap();
    assert!(
        mark().facts_changed_since(&parked),
        "the parked first Caps pass must hear new native service 42 without a console change"
    );
}

#[test]
fn a_parked_workstation_resumes_when_its_console_returns() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    let plan = selected(&[Setting::NaturalScrolling]);
    let revision = authorize_at(root, &plan).unwrap().unwrap();
    let console = Cell::new(502);
    let mut host = Fake::default();
    for _ in 0..10 {
        host.native_workstation = Some(scrolling(console.get()));
        let (report, next) =
            execute_initial_for_revision(&mut host, root, revision, &mut None, SetupHooks::NONE)
                .unwrap()
                .unwrap();
        assert!(report.needs_attention(), "{}", report.to_text());
        assert_eq!(next, SetupNext::Retry);
    }
    let saved = progress(root).unwrap().unwrap();
    assert_eq!(saved.done & Stage::Workstation.bit(), 0);
    assert!(!saved.completed);
    assert!(Automatic::load(root, revision).unwrap().failed.is_empty());
    let mark = || {
        initial_setup_park_mark_with(Some(root), None, 1_800_000_600, || {
            panic!("this accepted plan selects no network stage")
        })
        .with_console(|| {
            Some(!aterm_machines::console::remote(
                scrolling(console.get()).sys.as_ref(),
            ))
        })
    };
    let parked = mark();
    assert_eq!(parked.next_due(), None);
    assert!(parked.watches().none());
    assert!(!parked.waits_on_settings());

    console.set(501);
    host.native_workstation = Some(scrolling(console.get()));
    let (report, next) =
        execute_initial_for_revision(&mut host, root, revision, &mut None, SetupHooks::NONE)
            .unwrap()
            .unwrap();
    assert!(rows_ready(&report.rows), "{}", report.to_text());
    assert_eq!(next, SetupNext::Done);
    assert!(finished_at(root).unwrap());
    // Restore only the parked record: the local account's console return has changed no
    // setup file or retry deadline. The worker must hear that useful input, not a new plan.
    save_progress(root, saved).unwrap();
    assert!(
        mark().facts_changed_since(&parked),
        "the parked admitted workstation must hear its console return"
    );
}

#[test]
fn console_facts_belong_only_to_current_unheld_pending_preferences() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    let plan = selected(&[Setting::NaturalScrolling]);
    let revision = authorize_at(root, &plan).unwrap().unwrap();
    let original = progress(root).unwrap().unwrap();
    let records = || observation_at(root).unwrap().records;
    assert_eq!(pending_inputs::revision(&records()), Some(revision));
    assert_eq!(
        input_setup_scope_at(root).unwrap(),
        InputSetupScope::Inactive
    );
    let _ = initial_setup_park_mark_with(Some(root), None, 1_800_000_600, || None)
        .with_console(|| Some(true))
        .with_keyboard_services(|| panic!("scrolling alone observes no keyboard service"));
    let retired = || {
        let mark = initial_setup_park_mark_with(Some(root), None, 1_800_000_600, || None)
            .with_console(|| panic!("held or retired preferences read no console fact"));
        assert_eq!(mark.workstation_run, None);
        assert_eq!(pending_inputs::revision(&records()), None);
    };
    for closed in [
        Progress {
            admitted: false,
            ..original
        },
        Progress {
            completed: true,
            ..original
        },
        Progress {
            stopped: true,
            ..original
        },
        Progress {
            done: original.done | Stage::Workstation.bit(),
            ..original
        },
        Progress {
            only: Some(Selection::none()),
            ..original
        },
    ] {
        save_progress(root, closed).unwrap();
        retired();
    }
    save_progress(root, original).unwrap();
    request_stop_at(root, revision).unwrap();
    retired();
    settle_stop_request(root, StopRequest::Revision(revision)).unwrap();
    save_restore(root).unwrap();
    retired();

    record_pending(root).unwrap();
    let off = Plan::from(Selection::none());
    save_plan(root, &off).unwrap();
    retired(); // A changed saved plan cannot borrow the old admission.
    let mut bound = original;
    bound.bind(&off);
    save_progress(root, bound).unwrap();
    retired(); // Nor can an all-off plan observe a console input for a setting.
}

#[test]
fn a_console_recovery_cannot_lift_a_stage_parked_by_its_ledger() {
    for hung in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let plan = selected(&[Setting::NaturalScrolling]);
        let revision = authorize_at(root, &plan).unwrap().unwrap();
        ledger::update(root, revision, |ledger| {
            let stamp = ledger::Stamp {
                at: 10,
                ..ledger::Stamp::default()
            };
            for second in [false, true] {
                assert_eq!(
                    ledger.begin(Stage::Workstation, 0, stamp, root),
                    ledger::Admit::Run
                );
                if second && hung {
                    assert!(ledger.mark_hung(Stage::Workstation, stamp.at));
                }
                ledger.recover(0, None);
            }
        })
        .unwrap();
        let before = ledger::read(root, progress(root).unwrap());
        assert_eq!(
            before.parked(Stage::Workstation),
            Some(if hung {
                ledger::Park::Hung
            } else {
                ledger::Park::Crashed
            })
        );
        let mark = |at_console| {
            initial_setup_park_mark_with(Some(root), None, 1_800_000_600, || None)
                .with_console(|| Some(at_console))
        };
        assert!(mark(true).facts_changed_since(&mark(false)));
        let mut host = Fake::default().with_workstation(scrolling(501));
        let (report, next) =
            execute_initial_for_revision(&mut host, root, revision, &mut None, SetupHooks::NONE)
                .unwrap()
                .unwrap();
        assert_ne!(next, SetupNext::Done);
        assert!(report.needs_attention());
        assert!(host.calls.is_empty(), "a local input is not Check again");
        assert_eq!(
            ledger::read(root, progress(root).unwrap()).parked(Stage::Workstation),
            before.parked(Stage::Workstation)
        );
    }
}

#[test]
fn first_caps_observation_is_bounded_and_cannot_borrow_a_held_or_retired_scope() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    let plan = selected(&[Setting::CapsLock]);
    let revision = authorize_at(root, &plan).unwrap().unwrap();
    let mark = |ids| {
        initial_setup_park_mark_with(Some(root), None, 1_800_000_600, || None)
            .with_console(|| Some(true))
            .with_keyboard_services(|| Ok(ids))
    };
    let known = mark(vec![41, 42]);
    assert_eq!(mark(vec![42, 41]), known);
    for ids in [vec![41, 41], vec![0], (1..=4097).collect()] {
        let mut unread = mark(ids);
        assert!(!unread.facts_changed_since(&known));
        unread.retain_known_from(&known);
        assert_eq!(unread, known);
    }
    let _ = initial_setup_park_mark_with(Some(root), None, 1_800_000_600, || None)
        .with_console(|| Some(false))
        .with_keyboard_services(|| panic!("another console reads no keyboard facts"));

    let retired = || {
        assert_eq!(
            input_setup_scope_at(root).unwrap(),
            InputSetupScope::Inactive
        );
        let _ = initial_setup_park_mark_with(Some(root), None, 1_800_000_600, || None)
            .with_console(|| Some(true))
            .with_keyboard_services(|| panic!("held Caps reads no hardware"));
    };
    for hung in [false, true] {
        ledger::update(root, revision, |ledger| {
            ledger.retry();
            let stamp = ledger::Stamp {
                at: 10,
                ..ledger::Stamp::default()
            };
            for second in [false, true] {
                assert_eq!(
                    ledger.begin(Stage::Workstation, 0, stamp, root),
                    ledger::Admit::Run
                );
                if second && hung {
                    assert!(ledger.mark_hung(Stage::Workstation, stamp.at));
                }
                ledger.recover(0, None);
            }
        })
        .unwrap();
        retired();
    }
    ledger::update(root, revision, |ledger| {
        ledger.retry();
    })
    .unwrap();
    assert_eq!(
        input_setup_scope_at(root).unwrap(),
        InputSetupScope::Pending(revision)
    );
    request_stop_at(root, revision).unwrap();
    retired();
    settle_stop_request(root, StopRequest::Revision(revision)).unwrap();
    save_restore(root).unwrap();
    retired();

    // A later ordinary pass can finish Caps, but completed Caps supplies no pending-HID debt.
    record_pending(root).unwrap();
    record_applied(root, plan.selection).unwrap();
    assert_eq!(
        input_setup_scope_at(root).unwrap(),
        InputSetupScope::Applied(revision)
    );
    let _ = initial_setup_park_mark_with(Some(root), None, 1_800_000_600, || None)
        .with_console(|| Some(true))
        .with_keyboard_services(|| panic!("completed Caps has no pending hardware observation"));
}
