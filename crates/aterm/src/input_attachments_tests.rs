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
        retained();
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
        retained();
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
        sampled
            .send((false, first, reads, applied.clone()))
            .unwrap();
        let hinted = watch.wait_until(&worker_stop, Instant::now() + Duration::from_secs(60));
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
        sampled.send((hinted, second, reads, applied)).unwrap();
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
    assert_eq!(first.unwrap(), (false, Ok(true), 1, vec![vec![1]]));
    assert_eq!(
        second.unwrap(),
        (true, Ok(true), 2, vec![vec![1], vec![2]]),
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
        assert!(watch.wait_until(&stopped, Instant::now() + Duration::from_secs(60)));
        assert!(!watch.wait_until(&stopped, Instant::now()));
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
        assert!(!watch.wait_until(&stopped, Instant::now() + Duration::from_secs(60)));
    });
    assert!(before_setup.join().is_ok());
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
