// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::client::{Membership, Status};
use std::cell::Cell;

fn plan(setting: Setting) -> Plan {
    let mut selection = Selection::none();
    selection.set(setting, true);
    Plan::from(selection)
}

fn routes(last: u8) -> Option<String> {
    Some(format!("en0 192.0.2.{last} 00:11:22:33:44:55"))
}

fn git(present: bool) -> RepositoryGit {
    if present {
        RepositoryGit::Installed(aterm_machines::items::github::CLT_GIT.into())
    } else {
        RepositoryGit::Missing
    }
}

fn vault(membership: Membership) -> Option<Status> {
    Some(Status {
        config: None,
        machine: None,
        membership,
        names: Vec::new(),
    })
}

/// A parked stage's pause is the park mark's: ahead, it is the deadline the worker's timer
/// runs to and no due deadline yet; once it ends, at its second exactly, it is due and the
/// timer runs to nothing of it; and a park set after the clock now (the clock set back since)
/// is due at once rather than a day and its pause away. NEGATIVE CONTROL: read the stage's
/// due deadline only past `now`, or its next one up to `now`, and the worker sleeps through
/// the stage's own next begin.
#[test]
fn a_parked_stage_is_ahead_until_its_pause_ends_and_then_due() {
    let root = aterm_tempfile::tempdir().unwrap();
    let revision = authorize_at(root.path(), &plan(Setting::Dock))
        .unwrap()
        .unwrap();
    let parked_at = 1_800_000_000;
    let park = |at: u64| {
        ledger::update(root.path(), revision, |ledger| {
            *ledger = ledger::Ledger::of(revision);
            for _ in 0..2 {
                let stamp = ledger::Stamp {
                    at,
                    ..ledger::Stamp::default()
                };
                let admitted = ledger.begin(Stage::Workstation, 0, stamp, root.path());
                assert_eq!(admitted, ledger::Admit::Run);
                ledger.recover(0, None, None, at);
            }
            assert_eq!(
                ledger.parked(Stage::Workstation),
                Some(ledger::Park::Crashed)
            );
        })
        .unwrap();
    };
    let read = |now| {
        initial_setup_park_mark_with(Some(root.path()), None, now, || {
            panic!("a Dock-only run must not inspect network interfaces")
        })
    };
    park(parked_at);
    let due = parked_at + 60 * 60;
    let ahead = read(due - 1);
    assert_eq!(ahead.stage_retry_due(), None, "its pause is still ahead");
    assert_eq!(ahead.next_due(), Some(due), "the timer runs to its end");
    let ended = read(due);
    assert_eq!(ended.stage_retry_due(), Some(due), "due at its second");
    assert_eq!(ended.next_due(), None, "nothing of it ahead");
    assert_eq!(read(due + 3_600).stage_retry_due(), Some(due), "still due");
    // Set a day ahead of the clock now: due at once.
    park(parked_at + 86_400);
    let set_back = read(parked_at);
    assert_eq!(set_back.stage_retry_due(), Some(parked_at));
    assert_eq!(set_back.next_due(), None);
}

#[test]
fn a_workstation_retry_deadline_wakes_after_the_worker_has_parked() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = plan(Setting::Dock);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut doctor = super::super::tests::NeverVerifies::dock();
    let started = 1_800_000_000;
    let read = |after| {
        initial_setup_park_mark_with(Some(root.path()), None, started + after, || {
            panic!("a Dock-only run must not inspect network interfaces")
        })
    };
    // The try that failed leaves a retry; each pass after it, before this item's
    // second try, puts it off and parks on its deadline.
    for minute in 0..10 {
        let report = doctor
            .pass_at(root.path(), plan.selection, started + minute * 60)
            .unwrap();
        let next = if minute == 0 {
            SetupNext::Retry
        } else {
            SetupNext::Park
        };
        assert_eq!(SetupNext::of(&report.rows), next, "minute {minute}");
        assert_eq!(doctor.relaunches(), 1, "minute {minute}: put off");
    }
    let parked = read(540);
    assert_eq!(parked.mac_settings_retry_due(), None);
    assert_eq!(parked, read(599), "no early retry or changed park fact");
    assert_ne!(
        parked,
        read(600),
        "the second allowed doctor attempt must wake a parked worker"
    );
    assert_eq!(read(600).mac_settings_retry_due(), Some(started + 600));
    assert!(!read(600).facts_changed_since(&parked));
    assert_eq!(revision_at(root.path()).unwrap(), revision);
    doctor
        .pass_at(root.path(), plan.selection, started + 600)
        .unwrap();
    assert_eq!(doctor.relaunches(), 2);
    let paused = read(600);
    assert_eq!(paused.mac_settings_retry_due(), None);
    assert_eq!(paused, read(1_799), "the third try is not due yet");
    doctor
        .pass_at(root.path(), plan.selection, started + 1_200)
        .unwrap();
    assert_eq!(doctor.relaunches(), 2, "put off until its pause ends");
    assert_eq!(
        read(1_800).mac_settings_retry_due(),
        Some(started + 1_800),
        "the third try, twenty minutes on, wakes the parked worker"
    );
    doctor
        .pass_at(root.path(), plan.selection, started + 1_800)
        .unwrap();
    assert_eq!(doctor.relaunches(), 3, "never given up on");
}

/// A park waits on what a person does in System Settings or macOS only while an admitted
/// Accounts or Permissions stage is left: only such a park ends when one of aterm's windows
/// gains focus. A Mac settings stage, or one of those two finished, does not listen.
#[test]
fn only_a_left_accounts_or_permissions_stage_waits_on_settings() {
    for (setting, waits) in [
        (Setting::Permissions, true),
        (Setting::Messages, true),
        (Setting::Dock, false),
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &plan(setting)).unwrap().unwrap();
        let read = || {
            initial_setup_park_mark_with(Some(root.path()), None, 1_000, || None)
                .waits_on_settings()
        };
        assert_eq!(read(), waits, "{setting:?}");
        let pending = progress(root.path()).unwrap().unwrap();
        let stage = match setting {
            Setting::Permissions => Stage::Permissions,
            Setting::Messages => Stage::Accounts,
            _ => Stage::Workstation,
        };
        save_progress(
            root.path(),
            Progress {
                done: stage.bit(),
                ..pending
            },
        )
        .unwrap();
        assert!(!read(), "{setting:?}: finished");
    }
    assert!(!initial_setup_park_mark_with(None, None, 1_000, || None).waits_on_settings());
}

#[test]
fn a_mac_settings_deadline_requires_a_selected_unfinished_admitted_stage() {
    for setting in [Setting::Dock, Setting::Messages] {
        let root = aterm_tempfile::tempdir().unwrap();
        let selected = plan(setting);
        authorize_at(root.path(), &selected).unwrap().unwrap();
        let mut doctor = if setting == Setting::Dock {
            super::super::tests::NeverVerifies::dock()
        } else {
            super::super::tests::NeverVerifies::accounts()
        };
        doctor
            .pass_at(root.path(), selected.selection, 1_000)
            .unwrap();
        let read = || {
            initial_setup_park_mark_with(Some(root.path()), None, 1_600, || {
                panic!("no selected Secrets stage")
            })
            .mac_settings_retry_due()
        };
        assert_eq!(read(), Some(1_600), "{setting:?}");
        let pending = progress(root.path()).unwrap().unwrap();
        let stage = if setting == Setting::Dock {
            Stage::Workstation
        } else {
            Stage::Accounts
        };
        for blocked in [
            Progress {
                done: stage.bit(),
                ..pending
            },
            Progress {
                completed: true,
                ..pending
            },
            Progress {
                stopped: true,
                ..pending
            },
            Progress {
                admitted: false,
                ..pending
            },
            Progress {
                revision: pending.revision + 1,
                ..pending
            },
        ] {
            save_progress(root.path(), blocked).unwrap();
            assert_eq!(read(), None, "{setting:?}: {blocked:?}");
        }
        if setting == Setting::Dock {
            for only in [Selection::none(), plan(Setting::Widgets).selection] {
                save_progress(
                    root.path(),
                    Progress {
                        only: Some(only),
                        ..pending
                    },
                )
                .unwrap();
                assert_eq!(read(), None, "the choice left this scope alone");
            }
        }
        save_progress(root.path(), pending).unwrap();
        save_plan(root.path(), &plan(Setting::Permissions)).unwrap();
        assert_eq!(read(), None, "mismatched plan");
        save_progress(
            root.path(),
            Progress {
                plan: plan_digest(&plan(Setting::Permissions)),
                ..pending
            },
        )
        .unwrap();
        assert_eq!(read(), None, "no selected Mac settings stage");
        save_plan(root.path(), &selected).unwrap();
        save_progress(root.path(), pending).unwrap();
        request_stop_at(root.path(), pending.revision).unwrap();
        assert_eq!(read(), None, "a requested Stop holds before the fold");
    }
}

#[test]
fn pending_secrets_wakes_for_routes_while_membership_stays_unconfigured() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &plan(Setting::Secrets)).unwrap();
    let read = |network| {
        initial_setup_park_mark_with(
            Some(root.path()),
            vault(Membership::Unconfigured),
            1_000,
            || routes(network),
        )
    };
    let before = read(1);
    assert_eq!(before, read(1), "an unchanged network stays parked");
    assert_ne!(before, read(2), "a new route wakes the unfinished vault");
    let admitted = progress(root.path()).unwrap().unwrap();
    assert!(admitted.admitted && !admitted.completed && admitted.done == 0);
}

#[test]
fn completed_network_stages_still_wake_while_accounts_is_pending() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut selected = plan(Setting::Secrets);
    selected.selection.set(Setting::Repositories, true);
    selected.selection.set(Setting::Messages, true);
    let revision = authorize_at(root.path(), &selected).unwrap().unwrap();
    let mut saved = progress(root.path()).unwrap().unwrap();
    saved.done = Stage::Secrets.bit() | Stage::Repositories.bit();
    save_progress(root.path(), saved).unwrap();
    let materializations = Cell::new(0);
    let read = |network: u8, subscriber: Option<bool>, installed: Option<bool>| {
        initial_setup_park_mark_reading(
            Some(root.path()),
            vault(Membership::In { seq: 1, heard: 0 }),
            1_000,
            || routes(network),
            || {
                materializations.set(materializations.get() + 1);
                None
            },
            || subscriber,
        )
        .with_git(|| installed.map(git))
        .with_lan_broker(|| panic!("a completed join has no broker readiness to poll"))
    };
    let before = read(1, Some(true), Some(false));
    assert!(before.watches().secrets && before.watches().network && before.watches().git);
    assert_eq!(before.network_run, Some(revision));
    assert_eq!(before.materialization_run, Some(revision));
    assert_eq!(materializations.get(), 1);
    assert_eq!(before.subscriber_run, Some(revision));
    assert_eq!(before.git_run, Some(revision));
    assert_eq!(before.retry_due(), None);
    assert_eq!(before.next_due(), None);
    assert!(!before.waits_on_lan_broker());
    assert!(!read(1, Some(true), Some(false)).facts_changed_since(&before));
    assert!(read(2, Some(true), Some(false)).facts_changed_since(&before));
    assert!(read(1, Some(false), Some(false)).facts_changed_since(&before));
    assert!(read(1, Some(true), Some(true)).facts_changed_since(&before));
    let installed = read(1, Some(true), Some(true));
    assert!(read(1, Some(true), Some(false)).facts_changed_since(&installed));
    let mut unknown = read(1, None, None);
    assert!(!unknown.facts_changed_since(&before));
    unknown.retain_known_from(&before);
    assert_eq!(
        unknown, before,
        "unknown inputs cannot keep waking the park"
    );

    for inactive in [
        Progress {
            completed: true,
            ..saved
        },
        Progress {
            stopped: true,
            ..saved
        },
        Progress {
            admitted: false,
            ..saved
        },
        Progress {
            done: saved.done | Stage::Accounts.bit(),
            ..saved
        },
    ] {
        save_progress(root.path(), inactive).unwrap();
        let retired = read(2, Some(false), Some(true));
        assert!(retired.watches().none(), "{inactive:?}");
        assert!(!retired.facts_changed_since(&before));
        assert_eq!(retired.next_due(), None);
    }
    save_progress(root.path(), saved).unwrap();
    request_stop_at(root.path(), revision).unwrap();
    assert!(read(2, Some(false), Some(true)).watches().none());

    let off = aterm_tempfile::tempdir().unwrap();
    authorize_at(off.path(), &plan(Setting::Messages)).unwrap();
    let mark = initial_setup_park_mark_reading(
        Some(off.path()),
        None,
        1_000,
        || panic!("network groups are off"),
        || panic!("no selected materialization"),
        || panic!("no selected subscriber"),
    )
    .with_git(|| panic!("Repositories is off"))
    .with_lan_broker(|| panic!("Secrets is off"));
    assert!(mark.watches().none());
    assert!(mark.waits_on_settings());
    assert_eq!(mark.next_due(), None);
}

#[test]
fn routes_are_not_sampled_without_a_coherent_pending_network_stage() {
    for state in [
        "missing",
        "permissions",
        "staged",
        "done",
        "completed",
        "stopped",
        "cancelled",
        "restored",
        "unreadable",
        "mismatched",
        "missing decision",
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        if state != "missing" {
            let selected = plan(if state == "permissions" {
                Setting::Permissions
            } else {
                Setting::Secrets
            });
            authorize_at(root.path(), &selected).unwrap();
            let mut saved = progress(root.path()).unwrap().unwrap();
            match state {
                "staged" => saved.admitted = false,
                "done" => saved.done |= Stage::Secrets.bit(),
                "completed" => saved.completed = true,
                "stopped" => saved.stopped = true,
                "mismatched" => saved.plan = [0; 32],
                _ => {}
            }
            save_progress(root.path(), saved).unwrap();
            match state {
                "cancelled" | "restored" => write_json(
                    &root.path().join("first-run.json"),
                    &json!({"profile":PROFILE,"state":state}),
                )
                .unwrap(),
                "missing decision" => {
                    std::fs::remove_file(root.path().join("first-run.json")).unwrap();
                }
                "unreadable" => {
                    std::fs::write(root.path().join("initial-setup.json"), b"not json").unwrap();
                }
                _ => {}
            }
        }
        let calls = Cell::new(0);
        let read = |network| {
            initial_setup_park_mark_with(
                Some(root.path()),
                vault(Membership::Unconfigured),
                1_000,
                || {
                    calls.set(calls.get() + 1);
                    routes(network)
                },
            )
        };
        assert_eq!(read(1), read(2), "{state} must stay parked");
        assert_eq!(calls.get(), 0, "{state} sampled routes");
    }
    let calls = Cell::new(0);
    initial_setup_park_mark_with(None, vault(Membership::Unconfigured), 1_000, || {
        calls.set(calls.get() + 1);
        routes(1)
    });
    assert_eq!(calls.get(), 0, "an unknown root sampled routes");
    let missing = aterm_tempfile::tempdir().unwrap().path().join("absent");
    initial_setup_park_mark_with(
        Some(&missing),
        vault(Membership::Unconfigured),
        1_000,
        || panic!("no admitted run"),
    );
    assert!(!missing.exists(), "observing must not create a state root");
}

#[test]
fn membership_changes_still_wake_without_a_selected_vault() {
    let root = aterm_tempfile::tempdir().unwrap();
    let read = |membership| {
        initial_setup_park_mark_with(Some(root.path()), vault(membership), 1_000, || {
            panic!("no plan")
        })
    };
    let before = read(Membership::Pending);
    let after = read(Membership::In { seq: 1, heard: 10 });
    assert_ne!(before, after);
    assert_eq!(after, read(Membership::In { seq: 1, heard: 20 }));
    assert_ne!(after, read(Membership::In { seq: 2, heard: 20 }));
    assert_ne!(
        read(Membership::Revoked { seq: 2 }),
        read(Membership::Revoked { seq: 3 })
    );
}

/// The Command Line Tools arriving during a pass is a fact that wakes the run's park on
/// its Repositories stage (the tab the window opened for their install, or a person's own
/// `aterm init`), whatever else the mark reads, and a park already sampling sees them
/// change the mark. Losing Git also changes current repository readiness; unknown readings,
/// unselected Repositories and a run with no selected work left supply no such fact.
#[test]
fn the_command_line_tools_arriving_wakes_a_run_parked_on_its_repositories() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut selected = plan(Setting::Secrets);
    selected.selection.set(Setting::Repositories, true);
    authorize_at(root.path(), &selected).unwrap();
    let read = |installed: Option<bool>| {
        initial_setup_park_mark_reading(
            Some(root.path()),
            vault(Membership::Pending),
            1_000,
            || routes(1),
            || None,
            || Some(false),
        )
        .with_git(|| installed.map(git))
    };
    let missing = read(Some(false));
    assert!(!read(Some(false)).facts_changed_since(&missing));
    assert!(
        read(Some(true)).facts_changed_since(&missing),
        "the tools' install ends the wait"
    );
    // A park already sampling ends on any later sample that differs from its first.
    assert_ne!(
        read(Some(true)),
        missing,
        "the tools' install changes the sample"
    );
    assert!(
        missing.facts_changed_since(&read(Some(true))),
        "losing Git invalidates current repository evidence"
    );
    assert!(!read(None).facts_changed_since(&missing));
    assert!(!read(Some(true)).facts_changed_since(&read(None)));

    let mut saved = progress(root.path()).unwrap().unwrap();
    saved.done |= Stage::Secrets.bit() | Stage::Repositories.bit();
    save_progress(root.path(), saved).unwrap();
    let finished = read(Some(true));
    assert_eq!(finished.git, None, "read for no unfinished stage");
    assert!(!finished.facts_changed_since(&missing));

    // A plan without the repositories never reads the tools.
    let other = aterm_tempfile::tempdir().unwrap();
    authorize_at(other.path(), &plan(Setting::Secrets)).unwrap();
    let unselected = |installed: Option<bool>| {
        initial_setup_park_mark_reading(
            Some(other.path()),
            vault(Membership::Pending),
            1_000,
            || routes(1),
            || None,
            || Some(false),
        )
        .with_git(|| installed.map(git))
    };
    assert_eq!(unselected(Some(true)).git, None);
    assert!(!unselected(Some(true)).facts_changed_since(&unselected(Some(false))));
}

/// A repositories-only run can exhaust its automatic tries while native Git lacks a
/// required capability. Managed Git arriving later must wake it without Secrets or a
/// network move; both executables being installed is not the same readiness fact.
#[test]
fn a_new_managed_git_wakes_repositories_even_when_native_git_was_already_installed() {
    use aterm_machines::model::User;
    use aterm_machines::port::{Fake, Stat};

    let root = aterm_tempfile::tempdir().unwrap();
    let revision = authorize_at(root.path(), &plan(Setting::Repositories))
        .unwrap()
        .unwrap();
    let user = User {
        name: "tester".into(),
        uid: 501,
        gid: 20,
        home: "/test-git-readiness-home".into(),
        shell: "/bin/zsh".into(),
        admin: false,
    };
    let sys = Fake::root();
    sys.logged_in(&user);
    sys.executable(aterm_machines::items::github::CLT_GIT, b"old native Git");
    let read = || {
        initial_setup_park_mark_reading(
            Some(root.path()),
            None,
            1_000,
            || routes(1),
            || None,
            || None,
        )
        .with_git(|| Some(RepositoryGit::read(&sys)))
    };
    let before = read();
    let prefix = Path::new(&user.home).join("Library/Application Support/aterm/pkg");
    let store = prefix.join("store/git");
    let file = Stat {
        uid: user.uid,
        gid: user.gid,
        mode: 0o755,
        dir: false,
        symlink: false,
        len: 0,
    };
    sys.link(store.join("current").to_str().unwrap(), "12");
    for path in [
        store.join("12.ready"),
        store.join("12/bin/git"),
        prefix.join("bin/git"),
    ] {
        sys.stat_of(path.to_str().unwrap(), file);
    }
    sys.runtime_unready(&store.join("12"), "runtime not ready");
    assert!(
        !read().facts_changed_since(&before),
        "an unadmitted build does not wake setup"
    );
    sys.runtime_ready(&store.join("12"));
    let after = read();
    assert!(
        after.facts_changed_since(&before),
        "a new admitted Git must restart the parked run"
    );
    assert!(
        before.watches().git,
        "keep watching a selected unfinished repository stage"
    );
    let command = aterm_machines::items::github::installed_git(&sys);
    assert!(
        !read().facts_changed_since(&after),
        "unchanged activation stays parked"
    );
    sys.link(store.join("current").to_str().unwrap(), "13");
    for path in [store.join("13.ready"), store.join("13/bin/git")] {
        sys.stat_of(path.to_str().unwrap(), file);
    }
    assert_eq!(
        aterm_machines::items::github::installed_git(&sys),
        command,
        "atpkg's command shim stays at the same path"
    );
    assert!(
        read().facts_changed_since(&after),
        "a newer build behind that shim wakes too"
    );
    assert!(
        sys.calls().is_empty(),
        "sampling Git readiness starts no processes"
    );
    request_stop_at(root.path(), revision).unwrap();
    let stopped = read();
    assert!(!stopped.facts_changed_since(&before));
    assert!(!stopped.watches().git);
}

#[test]
fn late_pass_facts_compare_only_known_inputs_still_owed_by_the_same_run() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut selected = plan(Setting::Secrets);
    selected.selection.set(Setting::Repositories, true);
    authorize_at(root.path(), &selected).unwrap();
    let read = |network: Option<u8>, subscriber, vault| {
        initial_setup_park_mark_reading(
            Some(root.path()),
            vault,
            1_000,
            || network.and_then(routes),
            || None,
            || subscriber,
        )
    };
    let before = read(Some(1), Some(false), vault(Membership::Pending));
    assert!(!read(Some(1), Some(false), vault(Membership::Pending)).facts_changed_since(&before));
    assert!(read(Some(2), Some(false), vault(Membership::Pending)).facts_changed_since(&before));
    assert!(read(Some(1), Some(true), vault(Membership::Pending)).facts_changed_since(&before));
    assert!(
        read(
            Some(1),
            Some(false),
            vault(Membership::In { seq: 1, heard: 10 })
        )
        .facts_changed_since(&before)
    );
    let unknown = read(None, None, None);
    assert!(!unknown.facts_changed_since(&before));
    assert!(before.facts_changed_since(&unknown), "newly known inputs");

    // Secrets completion itself supplies no new fact, but its subscriber must still
    // be observed while the selected repository work keeps this setup pending.
    let mut saved = progress(root.path()).unwrap().unwrap();
    saved.done |= Stage::Secrets.bit();
    save_progress(root.path(), saved).unwrap();
    assert!(!read(Some(1), Some(false), vault(Membership::Pending)).facts_changed_since(&before));
    let after_secrets = read(Some(1), Some(true), vault(Membership::Pending));
    assert!(after_secrets.facts_changed_since(&before));

    saved.done |= Stage::Repositories.bit();
    saved.completed = true;
    save_progress(root.path(), saved).unwrap();
    assert!(
        !read(Some(2), Some(true), vault(Membership::Revoked { seq: 2 }))
            .facts_changed_since(&before),
        "completion retires this run's inputs"
    );
    let next = restart_at(root.path(), &selected).unwrap();
    assert!(
        !read(Some(2), Some(true), vault(Membership::Revoked { seq: 2 }))
            .facts_changed_since(&before),
        "a newer run cannot reuse this run's comparison"
    );
    request_stop_at(root.path(), next).unwrap();
    assert!(
        !read(Some(2), Some(true), vault(Membership::Revoked { seq: 2 }))
            .facts_changed_since(&before),
        "Stop retires this run's inputs"
    );
}

#[test]
fn pending_secrets_wakes_at_its_existing_retry_deadline_but_waiting_does_not() {
    let root = aterm_tempfile::tempdir().unwrap();
    let revision = authorize_at(root.path(), &plan(Setting::Secrets))
        .unwrap()
        .unwrap();
    let read = |now| {
        initial_setup_park_mark_with(
            Some(root.path()),
            vault(Membership::Unconfigured),
            now,
            || routes(1),
        )
    };
    let pace = |now| Pace {
        build: "test-build".into(),
        marks: Some("same-network".into()),
        inputs: None,
        materialization: None,
        network: None,
        now,
    };
    let rows = |status: &str| {
        vec![Row {
            name: Stage::Secrets.name().into(),
            status: status.into(),
            detail: "injected vault result".into(),
            elapsed_ms: None,
        }]
    };
    assert_eq!(read(1_000).retry_due(), None, "no previous attempt");
    secrets_retry::record(
        root.path(),
        revision,
        &pace(1_000),
        false,
        &mut rows("blocked"),
    )
    .unwrap();
    let paused = read(1_059);
    assert_eq!(paused.retry_due(), None);
    let due = read(1_060);
    assert_ne!(paused, due, "the same route becomes retryable when due");
    assert_eq!(due.retry_due(), Some(1_060));
    assert!(
        !due.facts_changed_since(&paused),
        "a deadline has its own consumed token, not a changed local input"
    );
    assert_eq!(
        read(1_061).retry_due(),
        Some(1_060),
        "one token until an actual try advances it"
    );

    secrets_retry::record(
        root.path(),
        revision,
        &pace(1_060),
        false,
        &mut rows(WAITING),
    )
    .unwrap();
    assert!(
        !read(2_000).facts_changed_since(&due),
        "consuming the deadline is not a local input change"
    );
    assert_eq!(
        read(2_000).retry_due(),
        None,
        "a person or approval has no timer"
    );
    secrets_retry::record(
        root.path(),
        revision,
        &pace(2_000),
        false,
        &mut rows("error"),
    )
    .unwrap();
    secrets_retry::record(
        root.path(),
        revision,
        &pace(3_000),
        false,
        &mut rows("error"),
    )
    .unwrap();
    assert_eq!(
        read(4_000).retry_due(),
        Some(3_000 + 480),
        "two failed changes come due again by themselves, later each time"
    );
}
