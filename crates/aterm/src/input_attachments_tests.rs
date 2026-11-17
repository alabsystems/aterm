// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_machines::port::System as _;

fn spawn_background_with(
    sys: &dyn aterm_machines::port::System,
    start: impl FnOnce(fn(), Option<fn() -> Result<(), String>>, Option<fn()>),
) {
    if BackgroundStartup::default().take(sys) {
        start(|| {}, None, None);
    }
}

fn keyboard_dump(registries: &[u64], native: &[(i64, i64)]) -> String {
    let pairs = native
        .iter()
        .map(|(source, destination)| {
            format!("<dict><key>Src</key><integer>{source}</integer><key>Dst</key><integer>{destination}</integer></dict>")
        })
        .collect::<String>();
    let services = registries
        .iter()
        .map(|registry| {
            format!(
                "<dict><key>IORegistryEntryID</key><integer>{registry}</integer>\
                 <key>PrimaryUsagePage</key><integer>1</integer>\
                 <key>PrimaryUsage</key><integer>6</integer>\
                 <key>ServiceFilterDebug</key><array><dict><key>plugin</key><dict>\
                 <key>Class</key><string>IOHIDKeyboardFilter</string>\
                 <key>HIDKeyboardModifierMappingPairs</key><array>{pairs}</array>\
                 <key>UserKeyMapping</key><array/>\
                 <key>FnFunctionUsageMap</key><array/>\
                 <key>FnKeyboardUsageMap</key><array/>\
                 <key>NumLockKeyboardUsageMap</key><array/>\
                 </dict></dict></array></dict>"
            )
        })
        .collect::<String>();
    format!(
        "<plist version=\"1.0\"><dict><key>ServiceRecords</key><array>{services}</array></dict></plist>"
    )
}

/// Reuse the real Fake input ports for logical keeper cases. Receipts still come
/// from the ordinary final probe; no success flag can manufacture their state.
fn mapped_keyboard_system(registries: &[u64]) -> aterm_machines::port::Fake {
    use aterm_machines::items::keyboard::{CAPS_LOCK, LEFT_CONTROL};
    use aterm_machines::port::Ran;
    let mut sys =
        aterm_cli::macos_doctor::InitialSetupAdmissionStopTest::keyboard_system(true, &[]);
    sys.keyboard_services = Some(Ok(registries.to_vec()));
    let mut census = format!(
        "Services:\n{:<12}{:<12}{:<16}{:<24}Built-In\n",
        "VendorID", "ProductID", "RegistryID", "Product"
    );
    let mut properties = String::from("RegistryID Key Value\n");
    for id in registries {
        census.push_str(&format!(
            "{:<12}{:<12}{:<16}{:<24}0\n",
            "0xfffe",
            "0xa9",
            format!("0x{id:x}"),
            "Fixture keyboard"
        ));
        properties.push_str(&format!("{id:x} UserKeyMapping (null)\n"));
    }
    census.push_str("\nDevices:\n");
    let dump = keyboard_dump(registries, &[(CAPS_LOCK, LEFT_CONTROL)]);
    sys.simulate(move |_, cmd| {
        let args: Vec<_> = cmd.args.iter().filter_map(|arg| arg.to_str()).collect();
        match cmd.program.file_name().and_then(|name| name.to_str()) {
            Some("hidutil") if args.first() == Some(&"list") => Some(Ran::ok(&census)),
            Some("hidutil") if args.first() == Some(&"property") => Some(Ran::ok(&properties)),
            Some("hidutil") if args.first() == Some(&"dump") => Some(Ran::ok(&dump)),
            Some("defaults") => Some(Ran::ok(&aterm_machines::plist::document(
                &aterm_machines::model::PValue::Dict(Vec::new()),
            ))),
            _ => None,
        }
    });
    sys
}

fn observations(registries: &[u64]) -> Result<Vec<KeyboardServiceObservation>, String> {
    mapped_keyboard_system(registries).keyboard_service_observations()
}

fn verified(registries: &[u64]) -> Result<Option<KeyboardAttachmentActivation>, String> {
    let sys = mapped_keyboard_system(registries);
    let result = aterm_machines::init::activate_keyboard_attachments(
        &sys,
        registries,
        &|_| false,
        &|| true,
        &|| panic!("the modeled mapping is already in place"),
    )?;
    assert!(sys.keyboard_changes().is_empty());
    Ok(Some(result))
}

fn native_pending_caps(
    root: &std::path::Path,
) -> (
    aterm_cli::macos_doctor::InitialSetupAdmissionStopTest<'_>,
    u64,
) {
    use aterm_cli::macos_doctor::{
        InitialSetupAdmissionFault, InitialSetupAdmissionStopTest, SetupNext, input_setup_scope_at,
    };
    use aterm_types::macos_setup::{Plan, Selection, Setting};

    let mut selected = Selection::none();
    selected.set(Setting::CapsLock, true);
    let mut setup = InitialSetupAdmissionStopTest::startup(root)
        .unwrap()
        .with_plan(Plan::from(selected));
    let revision = setup
        .persist(InitialSetupAdmissionFault::None)
        .unwrap()
        .unwrap();
    let (report, next) = setup
        .native_workstation_pass(revision, || {
            Box::new(InitialSetupAdmissionStopTest::keyboard_system(false, &[]))
        })
        .unwrap()
        .unwrap();
    assert!(report.needs_attention(), "{}", report.to_text());
    assert_eq!(next, SetupNext::Retry);
    assert_eq!(
        input_setup_scope_at(root).unwrap(),
        InputSetupScope::Pending(revision)
    );
    assert_eq!(setup.input_activation_revision().unwrap(), None);
    (setup, revision)
}

#[test]
fn a_window_keeps_the_exit_handoff_until_the_sampler_is_joinable() {
    use std::sync::mpsc;

    let (sampled, sampled_rx) = mpsc::channel();
    let (finish, finish_rx) = mpsc::channel();
    let (woke, woke_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let notifications = Notifications {
        changed: || {},
        invalidated: || {},
        console_return: None,
        pending: Some(|| {}),
        ended: Some(Arc::new(move || {
            woke.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(60))
                .unwrap();
        })),
    };
    let mut owner = Supervisor::new(notifications.clone());
    let clock = owner.clock;
    assert!(
        owner
            .maintain_with(|actor, stop, ending| {
                spawn_worker_with(
                    actor,
                    stop,
                    ending,
                    clock,
                    notifications,
                    move |_, _, _, _| {
                        sampled.send(()).unwrap();
                        finish_rx.recv_timeout(Duration::from_secs(60)).unwrap();
                    },
                )
            })
            .is_none()
    );
    sampled_rx.recv_timeout(Duration::from_secs(60)).unwrap();
    assert!(
        owner
            .maintain_with(|_, _, _| panic!("one live sampler"))
            .is_none(),
        "healthy idle windows gain no timer"
    );
    finish.send(()).unwrap();
    woke_rx.recv_timeout(Duration::from_secs(60)).unwrap();
    assert!(
        !owner.worker.as_ref().unwrap().is_finished(),
        "the actual exit callback still holds the worker"
    );
    assert!(
        owner
            .maintain_with(|_, _, _| panic!("join before replacement"))
            .is_some(),
        "an early exit wake must leave a deadline"
    );
    release.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(60);
    while !owner.worker.as_ref().unwrap().is_finished() {
        assert!(Instant::now() < until, "exit handoff did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        owner
            .maintain_with(|_, _, _| panic!("retained restart backoff"))
            .is_some()
    );
    assert!(owner.worker.is_none(), "the host reaped its owned handle");
}

#[test]
fn a_headless_keeper_reads_nothing_of_a_stopped_native_pending_caps_run() {
    use aterm_cli::macos_doctor::{InitialSetupAdmissionStopTest, input_setup_scope_at};
    use std::cell::Cell;

    let dir = aterm_tempfile::tempdir().unwrap();
    let (setup, revision) = native_pending_caps(dir.path());
    setup.stop_revision(revision).unwrap();
    assert_eq!(
        input_setup_scope_at(dir.path()).unwrap(),
        InputSetupScope::Inactive
    );
    let actor = InitialSetupAdmissionStopTest::keyboard_system(true, &[]);
    let started = Cell::new(false);
    let mut inputs = Inputs::default();
    spawn_background_with(&actor, |_, _, _| {
        started.set(true);
        let stopped_handoff = || -> () {
            panic!("Stop withholds the pending nudge");
        };
        inputs
            .round(
                true,
                0,
                || input_setup_scope_at(dir.path()),
                || panic!("Stop withholds the pending hardware read"),
                || panic!("Stop withholds the mapping read"),
                |_, _| panic!("Stop withholds activation"),
                &stopped_handoff,
                &|| {},
            )
            .unwrap();
    });
    assert!(started.get(), "the valid local Aqua startup branch ran");
}

#[test]
fn pending_first_caps_nudges_only_facts_for_its_actual_admitted_scope() {
    use aterm_cli::macos_doctor::{
        InitialSetupAdmissionFault, InitialSetupAdmissionStopTest, input_setup_scope_at,
    };
    use aterm_types::macos_setup::{Plan, Selection, Setting};

    let dir = aterm_tempfile::tempdir().unwrap();
    let mut selected = Selection::none();
    selected.set(Setting::CapsLock, true);
    let mut setup = InitialSetupAdmissionStopTest::startup(dir.path())
        .unwrap()
        .with_plan(Plan::from(selected));
    let revision = setup
        .persist(InitialSetupAdmissionFault::None)
        .unwrap()
        .unwrap();
    let scope = || input_setup_scope_at(dir.path());
    assert_eq!(scope().unwrap(), InputSetupScope::Pending(revision));
    let wakes = crate::SetupWakes::new();
    let changed = || wakes.raise_facts();
    let invalidations = std::cell::Cell::new(0);
    let invalidate = || invalidations.set(invalidations.get() + 1);
    let mut inputs = Inputs::default();
    let observe = |inputs: &mut Inputs, ids: Vec<u64>| {
        inputs.round(
            true,
            0,
            scope,
            || Ok(ids.clone()),
            || observations(&ids),
            |_, _| panic!("pending observation has no setter authority"),
            &changed,
            &invalidate,
        )
    };
    // Readability is keyed by service, not a count or an error string. Losing a
    // positive read cannot repeatedly rearm the same pending service's retry budget.
    let mut partial = Inputs::default();
    let partial_wakes = crate::SetupWakes::new();
    for (unread, expected_nudge) in [
        (&[41, 42][..], true),
        (&[41, 42][..], false),
        (&[42][..], true),
        (&[41][..], true),
        (&[][..], false),
        (&[41, 42][..], false),
        (&[41][..], false),
    ] {
        let sys = InitialSetupAdmissionStopTest::keyboard_system(true, unread);
        let before = partial_wakes.marks();
        assert!(
            !partial
                .round(
                    true,
                    0,
                    scope,
                    || sys.keyboard_service_ids(),
                    || sys.keyboard_service_observations(),
                    |_, _| panic!("readability authorizes no attachment setter"),
                    &|| partial_wakes.raise_facts(),
                    &|| {},
                )
                .unwrap()
        );
        assert_eq!(partial_wakes.marks().facts != before.facts, expected_nudge);
    }
    let before_error = partial_wakes.marks();
    assert!(
        !partial
            .round(
                true,
                0,
                scope,
                || Ok(vec![41, 42]),
                || Err("native properties unavailable".into()),
                |_, _| panic!("an unread native source authorizes no setter"),
                &|| partial_wakes.raise_facts(),
                &|| {},
            )
            .unwrap()
    );
    assert_eq!(partial_wakes.marks(), before_error);

    let before = wakes.marks();
    assert!(!observe(&mut inputs, vec![41]).unwrap());
    assert_eq!(
        invalidations.get(),
        1,
        "first native sample retires old facts"
    );
    let seeded = wakes.marks();
    assert_ne!(
        seeded.facts, before.facts,
        "the first sample closes the pass/observer gap"
    );
    assert!(!seeded.refreshed_since(before));
    assert!(!seeded.checked_since(before));
    assert!(!observe(&mut inputs, vec![41]).unwrap());
    assert_eq!(
        invalidations.get(),
        1,
        "an unchanged sample invalidates nothing"
    );
    assert_eq!(wakes.marks(), seeded, "unchanged samples owe no nudge");
    assert!(!observe(&mut inputs, vec![42, 41]).unwrap());
    assert_eq!(invalidations.get(), 2);
    let attached = wakes.marks();
    assert_ne!(attached.facts, seeded.facts);
    assert!(!attached.refreshed_since(before));
    assert!(!observe(&mut inputs, vec![41, 42]).unwrap());
    assert_eq!(
        invalidations.get(),
        2,
        "enumeration order changes no source"
    );
    assert_eq!(
        wakes.marks(),
        attached,
        "a different enumeration order is no new device"
    );
    assert!(observe(&mut inputs, vec![41, 41]).is_err());
    assert_eq!(invalidations.get(), 3, "an unread cohort retires old facts");
    assert_eq!(wakes.marks(), attached, "ambiguous IDs supply no fact");

    // A new keyboard can already be present when this observer first sees pending setup.
    let mut late = Inputs::default();
    assert!(!observe(&mut late, vec![41, 42]).unwrap());
    let first_after_attach = wakes.marks();
    assert_ne!(first_after_attach.facts, attached.facts);
    assert!(!first_after_attach.refreshed_since(before));
    assert!(!first_after_attach.checked_since(before));

    assert!(!observe(&mut inputs, vec![41, 42]).unwrap());
    let before_completion = invalidations.get();
    setup
        .native_workstation_pass(revision, || {
            Box::new(InitialSetupAdmissionStopTest::keyboard_system(true, &[]))
        })
        .unwrap()
        .unwrap();
    assert_eq!(scope().unwrap(), InputSetupScope::Applied(revision));
    let no_op = InitialSetupAdmissionStopTest::keyboard_system(true, &[]);
    assert!(
        inputs
            .round(
                true,
                0,
                scope,
                || panic!("applied scope uses one passive mapping observation"),
                || no_op.keyboard_service_observations(),
                |_, ids| aterm_machines::init::activate_keyboard_attachments(
                    &no_op,
                    ids,
                    &|_| false,
                    &|| true,
                    &invalidate,
                )
                .map(Some),
                &changed,
                &invalidate,
            )
            .unwrap()
    );
    assert_eq!(
        invalidations.get(),
        before_completion,
        "the same owned task's completion does not retire its readback"
    );
    assert!(
        no_op.keyboard_changes().is_empty(),
        "completion's real activation was a no-op"
    );

    assert!(
        !inputs
            .round(
                false,
                0,
                || panic!("another console reads no setup scope"),
                || panic!("another console reads no hardware"),
                || panic!("another console reads no mappings"),
                |_, _| panic!("another console activates nothing"),
                &changed,
                &invalidate,
            )
            .unwrap()
    );
    setup.stop_revision(revision).unwrap();
    assert_eq!(scope().unwrap(), InputSetupScope::Inactive);
    assert!(
        !inputs
            .round(
                true,
                0,
                scope,
                || panic!("Stop withdraws even pending hardware observation"),
                || panic!("Stop withdraws mapping observation"),
                |_, _| panic!("Stop withdraws activation"),
                &changed,
                &invalidate,
            )
            .unwrap()
    );
    assert_eq!(wakes.marks(), first_after_attach);
    let before = invalidations.get();
    let mut applied = Inputs::default();
    assert!(
        applied
            .round(
                true,
                0,
                || Ok(InputSetupScope::Applied(revision)),
                || Ok(vec![41]),
                || observations(&[41]),
                |_, _| {
                    assert_eq!(
                        invalidations.get(),
                        before + 1,
                        "retire before a failed activation"
                    );
                    Err("injected activation refusal".into())
                },
                &changed,
                &invalidate,
            )
            .is_err()
    );
    assert!(
        applied
            .round(
                true,
                1,
                || Err("injected source read failure".into()),
                || panic!("no hardware read after source refusal"),
                || panic!("no mapping read after source refusal"),
                |_, _| panic!("no activation after source refusal"),
                &changed,
                &invalidate,
            )
            .is_err()
    );
    assert_eq!(invalidations.get(), before + 2);
}

#[test]
fn a_console_return_reads_pending_setup_once_and_retains_only_an_unread_return() {
    use std::cell::Cell;
    let mut console = ConsoleReturn::default();
    let reads = Cell::new(0);
    let read = || {
        reads.set(reads.get() + 1);
        Ok(())
    };
    console
        .recover(true, || panic!("initial console ownership is no return"))
        .unwrap();
    console
        .recover(false, || panic!("another console reads no setup scope"))
        .unwrap();
    assert!(
        console
            .recover(true, || Err("records held".into()))
            .is_err()
    );
    console.recover(true, read).unwrap();
    console
        .recover(true, || panic!("stable ownership polls no setup records"))
        .unwrap();
    assert_eq!(reads.get(), 1);
    console
        .recover(false, || panic!("no console reads no setup scope"))
        .unwrap();
    console
        .recover(false, || panic!("still no console"))
        .unwrap();
    console.recover(true, read).unwrap();
    assert_eq!(reads.get(), 2);
    console
        .recover(true, || panic!("a handled return is spent"))
        .unwrap();
}

#[test]
fn node_start_keeps_inputs_only_inside_its_owned_startup() {
    use std::cell::RefCell;

    for owns in [false, true] {
        let events = RefCell::new(Vec::new());
        assert_eq!(
            node_serve_with(
                |startup| {
                    events.borrow_mut().push("lease");
                    if owns {
                        startup();
                    }
                    ExitCode::from(75)
                },
                || events.borrow_mut().push("keeper"),
                |()| {
                    assert_eq!(*events.borrow(), ["lease", "keeper"]);
                    events.borrow_mut().push("runtime");
                },
            ),
            ExitCode::from(75)
        );
        assert_eq!(
            events.into_inner(),
            if owns {
                vec!["lease", "keeper", "runtime"]
            } else {
                vec!["lease"]
            }
        );
    }
}

#[test]
fn background_keeper_requires_a_local_aqua_birth_but_no_terminal_or_vault() {
    use aterm_machines::port::{Fake, Stat};
    use std::cell::Cell;

    for (actor, console, graphic, remote, starts) in [
        (501, 501, Some(true), false, true),
        (0, 0, Some(true), false, false),
        (501, 502, Some(true), false, true),
        (501, 0, Some(true), false, true),
        (501, 501, Some(false), false, false),
        (501, 501, None, false, false),
        (501, 501, Some(true), true, false),
    ] {
        let mut sys = Fake::root();
        sys.euid = actor;
        sys.terminal = false;
        sys.graphic_session = graphic;
        sys.remote_login = remote;
        sys.stat_of(
            "/dev/console",
            Stat {
                uid: console,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        let started = Cell::new(false);
        if BackgroundStartup::default().take(&sys) {
            started.set(true);
        }
        assert_eq!(started.get(), starts);
        assert!(
            sys.calls().is_empty(),
            "startup needs no vault or hardware command"
        );
    }

    let mut sys = Fake::root();
    sys.euid = 501;
    sys.terminal = false;
    sys.graphic_session = None;
    let mut startup = BackgroundStartup::default();
    for _ in 0..3 {
        assert!(!startup.take(&sys), "unread session grants no startup");
    }
    assert!(
        sys.calls().is_empty(),
        "pending admission reads no hardware"
    );
    sys.graphic_session = Some(true);
    assert!(
        startup.take(&sys),
        "a retained background startup recovers its initially unread Aqua session"
    );
    assert!(
        !startup.take(&sys),
        "maintenance cannot grant a second owner"
    );
    assert!(sys.calls().is_empty());

    for initially_unread in [false, true] {
        for (uid, remote, graphic) in [
            (501, false, Some(false)),
            (0, false, None),
            (501, true, None),
        ] {
            let mut sys = Fake::root();
            sys.euid = 501;
            sys.graphic_session = None;
            let mut startup = BackgroundStartup::default();
            if initially_unread {
                assert!(!startup.take(&sys));
            }
            sys.euid = uid;
            sys.remote_login = remote;
            sys.graphic_session = graphic;
            assert!(!startup.take(&sys));
            sys.euid = 501;
            sys.remote_login = false;
            sys.graphic_session = Some(true);
            assert!(
                !startup.take(&sys),
                "a resolved non-Aqua, root or SSH barrier remains refused"
            );
            assert!(sys.calls().is_empty());
        }
    }

    // Exercise the actual retained callback too: a correct admission helper alone
    // cannot compensate for a factory that drops an unread startup at construction.
    let session = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let samples = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let maintenance = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let retained = background_maintenance(
        true,
        {
            let session = Arc::clone(&session);
            let samples = Arc::clone(&samples);
            move || {
                samples.fetch_add(1, Ordering::SeqCst);
                let mut sys = Fake::root();
                sys.euid = 501;
                sys.graphic_session = (session.load(Ordering::SeqCst) != 0).then_some(true);
                sys
            }
        },
        {
            let starts = Arc::clone(&starts);
            move || {
                starts.fetch_add(1, Ordering::SeqCst);
                Some(Supervisor::new(Notifications {
                    changed: || {},
                    invalidated: || {},
                    console_return: None,
                    pending: None,
                    ended: None,
                }))
            }
        },
        {
            let maintenance = Arc::clone(&maintenance);
            move |_| {
                maintenance.fetch_add(1, Ordering::SeqCst);
                None
            }
        },
    )
    .expect("unread admission retains the actual maintenance callback");
    for _ in 0..3 {
        assert!(
            retained().is_some_and(|at| at > Instant::now()),
            "an unread admission names when it is read again"
        );
    }
    assert_eq!(samples.load(Ordering::SeqCst), 4);
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert_eq!(maintenance.load(Ordering::SeqCst), 0);
    session.store(1, Ordering::SeqCst);
    retained();
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(maintenance.load(Ordering::SeqCst), 0);
    let admitted_samples = samples.load(Ordering::SeqCst);
    for _ in 0..3 {
        retained();
    }
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(maintenance.load(Ordering::SeqCst), 3);
    assert_eq!(samples.load(Ordering::SeqCst), admitted_samples);

    assert!(
        background_maintenance::<Fake>(
            false,
            || panic!("private-state or CI refusal precedes session reads"),
            || panic!("private-state or CI refusal precedes the owner"),
            |_| panic!("a refused factory has no maintenance"),
        )
        .is_none()
    );
    for refusal in [0, 1, 2] {
        let session = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let samples = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let retained = background_maintenance(
            true,
            {
                let session = Arc::clone(&session);
                let samples = Arc::clone(&samples);
                move || {
                    samples.fetch_add(1, Ordering::SeqCst);
                    let mut sys = Fake::root();
                    sys.euid = 501;
                    sys.graphic_session = None;
                    match session.load(Ordering::SeqCst) {
                        0 => {}
                        1 if refusal == 0 => sys.graphic_session = Some(false),
                        1 if refusal == 1 => sys.euid = 0,
                        1 => sys.remote_login = true,
                        _ => sys.graphic_session = Some(true),
                    }
                    sys
                }
            },
            || panic!("a resolved session refusal cannot create an owner"),
            |_| panic!("an unread or refused session has no sampler"),
        )
        .unwrap();
        retained();
        session.store(1, Ordering::SeqCst);
        retained();
        let refused_samples = samples.load(Ordering::SeqCst);
        session.store(2, Ordering::SeqCst);
        assert_eq!(
            retained(),
            None,
            "CONTROL: a refused session names no deadline"
        );
        assert_eq!(samples.load(Ordering::SeqCst), refused_samples);
    }
}

#[test]
fn an_aqua_keeper_parks_without_hardware_reads_and_resumes_after_user_switching() {
    use aterm_machines::port::{Fake, Stat};
    use std::cell::{Cell, RefCell};

    let mut sys = Fake::root();
    sys.euid = 501;
    sys.terminal = false;
    let console = |uid| {
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
    };
    // launchd can start this user's Aqua job while another login owns the console.
    console(502);
    let started = Cell::new(false);
    if BackgroundStartup::default().take(&sys) {
        started.set(true);
    }
    assert!(started.get());

    let mut keeper = Keeper::default();
    let reads = Cell::new((0, 0));
    let activated = RefCell::new(Vec::new());
    for (owner, ids, active) in [
        (502, vec![10], false),
        (501, vec![11], true),
        (502, vec![12], false),
        (501, vec![12], true),
    ] {
        console(owner);
        let before = reads.get();
        let applied = activated.borrow().len();
        assert_eq!(
            keeper
                .round_at_console(
                    &sys,
                    0,
                    || {
                        reads.set((reads.get().0 + 1, reads.get().1));
                        Ok(Some(7))
                    },
                    || {
                        reads.set((reads.get().0, reads.get().1 + 1));
                        observations(&ids)
                    },
                    |revision, new| {
                        assert_eq!(revision, 7);
                        activated.borrow_mut().push(new.to_vec());
                        verified(new)
                    },
                )
                .unwrap(),
            active
        );
        if !active {
            assert_eq!(
                reads.get(),
                before,
                "no setup or hardware reading while away"
            );
            assert_eq!(activated.borrow().len(), applied);
        }
    }
    assert_eq!(reads.get(), (2, 2));
    assert_eq!(*activated.borrow(), [vec![11], vec![12]]);
    assert!(sys.calls().is_empty());

    // Returning to the console cannot borrow a held or withdrawn Caps choice.
    assert!(
        !keeper
            .round_at_console(
                &sys,
                0,
                || Ok(None),
                || panic!("held Caps choice reads no hardware"),
                |_, _| panic!("held Caps choice activates nothing"),
            )
            .unwrap()
    );
}

#[test]
fn attach_and_reattach_activate_only_new_services_and_off_reads_no_hardware() {
    let mut keeper = Keeper::default();
    for (ids, expected) in [
        (vec![1], vec![1]),
        (vec![1, 2], vec![2]),
        (vec![1, 3], vec![3]),
    ] {
        assert!(
            keeper
                .round(
                    0,
                    Some(7),
                    || observations(&ids),
                    |revision, new| {
                        assert_eq!(revision, 7);
                        assert_eq!(new, expected);
                        verified(new)
                    }
                )
                .unwrap()
        );
    }
    assert!(
        !keeper
            .round(
                0,
                Some(7),
                || observations(&[1]),
                |_, _| panic!("removal only")
            )
            .unwrap()
    );
    assert!(
        !keeper
            .round(
                0,
                Some(7),
                || observations(&[1]),
                |_, _| panic!("unchanged")
            )
            .unwrap()
    );
    assert!(
        !keeper
            .round(
                0,
                None,
                || panic!("Off reads no HID state"),
                |_, _| panic!("Off")
            )
            .unwrap()
    );
    assert!(
        keeper
            .round(
                0,
                Some(8),
                || observations(&[1]),
                |revision, ids| {
                    assert_eq!((revision, ids), (8, &[1][..]));
                    verified(ids)
                }
            )
            .unwrap()
    );
}

#[test]
fn failed_or_held_attachments_retry_without_a_hot_loop_and_new_devices_wake_now() {
    let mut keeper = Keeper::default();
    assert!(
        !keeper
            .round(0, Some(7), || observations(&[1]), |_, _| Ok(None))
            .unwrap()
    );
    assert!(
        !keeper
            .round(
                4999,
                Some(7),
                || observations(&[1]),
                |_, _| panic!("lock retry paced")
            )
            .unwrap()
    );
    assert!(
        !keeper
            .round(
                5000,
                Some(7),
                || observations(&[1]),
                |_, _| Ok(Some(KeyboardAttachmentActivation::Unfinished))
            )
            .unwrap()
    );
    assert!(
        !keeper
            .round(
                9999,
                Some(7),
                || observations(&[1]),
                |_, _| panic!("unfinished retry paced")
            )
            .unwrap()
    );
    assert!(
        keeper
            .round(
                10000,
                Some(7),
                || observations(&[1]),
                |_, ids| verified(ids)
            )
            .unwrap()
    );
    assert!(
        !keeper
            .round(
                10001,
                Some(7),
                || observations(&[1]),
                |_, _| panic!("success stops")
            )
            .unwrap()
    );
    assert!(
        keeper
            .round(
                10001,
                Some(7),
                || observations(&[1, 2]),
                |_, ids| {
                    assert_eq!(ids, [2]);
                    verified(ids)
                }
            )
            .unwrap()
    );
    assert!(
        keeper
            .round(
                10002,
                Some(7),
                || observations(&[1, 2, 2]),
                |_, _| panic!("duplicate identity")
            )
            .is_err()
    );

    // A retained native wake runs the SAME full census/receipt path, rather than
    // treating a notification or its registry identity as proof of a mapping.
    use std::sync::mpsc;

    let wake = Arc::<watch::Control>::default();
    let stopped = Arc::new(AtomicBool::new(false));
    let services = Arc::new(Mutex::new(vec![1]));
    let (sampled, samples) = mpsc::channel();
    let (at_wait, waiting) = mpsc::sync_channel(1);
    let (proceed, proceed_rx) = mpsc::sync_channel(1);
    let worker_wake = Arc::clone(&wake);
    let worker_stop = Arc::clone(&stopped);
    let worker_services = Arc::clone(&services);
    let worker = std::thread::spawn(move || {
        let mut at_wait = Some(at_wait);
        let mut watch = watch::Watch::private(worker_wake, &worker_stop, move || {
            if let Some(at_wait) = at_wait.take() {
                at_wait.send(()).unwrap();
                proceed_rx.recv_timeout(Duration::from_secs(60)).unwrap();
            }
        });
        let mut keeper = Keeper::default();
        let mut reads = 0;
        let mut applied = Vec::new();
        let first = keeper.round(
            0,
            Some(7),
            || {
                reads += 1;
                observations(&worker_services.lock().unwrap())
            },
            |_, ids| {
                applied.push(ids.to_vec());
                verified(ids)
            },
        );
        sampled.send((None, first, reads, applied.clone())).unwrap();
        let hinted = watch.wait(&worker_stop, Instant::now() + Duration::from_secs(60));
        let second = keeper.round(
            1,
            Some(7),
            || {
                reads += 1;
                observations(&worker_services.lock().unwrap())
            },
            |_, ids| {
                applied.push(ids.to_vec());
                verified(ids)
            },
        );
        sampled
            .send((Some(hinted), second, reads, applied))
            .unwrap();
    });
    let first = samples.recv_timeout(Duration::from_secs(60));
    let entered = waiting.recv_timeout(Duration::from_secs(60));
    *services.lock().unwrap() = vec![1, 2];
    // This is after every predicate check and before native wait entry: the real
    // source must retain the signal, even when it arrives before the kernel waits.
    wake.wake();
    let released = proceed.send(());
    let second = samples.recv_timeout(Duration::from_secs(60));
    stopped.store(true, Ordering::Release);
    wake.wake();
    let ended = worker.join();
    // Release and join the only worker before any verdict, including a failed wake.
    assert!(ended.is_ok(), "{ended:?}");
    assert!(entered.is_ok(), "{entered:?}");
    assert!(released.is_ok(), "{released:?}");
    assert_eq!(first.unwrap(), (None, Ok(true), 1, vec![vec![1]]));
    assert_eq!(
        second.unwrap(),
        (
            Some(watch::Woke::Hint(watch::Causes::WAKE)),
            Ok(true),
            2,
            vec![vec![1], vec![2]]
        ),
        "native wake, then fresh complete census; the unchanged fallback is not a hint"
    );

    // Setup/wait gaps retain hints and stop; a consumed hint does not create a
    // repeating wake. These use the native source without live device subscription.
    let early = Arc::<watch::Control>::default();
    early.wake();
    early.wake();
    let native = Arc::clone(&early);
    let before_setup = std::thread::spawn(move || {
        let stopped = AtomicBool::new(false);
        let mut watch = watch::Watch::private(native, &stopped, || {
            panic!("a queued hint needs no blocking wait")
        });
        assert_eq!(
            watch.wait(&stopped, Instant::now() + Duration::from_secs(60)),
            watch::Woke::Hint(watch::Causes::WAKE)
        );
        assert_eq!(watch.wait(&stopped, Instant::now()), watch::Woke::Deadline);
    });
    assert!(before_setup.join().is_ok());
    // A late wake after teardown only latches state, never reaches freed CF handles.
    early.wake();
    let native = Arc::clone(&early);
    let before_setup = std::thread::spawn(move || {
        let stopped = AtomicBool::new(true);
        let mut watch = watch::Watch::private(native, &stopped, || {
            panic!("prelatched stop needs no blocking wait")
        });
        assert_eq!(
            watch.wait(&stopped, Instant::now() + Duration::from_secs(60)),
            watch::Woke::Stopped
        );
    });
    assert!(before_setup.join().is_ok());
}

/// What a worker on a private watch tells its test: that it is about to block with nothing
/// pending, or how a wait returned.
#[derive(Debug, PartialEq, Eq)]
enum Waited {
    Blocked,
    Returned(watch::Woke),
}

/// K1, on the wait a settled keeper blocks in, one with a deadline far off (here the
/// backstop's): three hints end three waits, a wait with nothing to tell blocks, and the fourth
/// blocks until stop.
/// NEGATIVE CONTROL: with a deadline still ahead read as passed, the first wait returns
/// `Deadline` before the worker blocks, and the worker ends there.
#[test]
fn a_wait_returns_once_for_each_hint_and_for_nothing_else_before_its_deadline() {
    use std::sync::mpsc;

    let control = Arc::<watch::Control>::default();
    let stopped = Arc::new(AtomicBool::new(false));
    let (tell, told) = mpsc::channel();
    let worker_control = Arc::clone(&control);
    let worker_stop = Arc::clone(&stopped);
    let worker = std::thread::spawn(move || {
        let blocked = tell.clone();
        let mut watch = watch::Watch::private(worker_control, &worker_stop, move || {
            let _ = blocked.send(Waited::Blocked);
        });
        let far = Instant::now() + BACKSTOP;
        loop {
            let woke = watch.wait(&worker_stop, far);
            let _ = tell.send(Waited::Returned(woke));
            if !matches!(woke, watch::Woke::Hint(_)) {
                return;
            }
        }
    });
    let next = || told.recv_timeout(Duration::from_secs(60));
    let mut seen = Vec::new();
    for _ in 0..3 {
        // The worker says it blocks before each hint: a wait with nothing to tell returns nothing.
        seen.push(next());
        control.hint(watch::Causes::RECORDS);
        seen.push(next());
    }
    seen.push(next());
    stopped.store(true, Ordering::Release);
    control.wake();
    seen.push(next());
    let ended = worker.join();
    // Release and join the only worker before any verdict.
    assert!(ended.is_ok(), "{ended:?}");
    let hinted = || Ok(Waited::Returned(watch::Woke::Hint(watch::Causes::RECORDS)));
    assert_eq!(
        seen,
        [
            Ok(Waited::Blocked),
            hinted(),
            Ok(Waited::Blocked),
            hinted(),
            Ok(Waited::Blocked),
            hinted(),
            Ok(Waited::Blocked),
            Ok(Waited::Returned(watch::Woke::Stopped)),
        ],
        "three hints end three waits, and the fourth wait blocks until stop"
    );
    assert!(
        told.try_recv().is_err(),
        "nothing blocked or returned after the stop"
    );
}

/// A scripted clock for [`keeper_loop`]: each hint arrives at its instant, a wait ends at the
/// first of a hint, its deadline and the script's end, and the clock moves only in a wait.
struct ScriptedWaits {
    start: Instant,
    now: std::rc::Rc<std::cell::Cell<Duration>>,
    hints: std::collections::VecDeque<(Duration, watch::Causes)>,
    until: Duration,
    armed: usize,
}

impl ScriptedWaits {
    fn new(hints: &[(u64, watch::Causes)], until_ms: u64) -> Self {
        Self {
            start: Instant::now(),
            now: std::rc::Rc::default(),
            hints: hints
                .iter()
                .map(|&(at, cause)| (Duration::from_millis(at), cause))
                .collect(),
            until: Duration::from_millis(until_ms),
            armed: 0,
        }
    }
}

impl Waits for ScriptedWaits {
    fn now(&self) -> Instant {
        self.start + self.now.get()
    }

    fn armed(&self) -> watch::Armed {
        watch::Armed {
            devices: true,
            console: true,
        }
    }

    fn wait(&mut self, stopped: &AtomicBool, deadline: Instant) -> watch::Woke {
        self.armed += 1;
        let now = self.now.get();
        let deadline = deadline.saturating_duration_since(self.start);
        // As the real watch tells them: a passed deadline before a hint that is also there.
        if now >= deadline {
            return watch::Woke::Deadline;
        }
        let hint = self.hints.front().copied();
        let next = hint.map_or(deadline, |(at, _)| at.min(deadline));
        if next.max(now) > self.until {
            self.now.set(self.until);
            stopped.store(true, Ordering::Release);
            return watch::Woke::Stopped;
        }
        match hint {
            Some((at, cause)) if at < deadline => {
                self.hints.pop_front();
                self.now.set(now.max(at));
                watch::Woke::Hint(cause)
            }
            _ => {
                self.now.set(deadline);
                watch::Woke::Deadline
            }
        }
    }
}

/// What a round that owes nothing finds, after a wait that heard `causes`.
fn settled(causes: watch::Causes) -> RoundFacts {
    RoundFacts {
        causes,
        moved: false,
        pending_scope: false,
        attempted: false,
        console_return: false,
        follow_up: crate::withheld_preferences::Owed::Idle,
        caps_stand: true,
        dispatch: None,
        undelivered: false,
    }
}

/// Every hint source armed.
const WHOLE: Coverage = Coverage {
    devices: true,
    console: true,
    records: true,
};

/// The instant each round of [`keeper_loop`] started on a scripted clock, in milliseconds, and
/// the causes it was handed; then the waits the loop armed. `found` is what the round of that
/// index finds; every hint source is armed.
fn scripted_rounds(
    hints: &[(u64, watch::Causes)],
    until_ms: u64,
    found: impl Fn(usize, watch::Causes) -> RoundFacts,
) -> (Vec<(u128, watch::Causes)>, usize) {
    let mut waits = ScriptedWaits::new(hints, until_ms);
    let clock = std::rc::Rc::clone(&waits.now);
    let mut rounds = Vec::new();
    keeper_loop(
        |causes| {
            rounds.push((clock.get().as_millis(), causes));
            found(rounds.len() - 1, causes)
        },
        |_, _, _| {},
        &mut waits,
        &AtomicBool::new(false),
        true,
        PACE,
    );
    (rounds, waits.armed)
}

/// A keeper's first round finds a new sample, and every round after it nothing owed.
fn fresh_then_settled(round: usize, causes: watch::Causes) -> RoundFacts {
    RoundFacts {
        moved: round == 0,
        ..settled(causes)
    }
}

/// While a round owes another (here an unfinished attachment), the timer and the pacing are
/// what they were: a round every two seconds, at once for a device notice or a wake, and every
/// other hint handed to the round the pace starts.
/// NEGATIVE CONTROL: with a records hint ending the wait as a device notice does, a round
/// starts at 500 ms carrying `records` alone.
#[test]
fn only_a_device_notice_or_a_wake_starts_a_round_before_the_pace() {
    use watch::Causes;
    let none = Causes::default();
    let owed = |_, causes| RoundFacts {
        attempted: true,
        ..settled(causes)
    };
    let (rounds, armed) = scripted_rounds(
        &[
            (500, Causes::RECORDS),
            (1_000, Causes::FOCUS),
            (1_500, Causes::REQUEST),
            (3_000, Causes::CONSOLE),
            (4_700, Causes::DEVICE),
            (9_000, Causes::WAKE),
        ],
        12_000,
        owed,
    );
    assert_eq!(
        rounds,
        [
            (0, none),
            (
                2_000,
                Causes::RECORDS.union(Causes::FOCUS).union(Causes::REQUEST)
            ),
            (4_000, Causes::CONSOLE),
            (4_700, Causes::DEVICE),
            (6_700, none),
            (8_700, none),
            (9_000, Causes::WAKE),
            (11_000, none),
        ],
        "a round every two seconds, and at once for the device notice and the wake"
    );
    // One wait a round, one more for each hint that did not end its wait, and the last, which
    // the script's end stopped.
    assert_eq!(armed, 8 + 4);

    // No hint in the hour that ends just before its last pace would: a round at each pace
    // and nothing else.
    let (rounds, armed) = scripted_rounds(&[], 3_599_999, owed);
    assert_eq!(rounds.len(), 1_800);
    assert_eq!(armed, 1_800);
    assert!(rounds.iter().all(|(_, causes)| causes.is_empty()));
}

/// K3, the wait a round's facts choose: the backstop for a settled round with every hint source
/// armed and no notice to confirm, and the two-second timer for each thing a round can owe and
/// while a notice is confirmed.
/// NEGATIVE CONTROL: without the `attempted` test, the unfinished attachment's row waits 300 s.
#[test]
fn a_settled_round_waits_for_the_backstop_and_an_owed_round_keeps_the_timer() {
    use crate::withheld_preferences::Owed;
    use watch::Causes;
    let none = Causes::default();
    let secs = Duration::from_secs;
    assert_eq!(BACKSTOP, secs(300));
    let waits = |facts: RoundFacts| next_wait(&facts, false, WHOLE, PACE);
    let rows = [
        ("settled", settled(none), secs(300)),
        ("a records hint", settled(Causes::RECORDS), secs(300)),
        ("a focus hint", settled(Causes::FOCUS), secs(300)),
        ("a request hint", settled(Causes::REQUEST), secs(300)),
        ("a wake", settled(Causes::WAKE), secs(300)),
        (
            "moved",
            RoundFacts {
                moved: true,
                ..settled(none)
            },
            PACE,
        ),
        (
            "a pending scope",
            RoundFacts {
                pending_scope: true,
                ..settled(none)
            },
            PACE,
        ),
        (
            "an unfinished attachment",
            RoundFacts {
                attempted: true,
                ..settled(none)
            },
            PACE,
        ),
        (
            "a kept console return",
            RoundFacts {
                console_return: true,
                ..settled(none)
            },
            PACE,
        ),
        (
            "a follow-up attempt running",
            RoundFacts {
                follow_up: Owed::Running,
                ..settled(none)
            },
            PACE,
        ),
        (
            "a Caps Lock live reading that does not stand",
            RoundFacts {
                caps_stand: false,
                ..settled(none)
            },
            PACE,
        ),
        (
            "readings the window's loop has not taken",
            RoundFacts {
                undelivered: true,
                ..settled(none)
            },
            PACE,
        ),
        (
            "a follow-up put off 40 s",
            RoundFacts {
                follow_up: Owed::In(secs(40)),
                ..settled(none)
            },
            secs(40),
        ),
        (
            "a follow-up a round has not seen",
            RoundFacts {
                follow_up: Owed::In(Duration::ZERO),
                ..settled(none)
            },
            PACE,
        ),
        (
            "a follow-up put off past the backstop",
            RoundFacts {
                follow_up: Owed::In(secs(600)),
                ..settled(none)
            },
            secs(300),
        ),
        (
            "a reader held 1 s",
            RoundFacts {
                dispatch: Some(secs(1)),
                ..settled(none)
            },
            PACE,
        ),
        (
            "a reader held 30 s",
            RoundFacts {
                dispatch: Some(secs(30)),
                follow_up: Owed::In(secs(40)),
                ..settled(none)
            },
            secs(30),
        ),
    ];
    for (name, facts, wait) in rows {
        assert_eq!(waits(facts), wait, "{name}");
    }
    assert_eq!(
        next_wait(&settled(none), true, WHOLE, PACE),
        PACE,
        "a notice still confirmed"
    );
    for (name, coverage) in [
        (
            "no device notices",
            Coverage {
                devices: false,
                ..WHOLE
            },
        ),
        (
            "no console key",
            Coverage {
                console: false,
                ..WHOLE
            },
        ),
        (
            "no records watch",
            Coverage {
                records: false,
                ..WHOLE
            },
        ),
    ] {
        assert_eq!(
            next_wait(&settled(none), false, coverage, PACE),
            PACE,
            "{name}"
        );
    }
}

/// K3, round counts on a scripted clock: a settled keeper with every source armed runs a round
/// for each hint, five more at the pace after a keyboard or console notice ([`CONFIRMING`]), one
/// at each backstop, and none else.
/// NEGATIVE CONTROLS: with the wait after every round the pace, the hour with no hint runs 1800
/// rounds; with [`CONFIRMING`] 1, each notice runs one confirming round, and its count reads 12.
#[test]
fn a_settled_keeper_runs_a_round_for_a_hint_or_the_backstop_and_none_else() {
    use watch::Causes;
    let none = Causes::default();
    // The five rounds that confirm a notice whose round started at `from`, each at the pace.
    let confirming = |from: u128| (1..=5).map(move |nth| (from + nth * 2_000, none));

    // An hour with no hint: the first round, the one its new sample owes, and the backstop's.
    let (rounds, armed) = scripted_rounds(&[], 3_599_999, fresh_then_settled);
    let backstops = (1..=11).map(|nth| (2_000 + nth * 300_000, none));
    assert_eq!(
        rounds,
        [(0, none), (2_000, none)]
            .into_iter()
            .chain(backstops)
            .collect::<Vec<_>>(),
        "thirteen rounds in an hour: two, then one every five minutes"
    );
    assert_eq!(armed, 13, "one wait a round");

    // Five hints of each kind, fifteen seconds apart, in a hundred seconds: no backstop falls in
    // them, and a notice's confirming rounds end before the next hint.
    let count = |cause| {
        let hints: Vec<_> = (1..=5).map(|nth| (nth * 15_000, cause)).collect();
        scripted_rounds(&hints, 100_000, fresh_then_settled).0.len()
    };
    assert_eq!(scripted_rounds(&[], 100_000, fresh_then_settled).0.len(), 2);
    assert_eq!(
        count(Causes::DEVICE),
        2 + 6 * 5,
        "a round and its five confirming rounds"
    );
    assert_eq!(
        count(Causes::CONSOLE),
        2 + 6 * 5,
        "a round and its five confirming rounds"
    );
    assert_eq!(count(Causes::RECORDS), 2 + 5);
    assert_eq!(count(Causes::FOCUS), 2 + 5);
    assert_eq!(count(Causes::REQUEST), 2 + 5);
    assert_eq!(count(Causes::WAKE), 2 + 5);

    // When each round starts: at the hint, but no sooner than the pace after the round before,
    // a keyboard notice and a wake apart; and a notice's confirming rounds at the pace after it.
    let (rounds, _) = scripted_rounds(
        &[
            (10_000, Causes::DEVICE),
            (30_000, Causes::RECORDS),
            (30_500, Causes::FOCUS),
            (31_000, Causes::REQUEST),
            (31_500, Causes::RECORDS),
            (40_000, Causes::CONSOLE),
            (60_000, Causes::WAKE),
            (60_100, Causes::DEVICE),
        ],
        80_000,
        fresh_then_settled,
    );
    let expected: Vec<_> = [(0, none), (2_000, none), (10_000, Causes::DEVICE)]
        .into_iter()
        .chain(confirming(10_000))
        .chain([
            (30_000, Causes::RECORDS),
            (
                32_000,
                Causes::FOCUS.union(Causes::REQUEST).union(Causes::RECORDS),
            ),
            (40_000, Causes::CONSOLE),
        ])
        .chain(confirming(40_000))
        .chain([(60_000, Causes::WAKE), (60_100, Causes::DEVICE)])
        .chain(confirming(60_100))
        .collect();
    assert_eq!(rounds, expected);
}

/// What a keeper's worker tells its test: each round's causes and the wait chosen after it, and
/// how each wait returned.
#[derive(Debug, PartialEq, Eq)]
enum Kept {
    Round(watch::Causes, Duration),
    Waited(watch::Woke),
}

/// The real watch, telling each wait's return. Where the test asked for it since the wait before
/// returned, the console's callout is called on the worker's thread as this wait returns, so
/// the next wait tells it.
struct TappedWatch {
    watch: watch::Watch,
    tell: std::sync::mpsc::Sender<Kept>,
    console: Arc<AtomicBool>,
    /// Stand for a Mac whose keyboard notices and console key armed, which a private watch
    /// registers none of.
    covered: bool,
}

impl Waits for TappedWatch {
    fn now(&self) -> Instant {
        self.watch.now()
    }

    fn armed(&self) -> watch::Armed {
        if self.covered {
            watch::Armed {
                devices: true,
                console: true,
            }
        } else {
            self.watch.armed()
        }
    }

    fn wait(&mut self, stopped: &AtomicBool, deadline: Instant) -> watch::Woke {
        let woke = Waits::wait(&mut self.watch, stopped, deadline);
        if self.console.swap(false, Ordering::AcqRel) {
            self.watch.console_notice();
        }
        let _ = self.tell.send(Kept::Waited(woke));
        woke
    }
}

/// A background keeper's worker on a private watch with `records` as its records folder: the
/// function the keeper's thread runs, over a system that reads no hardware, off the console
/// unless [`KeptWorker::start_as`] is handed another. It tells each round and each wait's return.
struct KeptWorker {
    control: Arc<watch::Control>,
    stopped: Arc<AtomicBool>,
    console: Arc<AtomicBool>,
    told: std::sync::mpsc::Receiver<Kept>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl KeptWorker {
    fn start(records: Option<std::path::PathBuf>, pace: Duration, covered: bool) -> Self {
        Self::start_as(records, pace, covered, || {
            (
                aterm_machines::port::Fake::root(),
                InputSetupScope::Inactive,
            )
        })
    }

    /// [`Self::start`] over the system and under the setup scope `world` makes on the worker's
    /// thread.
    fn start_as(
        records: Option<std::path::PathBuf>,
        pace: Duration,
        covered: bool,
        world: fn() -> (aterm_machines::port::Fake, InputSetupScope),
    ) -> Self {
        let control = Arc::<watch::Control>::default();
        let stopped = Arc::new(AtomicBool::new(false));
        let console = Arc::new(AtomicBool::new(false));
        let (tell, told) = std::sync::mpsc::channel();
        let worker_control = Arc::clone(&control);
        let worker_stop = Arc::clone(&stopped);
        let worker_console = Arc::clone(&console);
        let worker = std::thread::spawn(move || {
            let (sys, scope) = world();
            let rounds = tell.clone();
            let mut watch = TappedWatch {
                watch: watch::Watch::private(Arc::clone(&worker_control), &worker_stop, || {}),
                tell,
                console: worker_console,
                covered,
            };
            keeper_worker(
                &Mutex::default(),
                &worker_stop,
                Instant::now(),
                &Notifications {
                    changed: || {},
                    invalidated: || {},
                    console_return: None,
                    pending: None,
                    ended: None,
                },
                &worker_control,
                &mut watch,
                &Ports {
                    sys: &sys,
                    scope: &|| Ok(scope),
                    activate: &|_, _, _| panic!("a keeper with no applied scope activates nothing"),
                    records,
                    chose: &|facts, _, wait| {
                        let _ = rounds.send(Kept::Round(facts.causes, wait));
                    },
                    pace,
                },
            );
        });
        Self {
            control,
            stopped,
            console,
            told,
            worker: Some(worker),
        }
    }

    /// What the worker tells next: a wait for something that must happen.
    fn next(&self) -> Result<Kept, std::sync::mpsc::RecvTimeoutError> {
        self.told.recv_timeout(Duration::from_secs(60))
    }

    /// Stop and join the worker, and hand back everything it told after the last `next`.
    fn end(mut self) -> Vec<Kept> {
        self.stopped.store(true, Ordering::Release);
        self.control.wake();
        let ended = self.worker.take().expect("one worker").join();
        assert!(ended.is_ok(), "{ended:?}");
        self.told.try_iter().collect()
    }
}

/// On the function the keeper's thread runs: each hint source reaches the worker's wait, a
/// records, focus, request or console hint starts no round while the pace runs, and the round a
/// device notice starts is handed all of them. The records hints come from the worker's own
/// watch of its records folder: its first look, then a record renamed into the folder.
/// NEGATIVE CONTROL: a worker that starts no records watch tells no `Hint(records)`; the wait
/// for it runs into the one-minute hang detector.
#[test]
fn a_worker_hears_each_hint_and_runs_no_round_for_it_before_the_pace() {
    use watch::{Causes, Woke};
    let dir = aterm_tempfile::tempdir().unwrap();
    let folder = dir.path().join("doctor");
    std::fs::create_dir(&folder).unwrap();
    let staged = dir.path().join("first-run.json.new");
    std::fs::write(&staged, b"{}").unwrap();
    // An hour's pace: no round but the first starts unless a device notice starts it.
    let hour = Duration::from_secs(3_600);
    let worker = KeptWorker::start(Some(folder.clone()), hour, true);
    let mut told = Vec::new();
    // The first round, then the records watch's first look.
    told.push(worker.next());
    told.push(worker.next());
    // The console's callout runs on the worker's thread as the focus hint's wait returns.
    worker.console.store(true, Ordering::Release);
    worker.control.hint(Causes::FOCUS);
    told.push(worker.next());
    told.push(worker.next());
    worker.control.hint(Causes::REQUEST);
    told.push(worker.next());
    std::fs::rename(&staged, folder.join("first-run.json")).unwrap();
    told.push(worker.next());
    worker.control.hint(Causes::DEVICE);
    told.push(worker.next());
    told.push(worker.next());
    let after = worker.end();
    let heard = |cause| Ok(Kept::Waited(Woke::Hint(cause)));
    assert_eq!(
        told,
        [
            Ok(Kept::Round(Causes::default(), hour)),
            heard(Causes::RECORDS),
            heard(Causes::FOCUS),
            heard(Causes::CONSOLE),
            heard(Causes::REQUEST),
            heard(Causes::RECORDS),
            heard(Causes::DEVICE),
            Ok(Kept::Round(
                Causes::RECORDS
                    .union(Causes::FOCUS)
                    .union(Causes::CONSOLE)
                    .union(Causes::REQUEST)
                    .union(Causes::DEVICE),
                hour
            )),
        ],
        "six hints, one round, and that round the device notice's"
    );
    assert_eq!(after, [Kept::Waited(Woke::Stopped)], "nothing but the stop");
}

/// K3, on the function the keeper's thread runs, with no pace so that only a settled wait
/// blocks: a fresh worker runs its first round, the one its new sample owes, and one for its
/// records watch's first look, then blocks. Each hint then runs one round, and a keyboard or
/// console notice five more that confirm it, and nothing else runs: 49 rounds, 3 of them the
/// fresh worker's, 30 for five keyboard notices, 9 for three focus, three request and three
/// records hints, and 7 for a focus hint and a console notice.
/// NEGATIVE CONTROL: with the wait after every round the pace, the second round's wait returns
/// `Deadline` where the settled wait blocks for `Hint(records)`, and rounds follow with no
/// hint.
#[test]
fn a_settled_worker_blocks_until_a_hint_and_runs_one_round_for_it() {
    use watch::{Causes, Woke};
    let none = Causes::default();
    let dir = aterm_tempfile::tempdir().unwrap();
    let folder = dir.path().join("doctor");
    std::fs::create_dir(&folder).unwrap();
    let worker = KeptWorker::start(Some(folder.clone()), Duration::ZERO, true);
    let mut told = Vec::new();
    let mut expected = Vec::new();
    let now = Duration::ZERO;
    let mut expect = |told: &mut Vec<_>, events: &[Kept]| {
        for event in events {
            told.push(worker.next());
            expected.push(Ok(match event {
                Kept::Round(causes, wait) => Kept::Round(*causes, *wait),
                Kept::Waited(woke) => Kept::Waited(*woke),
            }));
        }
    };
    // One round for a hint that needs no confirming: the hint ends the settled wait, the pace
    // (none here) has passed, and the round settles again.
    let one_round = |cause| {
        [
            Kept::Waited(Woke::Hint(cause)),
            Kept::Waited(Woke::Deadline),
            Kept::Round(cause, BACKSTOP),
        ]
    };
    expect(
        &mut told,
        &[
            Kept::Round(none, now),
            Kept::Waited(Woke::Deadline),
            Kept::Round(none, BACKSTOP),
        ],
    );
    // The five rounds that confirm a notice: each at the pace (none here), and the fifth
    // settles again.
    let confirming: Vec<_> = (1..=5)
        .flat_map(|nth| {
            [
                Kept::Waited(Woke::Deadline),
                Kept::Round(none, if nth < 5 { now } else { BACKSTOP }),
            ]
        })
        .collect();
    // The records watch's first look.
    expect(&mut told, &one_round(Causes::RECORDS));
    for _ in 0..5 {
        worker.control.hint(Causes::DEVICE);
        expect(
            &mut told,
            &[
                Kept::Waited(Woke::Hint(Causes::DEVICE)),
                Kept::Round(Causes::DEVICE, now),
            ],
        );
        expect(&mut told, &confirming);
    }
    for nth in 0..3 {
        worker.control.hint(Causes::FOCUS);
        expect(&mut told, &one_round(Causes::FOCUS));
        worker.control.hint(Causes::REQUEST);
        expect(&mut told, &one_round(Causes::REQUEST));
        let staged = dir.path().join("record.new");
        std::fs::write(&staged, b"{}").unwrap();
        std::fs::rename(&staged, folder.join(format!("record-{nth}.json"))).unwrap();
        expect(&mut told, &one_round(Causes::RECORDS));
    }
    // The console's callout runs on the worker's thread as the focus hint's wait returns.
    worker.console.store(true, Ordering::Release);
    worker.control.hint(Causes::FOCUS);
    expect(&mut told, &one_round(Causes::FOCUS));
    expect(
        &mut told,
        &[
            Kept::Waited(Woke::Hint(Causes::CONSOLE)),
            Kept::Waited(Woke::Deadline),
            Kept::Round(Causes::CONSOLE, now),
        ],
    );
    expect(&mut told, &confirming);
    let after = worker.end();
    assert_eq!(told, expected);
    let rounds = told
        .iter()
        .filter(|event| matches!(event, Ok(Kept::Round(..))))
        .count();
    assert_eq!(rounds, 3 + 6 * 5 + 3 * 3 + 1 + 6);
    assert_eq!(after, [Kept::Waited(Woke::Stopped)], "nothing but the stop");
}

/// K3, on the function the keeper's thread runs: a worker whose keyboard notices and console
/// key did not arm (a private watch registers neither, and says so) keeps its timer after
/// every round, its records watch armed or not.
/// NEGATIVE CONTROL: with `Watch::armed` saying both armed, the second round settles and the
/// worker blocks: the third round is the records watch's first look, after `Hint(records)`.
#[test]
fn a_worker_whose_hint_sources_did_not_arm_keeps_its_timer() {
    use watch::{Causes, Woke};
    let dir = aterm_tempfile::tempdir().unwrap();
    let worker = KeptWorker::start(Some(dir.path().to_owned()), Duration::ZERO, false);
    let told: Vec<_> = (0..8).map(|_| worker.next()).collect();
    let _ = worker.end();
    let round = || Ok(Kept::Round(Causes::default(), Duration::ZERO));
    let paced = || Ok(Kept::Waited(Woke::Deadline));
    assert_eq!(
        told,
        [
            round(),
            paced(),
            round(),
            paced(),
            round(),
            paced(),
            round(),
            paced()
        ],
        "round after round at the pace"
    );
}

/// K3, on what the keeper's thread runs ([`live_worker`]: the live watch, the records watch
/// and the two-second pace), over a system that reads no hardware: every round finds all
/// three hint sources armed, the first round's wait is the pace, and a round after it settles
/// on the backstop. A wake starts each later round, so the law waits out no pace; a keyboard
/// this Mac attaches meanwhile only adds the paced rounds that confirm its notice.
/// NEGATIVE CONTROLS: with the console registration deleted from the live watch's setup the
/// first round's coverage says `console: false`; with `live_worker` handing its worker no
/// records folder it says `records: false`.
#[test]
fn the_keepers_thread_arms_every_hint_source_and_settles_on_the_backstop() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let folder = dir.path().to_owned();
    let control = Arc::<watch::Control>::default();
    let stopped = Arc::new(AtomicBool::new(false));
    let (tell, told) = std::sync::mpsc::channel();
    let worker_control = Arc::clone(&control);
    let worker_stop = Arc::clone(&stopped);
    let worker = std::thread::spawn(move || {
        let sys = aterm_machines::port::Fake::root();
        live_worker(
            &Mutex::default(),
            &worker_stop,
            Instant::now(),
            &Notifications {
                changed: || {},
                invalidated: || {},
                console_return: None,
                pending: None,
                ended: None,
            },
            &worker_control,
            &Outside {
                sys: &sys,
                scope: &|| Ok(InputSetupScope::Inactive),
                activate: &|_, _, _| panic!("a keeper off the console activates nothing"),
                records: Some(folder),
                chose: &|_, coverage, wait| {
                    let _ = tell.send((coverage, wait));
                },
            },
        );
    });
    let whole = Coverage {
        devices: true,
        console: true,
        records: true,
    };
    // Each round's coverage and wait, to the first that settled or found a source unarmed, and
    // no more than twenty, room for three keyboard notices' confirming rounds: each is a wait for
    // something that must happen.
    let mut rounds = Vec::new();
    for _ in 0..20 {
        let Ok(round) = told.recv_timeout(Duration::from_secs(60)) else {
            break;
        };
        rounds.push(round);
        if round.0 != whole || round.1 == BACKSTOP {
            break;
        }
        control.wake();
    }
    stopped.store(true, Ordering::Release);
    control.wake();
    let ended = worker.join();
    assert!(ended.is_ok(), "{ended:?}");
    assert_eq!(
        rounds.first(),
        Some(&(whole, PACE)),
        "the first round: every source armed, and its new sample owes a round at the pace"
    );
    assert_eq!(
        rounds.last(),
        Some(&(whole, BACKSTOP)),
        "a later round settled"
    );
    assert!(
        rounds
            .iter()
            .all(|round| round.0 == whole && (round.1 == PACE || round.1 == BACKSTOP)),
        "{rounds:?}"
    );
}

/// K3, on the function the keeper's thread runs, with no pace: a background keeper at the
/// console under a pending scope settles as one off the console does, since nothing reads its
/// pending sample, and a record published into its folder starts one round.
/// NEGATIVE CONTROL: with a background keeper's pending sample counted as owed, every round's
/// wait is the pace and no wait blocks for the records hint.
#[test]
fn a_background_keeper_under_a_pending_scope_settles_and_a_record_starts_its_round() {
    use watch::{Causes, Woke};
    let none = Causes::default();
    let dir = aterm_tempfile::tempdir().unwrap();
    let folder = dir.path().join("doctor");
    std::fs::create_dir(&folder).unwrap();
    let worker = KeptWorker::start_as(Some(folder.clone()), Duration::ZERO, true, || {
        let who = aterm_machines::model::User {
            name: "alice".into(),
            uid: 501,
            gid: 20,
            home: "/Users//alice".into(),
            shell: "/bin/zsh".into(),
            admin: true,
        };
        let mut sys = aterm_machines::port::Fake::root();
        sys.euid = who.uid;
        sys.logged_in(&who);
        sys.keyboard_services = Some(Ok(vec![41]));
        sys.stat_of(
            "/dev/console",
            aterm_machines::port::Stat {
                uid: who.uid,
                gid: who.gid,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        (sys, InputSetupScope::Pending(7))
    });
    let one_round = |cause| {
        [
            Ok(Kept::Waited(Woke::Hint(cause))),
            Ok(Kept::Waited(Woke::Deadline)),
            Ok(Kept::Round(cause, BACKSTOP)),
        ]
    };
    // The fresh worker's two rounds, then the round for the records watch's first look.
    let mut told: Vec<_> = (0..6).map(|_| worker.next()).collect();
    let staged = dir.path().join("record.new");
    std::fs::write(&staged, b"{}").unwrap();
    std::fs::rename(&staged, folder.join("record.json")).unwrap();
    told.extend((0..3).map(|_| worker.next()));
    let after = worker.end();
    let mut expected = vec![
        Ok(Kept::Round(none, Duration::ZERO)),
        Ok(Kept::Waited(Woke::Deadline)),
        Ok(Kept::Round(none, BACKSTOP)),
    ];
    expected.extend(one_round(Causes::RECORDS));
    expected.extend(one_round(Causes::RECORDS));
    assert_eq!(told, expected);
    assert_eq!(after, [Kept::Waited(Woke::Stopped)], "nothing but the stop");
}

/// K2: the watch the keeper's thread makes ([`watch::Watch::new`]) arms every keyboard
/// lifecycle notice and the console user's key on this Mac, and the key's callout, called as
/// the store's run-loop source calls it, is the next wait's `console` cause, once.
/// NEGATIVE CONTROLS: with the console registration deleted from the live watch's setup, the
/// watch says `console: false`; with the callout setting nothing, the first wait finds nothing
/// to tell and would block: the worker panics there, and the two returns are `None`.
#[test]
fn the_console_users_key_arms_and_its_callout_is_the_waits_console_cause() {
    use watch::{Causes, Woke};
    let armed = std::thread::spawn(|| {
        let stopped = AtomicBool::new(false);
        watch::Watch::new(Arc::default(), &stopped).armed()
    })
    .join();
    assert_eq!(
        armed.ok(),
        Some(watch::Armed {
            devices: true,
            console: true
        }),
        "the live watch's keyboard notices and its dynamic store source armed"
    );
    let told = std::thread::spawn(|| {
        let stopped = AtomicBool::new(false);
        let mut watch = watch::Watch::private(Arc::default(), &stopped, || {
            panic!("a told notice needs no blocking wait")
        });
        let hour = Instant::now() + Duration::from_secs(3_600);
        watch.console_notice();
        watch.console_notice();
        [
            watch.wait(&stopped, hour),
            watch.wait(&stopped, Instant::now()),
        ]
    })
    .join();
    assert_eq!(
        told.ok(),
        Some([Woke::Hint(Causes::CONSOLE), Woke::Deadline]),
        "two notices before a wait are one cause, and a taken cause is not told again"
    );
}

#[test]
fn an_unseen_model_retries_the_same_native_service_and_applies_when_its_census_recovers() {
    use aterm_machines::items::keyboard::{CAPS_LOCK, LEFT_CONTROL};
    use aterm_machines::port::{Cmd, Fake, Ran, Stat};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const REGISTRY: u64 = 41;

    let who = aterm_machines::model::User {
        name: "alice".into(),
        uid: 501,
        gid: 20,
        home: "/Users//alice".into(),
        shell: "/bin/zsh".into(),
        admin: true,
    };
    let mut sys = Fake::root();
    sys.euid = who.uid;
    sys.logged_in(&who);
    sys.keyboard_services = Some(Ok(vec![REGISTRY]));
    sys.stat_of(
        "/dev/console",
        Stat {
            uid: who.uid,
            gid: who.gid,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 0,
        },
    );
    let census = format!(
        "Services:\n{:<12}{:<12}{:<16}{:<24}Built-In\n{:<12}{:<12}{:<16}{:<24}0\n{:<12}{:<12}{:<16}{:<24}0\n\nDevices:\n",
        "VendorID",
        "ProductID",
        "RegistryID",
        "Product",
        "0x5ac",
        "0x378",
        "0x2a",
        "Other keyboard",
        "0xfffe",
        "0xa9",
        "0x29",
        "Fixture keyboard"
    );
    let available = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&available);
    let written_before_reset = Arc::new(AtomicUsize::new(0));
    let forgotten = Arc::clone(&written_before_reset);
    sys.simulate(move |sys, cmd| {
        let args: Vec<_> = cmd.args.iter().filter_map(|arg| arg.to_str()).collect();
        match cmd.program.file_name().and_then(|name| name.to_str()) {
            Some("hidutil") if args.first() == Some(&"list") => {
                Some(Ran::ok(if observed.load(Ordering::SeqCst) {
                    &census
                } else {
                    "Services:\n\nDevices:\n"
                }))
            }
            Some("hidutil") if args.first() == Some(&"property") => {
                let matching: aterm_json::Value = aterm_json::from_str(args[2]).unwrap();
                let external = matching
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["VendorID"].as_i64() == Some(65534));
                Some(Ran::ok(&format!(
                    "RegistryID Key Value\n{:x} UserKeyMapping (null)\n",
                    if external { REGISTRY } else { 10 }
                )))
            }
            Some("hidutil") if args.first() == Some(&"dump") => {
                let changes: Vec<_> = sys
                    .keyboard_changes()
                    .into_iter()
                    .filter(|change| change.registry == REGISTRY)
                    .collect();
                let native = changes
                    .into_iter()
                    .skip(forgotten.load(Ordering::SeqCst))
                    .next_back()
                    .and_then(|change| change.native)
                    .unwrap_or_default();
                Some(Ran::ok(&keyboard_dump(&[REGISTRY], &native)))
            }
            Some("defaults") => Some(Ran::ok(&aterm_machines::plist::document(
                &aterm_machines::model::PValue::Dict(Vec::new()),
            ))),
            _ => None,
        }
    });
    let invalidations = std::cell::Cell::new(0);
    let writes_before = std::cell::Cell::new(0);
    let activate = |revision, ids: &[u64]| {
        assert_eq!(revision, 7);
        aterm_machines::init::activate_keyboard_attachments(
            &sys,
            ids,
            &|_| false,
            &|| true,
            &|| {
                assert_eq!(
                    sys.keyboard_changes().len(),
                    writes_before.get(),
                    "retire before the next real native setter"
                );
                invalidations.set(invalidations.get() + 1);
            },
        )
        .map(Some)
    };
    let mut keeper = Keeper::default();
    assert!(
        !keeper
            .round(0, Some(7), || sys.keyboard_service_observations(), activate)
            .unwrap()
    );
    assert!(sys.keyboard_changes().is_empty());
    assert_eq!(
        invalidations.get(),
        1,
        "the missing requested cohort retired the old fact"
    );
    assert!(
        keeper.live.is_none(),
        "unseen native coverage supplies no live diagnostic"
    );
    available.store(true, Ordering::SeqCst);
    assert!(
        !keeper
            .round(
                4999,
                Some(7),
                || sys.keyboard_service_observations(),
                |_, _| panic!("unfinished retry remains paced")
            )
            .unwrap()
    );
    assert!(
        keeper
            .round(
                5000,
                Some(7),
                || sys.keyboard_service_observations(),
                activate
            )
            .unwrap()
    );
    let changed = sys.keyboard_changes();
    assert_eq!(
        invalidations.get(),
        2,
        "the actual native mutation retired before its write"
    );
    assert_eq!(
        changed.len(),
        1,
        "the unrelated model's NotApplicable row does not block coverage"
    );
    assert_eq!(changed[0].registry, REGISTRY);
    assert_eq!(changed[0].native, Some(vec![(CAPS_LOCK, LEFT_CONTROL)]));
    assert!(
        keeper.live.is_none(),
        "a setter's before-image is not its verified live state"
    );
    assert!(sys.calls().iter().all(|call| {
        !call.contains("defaults") || (!call.contains(" write ") && !call.contains(" delete "))
    }));
    assert!(
        !keeper
            .round(
                5001,
                Some(7),
                || sys.keyboard_service_observations(),
                |_, _| panic!("verified cohort settles")
            )
            .unwrap()
    );
    assert_eq!(
        keeper.live,
        Some((
            7,
            LiveKeyboardMapping {
                control: 1,
                explicit_other: 0,
                held: 0,
            }
        ))
    );
    assert!(matches!(
        aterm_machines::init::activate_keyboard_attachments(
            &sys,
            &[REGISTRY],
            &|_| false,
            &|| true,
            &|| panic!("a real no-op must retain an existing valid observation"),
        )
        .unwrap(),
        KeyboardAttachmentActivation::Verified(_)
    ));
    assert_eq!(sys.keyboard_changes().len(), 1);

    // The same live service loses its map after a successfully verified activation.
    // Neither its registry identity nor the accepted setup revision changes.
    written_before_reset.store(changed.len(), Ordering::SeqCst);
    writes_before.set(changed.len());
    let reset = sys
        .run_complete_stdout(&Cmd::new("hidutil").args(["dump", "services", "-f", "xml"]))
        .unwrap();
    assert!(reset.success());
    assert_eq!(reset.out(), keyboard_dump(&[REGISTRY], &[]));
    assert_eq!(sys.keyboard_changes().len(), 1, "no reset-time setter");
    assert!(
        keeper
            .round(
                5002,
                Some(7),
                || sys.keyboard_service_observations(),
                activate
            )
            .unwrap(),
        "an unchanged service identity must not hide a lost effective mapping"
    );
    let repaired = sys.keyboard_changes();
    assert_eq!(repaired.len(), 2);
    assert_eq!(repaired[1].registry, REGISTRY);
    assert_eq!(repaired[1].native, Some(vec![(CAPS_LOCK, LEFT_CONTROL)]));
    assert_eq!(invalidations.get(), 3);
    let paid = sys.calls().len();
    assert!(
        !keeper
            .round(
                5003,
                Some(7),
                || sys.keyboard_service_observations(),
                |_, _| panic!("the verified repaired state needs no further activation")
            )
            .unwrap()
    );
    assert_eq!(sys.keyboard_changes().len(), 2);
    assert!(
        sys.calls()[paid..]
            .iter()
            .all(|call| call == "keyboard-service-ids")
    );

    let mut inputs = Inputs::default();
    let positive_readback = std::cell::Cell::new(false);
    let invalidate = || positive_readback.set(false);
    let round = |inputs: &mut Inputs, now, held| {
        inputs.round(
            true,
            now,
            || Ok(InputSetupScope::Applied(7)),
            || panic!("accepted inputs read one native mapping sample"),
            || sys.keyboard_service_observations(),
            |revision, ids| {
                if held {
                    Ok(None)
                } else {
                    activate(revision, ids)
                }
            },
            &|| panic!("accepted inputs are not a pending stage"),
            &invalidate,
        )
    };
    assert!(round(&mut inputs, 6000, false).unwrap());
    assert_eq!(
        inputs.live,
        Some((
            7,
            LiveKeyboardMapping {
                control: 1,
                explicit_other: 0,
                held: 0,
            }
        )),
        "an actual final-probe no-op preserves its known matching passive state"
    );
    assert_eq!(
        sys.keyboard_changes().len(),
        2,
        "the initial receipt is a no-op"
    );
    written_before_reset.store(2, Ordering::SeqCst);
    writes_before.set(2);
    assert!(round(&mut inputs, 6001, false).unwrap());
    assert!(inputs.live.is_none());
    assert_eq!(sys.keyboard_changes().len(), 3);
    positive_readback.set(true);
    assert!(!round(&mut inputs, 6002, false).unwrap());
    assert!(inputs.live.is_some());
    assert!(
        positive_readback.get(),
        "our verified write is not new drift"
    );
    written_before_reset.store(3, Ordering::SeqCst);
    assert!(!round(&mut inputs, 6003, true).unwrap());
    assert!(
        inputs.live.is_none(),
        "held repair retains retry custody, not a positive diagnostic"
    );
    assert!(
        !positive_readback.get(),
        "a reset to the old before-image retires the newer fact even while repair is held"
    );
    assert_eq!(
        sys.keyboard_changes().len(),
        3,
        "held repair writes nothing"
    );
    inputs
        .round(
            false,
            6004,
            || panic!("away console reads no scope"),
            || panic!("away console reads no IDs"),
            || panic!("away console reads no maps"),
            |_, _| panic!("away console never activates"),
            &|| {},
            &invalidate,
        )
        .unwrap();
    assert!(inputs.live.is_none());
    inputs
        .round(
            true,
            6005,
            || Ok(InputSetupScope::Inactive),
            || panic!("withdrawn scope reads no IDs"),
            || panic!("withdrawn scope reads no maps"),
            |_, _| panic!("withdrawn scope never activates"),
            &|| {},
            &invalidate,
        )
        .unwrap();
    assert!(inputs.live.is_none());
}
