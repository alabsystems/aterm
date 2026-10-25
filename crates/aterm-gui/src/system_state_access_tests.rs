// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! System State's access steps: the Local Network trigger's target, the menu
//! line, the step gating and a Needs-you row's press. Nothing reaches a LAN, a
//! person's state directory or System Settings. The dialog-ownership regressions use
//! the real process-wide guard, without raising a dialog.

use std::cell::RefCell;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use aterm_types::macos_setup::{
    Access, AccessOutcome, Observation, Plan, Selection, Setting, Standing, Subject,
};

use super::{
    JOIN_PENDING_KEY, LOCAL_NETWORK_ALERT_HOLD, LanInterface, RETIRED_KEYS, StepsReach, lan_target,
    menu_line,
};
use crate::status_item::SystemStateLine;
use crate::{App, InitialSetupPermissionPolicy, SetupStatus};

/// How the last record filed under `key` ended, if it ended.
fn ended_as(app: &App, key: &str) -> Option<aterm_messages::Retired> {
    app.messages
        .log()
        .records()
        .rev()
        .find(|r| r.key.as_deref() == Some(key))
        .and_then(|r| r.retired().cloned())
}

fn ip(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
    Ipv4Addr::new(a, b, c, d)
}

/// A broadcast-capable interface (a LAN segment).
fn lan(address: Ipv4Addr, mask: Ipv4Addr) -> LanInterface {
    LanInterface {
        address,
        mask,
        broadcast: true,
    }
}

/// The Local Network target: a private subnet's first host (its second when the first is this
/// Mac), a link-local one only when no private subnet is up, and none on a subnet with no room
/// for another host, a mask outside /8–/30, a tunnel (no broadcast: a VPN's `utun`) or a public
/// address.
#[test]
fn the_lan_target_is_a_private_subnets_other_host() {
    let slash24 = ip(255, 255, 255, 0);
    assert_eq!(
        lan_target(&[lan(ip(192, 168, 1, 23), slash24)]),
        Some(ip(192, 168, 1, 1))
    );
    assert_eq!(
        lan_target(&[lan(ip(192, 168, 1, 1), slash24)]),
        Some(ip(192, 168, 1, 2)),
        "the first host is this Mac"
    );
    assert_eq!(
        lan_target(&[
            lan(ip(17, 5, 7, 8), slash24),
            lan(ip(10, 0, 0, 9), ip(255, 0, 0, 0))
        ]),
        Some(ip(10, 0, 0, 1)),
        "a public address is no LAN"
    );
    assert_eq!(
        lan_target(&[
            lan(ip(169, 254, 3, 4), ip(255, 255, 0, 0)),
            lan(ip(172, 16, 5, 5), ip(255, 240, 0, 0))
        ]),
        Some(ip(172, 16, 0, 1)),
        "private before link-local"
    );
    assert_eq!(
        lan_target(&[lan(ip(169, 254, 3, 4), ip(255, 255, 0, 0))]),
        Some(ip(169, 254, 0, 1))
    );
    assert_eq!(
        lan_target(&[lan(ip(10, 0, 0, 5), ip(255, 255, 255, 252))]),
        Some(ip(10, 0, 0, 6)),
        "a /30 has room for one other host"
    );
    let tunnel = LanInterface {
        broadcast: false,
        ..lan(ip(10, 8, 0, 2), slash24)
    };
    assert_eq!(
        lan_target(&[tunnel, lan(ip(192, 168, 1, 23), slash24)]),
        Some(ip(192, 168, 1, 1)),
        "a VPN's tunnel is passed over"
    );
    for (interface, why) in [
        (lan(ip(10, 0, 0, 4), ip(255, 255, 255, 254)), "/31"),
        (lan(ip(10, 0, 0, 4), ip(255, 255, 255, 255)), "/32"),
        (lan(ip(10, 0, 0, 4), ip(0, 0, 0, 0)), "an empty mask (/0)"),
        (lan(ip(10, 0, 0, 4), ip(254, 0, 0, 0)), "/7"),
        (
            lan(ip(10, 0, 0, 4), ip(255, 0, 255, 0)),
            "a mask with holes",
        ),
        (lan(ip(17, 5, 7, 8), slash24), "public only"),
        (tunnel, "a tunnel only"),
    ] {
        assert_eq!(lan_target(&[interface]), None, "{why}");
    }
    assert_eq!(lan_target(&[]), None);
}

fn observed(subject: Subject, standing: Standing, note: Option<&str>) -> Observation {
    Observation {
        subject,
        standing,
        now: None,
        after_login: None,
        wanted: subject.wanted().to_string(),
        note: note.map(str::to_string),
        detail: None,
    }
}

fn status(selection: Selection, observed: Vec<Observation>) -> SetupStatus {
    SetupStatus {
        observed_vault_url: Some(Plan::default().vault_url),
        plan: Some(Plan::from(selection)),
        observed,
        ..SetupStatus::default()
    }
}

fn line(label: &str, note: Option<&str>) -> Option<SystemStateLine> {
    Some(SystemStateLine {
        label: label.to_string(),
        note: note.map(str::to_string),
    })
}

/// The menu line says only rows the approved plan manages that need a person or failed: one is
/// named, several are counted; a regressed Full Disk Access grant counts once, as needing a
/// person; the Secrets row's approval code rides along.
#[test]
fn the_menu_line_names_what_needs_a_person() {
    let dock = Subject::Setting(Setting::Dock);
    let fda = Subject::Access(Access::FullDiskAccess);
    let accessibility = Subject::Access(Access::Accessibility);
    let secrets = Subject::Setting(Setting::Secrets);
    let all = Selection::all();

    assert_eq!(menu_line(&SetupStatus::default(), false), None);
    let settled = status(
        all,
        vec![
            observed(dock, Standing::InPlace, None),
            observed(fda, Standing::Differs, None),
            observed(secrets, Standing::Waiting, None),
        ],
    );
    assert_eq!(menu_line(&settled, false), None, "nothing needs a person");

    let one = status(all, vec![observed(dock, Standing::NeedsYou, None)]);
    assert_eq!(
        menu_line(&one, false),
        line("System State: Dock needs you", None)
    );
    let two = status(
        all,
        vec![
            observed(dock, Standing::NeedsYou, None),
            observed(fda, Standing::NeedsYou, None),
        ],
    );
    assert_eq!(
        menu_line(&two, false),
        line("System State: 2 need you", None)
    );
    // A failure is said as one: it may retry by itself and need no one.
    let failed = status(all, vec![observed(dock, Standing::Failed, None)]);
    assert_eq!(
        menu_line(&failed, false),
        line("System State: Dock failed", None)
    );
    let mixed = status(
        all,
        vec![
            observed(dock, Standing::Failed, None),
            observed(fda, Standing::NeedsYou, None),
        ],
    );
    assert_eq!(
        menu_line(&mixed, false),
        line("System State: 1 needs you, 1 failed", None)
    );
    let both_failed = status(
        all,
        vec![
            observed(dock, Standing::Failed, None),
            observed(fda, Standing::Failed, None),
        ],
    );
    assert_eq!(
        menu_line(&both_failed, false),
        line("System State: 2 failed", None)
    );
    assert_eq!(
        menu_line(&both_failed, true),
        line("System State: 1 needs you, 1 failed", None),
        "a regressed grant needs a person, failed or not"
    );

    let mut unmanaged = Selection::all();
    unmanaged.set(Setting::Dock, false);
    assert_eq!(
        menu_line(
            &status(unmanaged, vec![observed(dock, Standing::Failed, None)]),
            false
        ),
        None,
        "a row the plan does not manage"
    );
    let mut no_signouts = Selection::all();
    no_signouts.set(Setting::Messages, false);
    no_signouts.set(Setting::FaceTime, false);
    assert_eq!(
        menu_line(
            &status(
                no_signouts,
                vec![observed(accessibility, Standing::NeedsYou, None)]
            ),
            false
        ),
        None,
        "Accessibility is needed only for a sign-out"
    );
    assert_eq!(
        menu_line(
            &status(all, vec![observed(accessibility, Standing::NeedsYou, None)]),
            false
        ),
        line("System State: Accessibility needs you", None)
    );

    assert_eq!(
        menu_line(&SetupStatus::default(), true),
        line("System State: Full Disk Access needs you", None),
        "a regressed grant shows with no plan"
    );
    assert_eq!(
        menu_line(
            &status(all, vec![observed(fda, Standing::NeedsYou, None)]),
            true
        ),
        line("System State: Full Disk Access needs you", None),
        "counted once"
    );

    let code = "Approve 4F7-2KQ on an admin Mac";
    assert_eq!(
        menu_line(
            &status(all, vec![observed(secrets, Standing::NeedsYou, Some(code))]),
            false
        ),
        line("System State: Secrets vault needs you", Some(code))
    );
    assert_eq!(
        menu_line(
            &status(all, vec![observed(secrets, Standing::Failed, Some(code))]),
            false
        )
        .and_then(|line| line.note),
        None,
        "only a join that waits on a person carries its code"
    );
}

/// A step the last run could not finish is a failure on the menu line though no reading says so
/// (a join the vault refused reads Not joined). A step that names a row is that row, counted once
/// with it; any other is counted, never named; one for a group left as is, or one that only
/// waits or was blocked, is none.
#[test]
fn a_step_the_run_could_not_finish_is_on_the_menu_line() {
    let secrets = Subject::Setting(Setting::Secrets);
    let dock = Subject::Setting(Setting::Dock);
    let fda = Subject::Access(Access::FullDiskAccess);
    let all = Selection::all();
    let step = |name: &str, status: &str| crate::SetupStatusRow {
        name: name.into(),
        status: status.into(),
        detail: "DETAIL".into(),
    };
    let ran = |selection: Selection, observed: Vec<Observation>, rows| SetupStatus {
        rows,
        ..status(selection, observed)
    };
    let not_joined = || vec![observed(secrets, Standing::Differs, None)];
    assert_eq!(
        menu_line(
            &ran(all, not_joined(), vec![step("Secrets vault", "error")]),
            false
        ),
        line("System State: Secrets vault failed", None)
    );
    assert_eq!(
        menu_line(
            &ran(
                all,
                vec![observed(secrets, Standing::Failed, None)],
                vec![step("Secrets vault", "failed")]
            ),
            false
        ),
        line("System State: Secrets vault failed", None),
        "counted once with its row"
    );
    assert_eq!(
        menu_line(
            &ran(
                all,
                vec![observed(dock, Standing::Failed, None)],
                vec![step("Secrets vault", "error")]
            ),
            false
        ),
        line("System State: 2 failed", None)
    );
    assert_eq!(
        menu_line(
            &ran(
                all,
                vec![observed(fda, Standing::NeedsYou, None)],
                vec![step("Selected Mac settings", "error")]
            ),
            false
        ),
        line("System State: 1 needs you, 1 failed", None)
    );
    assert_eq!(
        menu_line(
            &ran(
                all,
                Vec::new(),
                vec![step(
                    "Caps Lock acts as Control now on keyboard 05ac-0342",
                    "error"
                )]
            ),
            false
        ),
        line("System State: 1 step failed", None),
        "a step no row names is counted, not named"
    );
    assert_eq!(
        menu_line(
            &ran(
                all,
                Vec::new(),
                vec![
                    step("Selected Mac settings", "error"),
                    step("aterm permissions", "error")
                ]
            ),
            false
        ),
        line("System State: 2 failed", None)
    );
    let mut left = all;
    left.set(Setting::Secrets, false);
    assert_eq!(
        menu_line(
            &ran(left, not_joined(), vec![step("Secrets vault", "error")]),
            false
        ),
        None,
        "the vault is left as is"
    );
    for quiet in [
        "blocked",
        "waiting",
        "not started",
        "unverified",
        "restored",
        "ok",
    ] {
        assert_eq!(
            menu_line(
                &ran(all, not_joined(), vec![step("Secrets vault", quiet)]),
                false
            ),
            None,
            "{quiet}"
        );
    }
}

/// A failure the run still retries by itself (a stage's error, a failed pass or row) stays off
/// the menu line until the worker stops retrying it; a row that needs a person, or a verified
/// grant that reads off, is said at once, since no pass retries it.
#[test]
fn a_failure_the_run_still_retries_stays_off_the_menu_line() {
    let secrets = Subject::Setting(Setting::Secrets);
    let dock = Subject::Setting(Setting::Dock);
    let fda = Subject::Access(Access::FullDiskAccess);
    let step = |name: &str, status: &str| crate::SetupStatusRow {
        name: name.into(),
        status: status.into(),
        detail: "DETAIL".into(),
    };
    let ran =
        |observed: Vec<Observation>, rows: Vec<crate::SetupStatusRow>, retrying| SetupStatus {
            rows,
            retrying,
            ..status(Selection::all(), observed)
        };
    for (observed, rows, said) in [
        (
            vec![observed(secrets, Standing::Differs, None)],
            vec![step("Secrets vault", "error")],
            "System State: Secrets vault failed",
        ),
        (
            Vec::new(),
            vec![step("Setup", "failed")],
            "System State: 1 step failed",
        ),
        (
            vec![observed(dock, Standing::Failed, None)],
            Vec::new(),
            "System State: Dock failed",
        ),
    ] {
        assert_eq!(
            menu_line(&ran(observed.clone(), rows.clone(), true), false),
            None,
            "{said}: retried"
        );
        assert_eq!(
            menu_line(&ran(observed, rows, false), false),
            line(said, None),
            "no longer retried"
        );
    }
    let person = vec![
        observed(fda, Standing::NeedsYou, None),
        observed(dock, Standing::Failed, None),
    ];
    let failed = vec![step("Secrets vault", "error")];
    assert_eq!(
        menu_line(&ran(person.clone(), failed.clone(), true), false),
        line("System State: Full Disk Access needs you", None)
    );
    assert_eq!(
        menu_line(&ran(person, failed.clone(), false), false),
        line("System State: 1 needs you, 2 failed", None)
    );
    assert_eq!(
        menu_line(&ran(Vec::new(), failed, true), true),
        line("System State: Full Disk Access needs you", None),
        "a grant that reads off"
    );
}

thread_local! {
    static TRIGGERS: RefCell<u32> = const { RefCell::new(0) };
    static RECORDS: RefCell<Vec<(u64, Access, AccessOutcome)>> = const { RefCell::new(Vec::new()) };
    static DIALOG_AVAILABLE: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
}

fn triggers() -> u32 {
    TRIGGERS.with(|t| *t.borrow())
}

fn records() -> Vec<(u64, Access, AccessOutcome)> {
    RECORDS.with(|r| r.borrow().clone())
}

fn note_dialog_available() {
    let available = aterm_types::macos_setup::MacosDialog::when_none_up(Duration::ZERO).is_some();
    DIALOG_AVAILABLE.with(|read| read.borrow_mut().push(available));
}

static DIALOG_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Steps whose trigger and record are counted on this thread, with System State
/// frontmost and a quiet moment.
fn stepping() -> StepsReach {
    StepsReach {
        trigger_local_network: || {
            TRIGGERS.with(|t| *t.borrow_mut() += 1);
            true
        },
        record: |revision, access, outcome| {
            RECORDS.with(|r| r.borrow_mut().push((revision, access, outcome)));
            Ok(())
        },
        recorded: |_, _| None,
        frontmost: |_| true,
        quiet: |_, _| true,
        ..StepsReach::INERT
    }
}

fn approve(app: &mut App, revision: u64) {
    app.observe_initial_setup_permissions(Some(InitialSetupPermissionPolicy {
        revision,
        allowed: true,
    }));
}

/// Give every window of `app` a stand-in OS surface with keyboard focus, or take
/// it away (`App::any_os_window_focused`).
fn focus(app: &mut App, focused: bool) {
    for window in app.windows.values_mut() {
        window.os_window_for_test = focused;
        window.focused = focused;
    }
}

/// A verified grant is also a recovery fact when Stop holds Permissions: an
/// already-requested Restore still finishes, without a made-up access record.
#[test]
fn a_verified_fda_grant_recovers_a_waiting_restore_with_permissions_held() {
    use aterm_containment::consent::{FdaProbe, FdaState, ProbeLabel};
    use aterm_types::macos_setup::{PutBack, RestoreOutcome};

    thread_local! {
        static RECOVERY: RefCell<(bool, u32, u32)> = const { RefCell::new((false, 0, 0)) };
    }

    fn app() -> App {
        RECOVERY.with(|state| *state.borrow_mut() = (false, 0, 0));
        let mut app = App::headless_for_test();
        app.access_steps.set_reach(stepping());
        app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(FdaProbe {
            state: FdaState::Granted,
            label: ProbeLabel::OpenOk,
        });
        app.set_system_state_handlers_for_test(crate::InitialSetupHandlers {
            fda_granted: || {
                RECOVERY.with(|state| {
                    let mut state = state.borrow_mut();
                    state.1 += 1;
                    if std::mem::take(&mut state.0) {
                        state.2 += 1;
                    }
                });
            },
            ..crate::InitialSetupHandlers::inert_for_test()
        });
        app
    }

    let now = Instant::now();
    let before = records().len();
    let mut nothing_owed = app();
    nothing_owed.note_macos_access_granted(now);
    assert_eq!(RECOVERY.with(|state| state.borrow().2), 0);
    assert_eq!(records().len(), before, "no invented access revision");

    #[cfg(target_os = "macos")]
    {
        let mut foreign = app();
        foreign
            .consent
            .set_responsible_for_test(|pids| pids.iter().map(|_| Ok(1)).collect());
        RECOVERY.with(|state| state.borrow_mut().0 = true);
        foreign.note_macos_access_granted(now);
        assert_eq!(RECOVERY.with(|state| *state.borrow()), (true, 0, 0));
        assert_eq!(
            records().len(),
            before,
            "a parent's grant is no FDA fact here"
        );
    }

    // An approved grant keeps its existing record/append route: one line for
    // the outcome, without also starting the no-revision fallback.
    let mut approved = app();
    approve(&mut approved, 8_310);
    approved.note_macos_access_granted(now);
    approved.note_macos_access_granted(now);
    assert_eq!(
        &records()[before..],
        [(8_310, Access::FullDiskAccess, AccessOutcome::Granted)]
    );
    assert_eq!(RECOVERY.with(|state| state.borrow().1), 0);

    let mut held = app();
    approve(&mut held, 8_311);
    held.stop_initial_setup_permissions();
    let waiting = crate::system_state::restore_rows(&RestoreOutcome::of([(
        Some(Setting::Widgets),
        PutBack::Waits,
    )]));
    held.publish_system_state(SetupStatus {
        plan: Some(Plan::default()),
        rows: waiting,
        ..SetupStatus::default()
    });
    RECOVERY.with(|state| state.borrow_mut().0 = true);
    let before = records().len();
    held.resolve_fda_need(false, now);
    assert_eq!(RECOVERY.with(|state| *state.borrow()), (true, 0, 0));
    held.note_macos_access_granted(now);
    assert_eq!(
        RECOVERY.with(|state| state.borrow().2),
        1,
        "the verified grant must reach the waiting Restore despite Stop"
    );
    assert!(!RECOVERY.with(|state| state.borrow().0));
    assert_eq!(records().len(), before, "a completed fact is no admission");
    assert!(held.access_revision().is_none());
    assert!(held.admitted_access().is_none());
}

/// The Local Network step: nothing before an approved revision; then once per revision, first,
/// recorded `triggered`, its alert counted as up for its hold (no later step meanwhile); a new
/// revision asks again.
#[test]
fn the_local_network_step_runs_first_and_once_per_approved_revision() {
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(stepping());
    let now = Instant::now();
    let (fired, recorded) = (triggers(), records().len());

    app.tick_local_network_step(now);
    assert_eq!(triggers(), fired, "no approved revision");
    assert!(!app.access_step_moment(now));
    assert_eq!(app.system_state_access_step(), None);

    approve(&mut app, 3);
    assert_eq!(app.access_revision(), Some(3));
    assert_eq!(
        app.system_state_access_step(),
        Some(Access::LocalNetwork),
        "about to start"
    );
    assert!(!app.access_step_moment(now), "Local Network goes first");
    app.tick_local_network_step(now);
    assert_eq!(triggers(), fired + 1);
    assert_eq!(
        records()[recorded..],
        [(3, Access::LocalNetwork, AccessOutcome::Triggered)]
    );
    assert_eq!(app.system_state_access_step(), Some(Access::LocalNetwork));
    // The host fills the step into each publication (the worker's status
    // arrives about once a second), so the page's headline names it.
    app.publish_system_state(crate::system_state::SetupStatus::default());
    assert_eq!(
        app.published_system_state_access_step(),
        Some(Access::LocalNetwork),
        "the page is told which step is up"
    );
    assert!(!app.access_step_moment(now), "its alert is up");
    assert_eq!(
        app.next_access_step_deadline(now),
        Some(now + LOCAL_NETWORK_ALERT_HOLD)
    );

    let later = now + LOCAL_NETWORK_ALERT_HOLD + Duration::from_secs(1);
    app.tick_local_network_step(later);
    assert_eq!(triggers(), fired + 1, "once per revision");
    assert!(app.access_step_moment(later), "the next step's moment");
    assert_eq!(app.next_access_step_deadline(later), None);

    approve(&mut app, 4);
    app.tick_local_network_step(later);
    assert_eq!(triggers(), fired + 2, "a new revision asks again");
}

/// Once macOS's step started, a failed record write is still owed after the disk recovers:
/// normal turns save that same outcome without starting another macOS step.
#[test]
fn a_local_network_step_recovers_its_record_without_triggering_again() {
    use aterm_types::macos_setup::access_record;

    struct Writes {
        dir: std::path::PathBuf,
        attempted: usize,
        failed: usize,
    }
    thread_local! {
        static WRITES: RefCell<Option<Writes>> = const { RefCell::new(None) };
    }
    let root = aterm_tempfile::tempdir().expect("scratch access record");
    let revision = 8_210;
    access_record::append(
        root.path(),
        revision - 1,
        Access::LocalNetwork,
        AccessOutcome::Triggered,
    )
    .unwrap();
    access_record::append(
        root.path(),
        revision,
        Access::DeveloperTools,
        AccessOutcome::Granted,
    )
    .unwrap();
    let record = root.path().join(access_record::FILE);
    let retained = root.path().join("retained-access-record");
    std::fs::rename(&record, &retained).unwrap();
    std::fs::create_dir(&record).unwrap();
    WRITES.with(|writes| {
        *writes.borrow_mut() = Some(Writes {
            dir: root.path().to_owned(),
            attempted: 0,
            failed: 0,
        });
    });
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        record: |revision, access, outcome| {
            let dir = WRITES.with(|writes| writes.borrow().as_ref().unwrap().dir.clone());
            let saved = access_record::append(&dir, revision, access, outcome);
            WRITES.with(|writes| {
                let mut writes = writes.borrow_mut();
                let writes = writes.as_mut().unwrap();
                writes.attempted += 1;
                writes.failed += usize::from(saved.is_err());
            });
            saved
        },
        recorded: |revision, access| {
            let dir = WRITES.with(|writes| writes.borrow().as_ref().unwrap().dir.clone());
            access_record::latest(&dir, revision, access)
        },
        ..stepping()
    });
    let now = Instant::now();
    let fired = triggers();
    approve(&mut app, revision);
    app.tick_local_network_step(now);
    assert_eq!(triggers(), fired + 1);
    WRITES.with(|writes| {
        let writes = writes.borrow();
        let writes = writes.as_ref().unwrap();
        assert_eq!((writes.attempted, writes.failed), (1, 1));
    });
    assert_eq!(
        access_record::latest(root.path(), revision, Access::LocalNetwork),
        None
    );
    std::fs::remove_dir(&record).unwrap();
    std::fs::rename(&retained, &record).unwrap();

    let later = now + LOCAL_NETWORK_ALERT_HOLD + super::LOCAL_NETWORK_RETRY;
    app.tick_local_network_step(later);
    app.settle_access_steps(later);
    app.tick_local_network_step(later + super::LOCAL_NETWORK_RETRY);
    app.settle_access_steps(later + super::LOCAL_NETWORK_RETRY);
    assert_eq!(triggers(), fired + 1, "only the record is retried");
    assert_eq!(
        access_record::latest(root.path(), revision, Access::LocalNetwork),
        Some(AccessOutcome::Triggered)
    );
    assert_eq!(
        access_record::read(root.path(), revision),
        [
            (Access::DeveloperTools, AccessOutcome::Granted),
            (Access::LocalNetwork, AccessOutcome::Triggered),
        ]
    );
    assert_eq!(
        access_record::read(root.path(), revision - 1),
        [(Access::LocalNetwork, AccessOutcome::Triggered)]
    );
    WRITES.with(|writes| *writes.borrow_mut() = None);
}

/// A completed debugger change is not repeated while its outcome waits for
/// writable history, including after its page closes and Permissions is held.
#[test]
fn a_completed_debugger_step_recovers_its_record_after_the_page_closes() {
    use crate::debugger_access::{DebuggerStepQueue, Outcome};
    use aterm_containment::consent::DebuggerChange;
    use aterm_types::macos_setup::access_record;

    let root = aterm_tempfile::tempdir().unwrap();
    let revision = 8_220;
    access_record::append(
        root.path(),
        revision - 1,
        Access::Debugger,
        AccessOutcome::Granted,
    )
    .unwrap();
    access_record::append(
        root.path(),
        revision,
        Access::LocalNetwork,
        AccessOutcome::Triggered,
    )
    .unwrap();
    access_record::append(
        root.path(),
        revision,
        Access::Debugger,
        AccessOutcome::Handed,
    )
    .unwrap();
    let earlier = access_record::read(root.path(), revision - 1);
    let record = root.path().join(access_record::FILE);
    let retained = root.path().join("retained-access-record");
    std::fs::rename(&record, &retained).unwrap();
    std::fs::create_dir(&record).unwrap();

    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach::INERT);
    app.debugger_launch = DebuggerStepQueue::queued_for_test(|| Some("MAC-RECORD-RETRY"))
        .recording_in(Some(root.path().to_owned()));
    approve(&mut app, revision);
    assert!(app.open_system_state_tab());
    let wid = app.frontmost_window.unwrap();
    let (_, view) = app.active_native_view(wid).unwrap();
    app.debugger_launch.note_started(revision);
    app.debugger_launch.end();
    app.debugger_launch.record(revision, AccessOutcome::Handed);
    app.debugger_launch.record_written_for_test();
    app.note_system_state_debugger_outcome(Outcome::NotChanged(DebuggerChange::On));
    app.debugger_launch.record_written_for_test();
    assert_eq!(app.debugger_launch.started(), None);
    assert!(!app.debugger_launch.queued());
    assert_eq!(
        access_record::latest(root.path(), revision, Access::Debugger),
        None,
        "the completed outcome could not be saved"
    );

    std::fs::remove_dir(&record).unwrap();
    std::fs::rename(&retained, &record).unwrap();
    assert_eq!(
        access_record::latest(root.path(), revision, Access::Debugger),
        Some(AccessOutcome::Handed)
    );
    app.close_active_native_tab(wid).unwrap();
    assert!(app.native_runtime.view_state(view).is_none());
    app.hold_initial_setup_permissions();
    app.config.privacy = Some(crate::app_config::PrivacyConfig {
        enabled: Some(false),
        ..Default::default()
    });
    assert_eq!(app.access_revision(), None);
    let mut now = Instant::now();
    assert!(
        app.next_access_step_deadline(now).is_some(),
        "the completed outcome keeps its save deadline without a page or permission effects"
    );
    for _ in 0..3 {
        let Some(deadline) = app.next_access_step_deadline(now) else {
            break;
        };
        now = deadline;
        app.settle_access_steps(now);
        app.debugger_launch.record_written_for_test();
    }
    assert_eq!(
        access_record::latest(root.path(), revision, Access::Debugger),
        Some(AccessOutcome::Declined),
        "normal settlement saves the completed change without another macOS step"
    );
    assert_eq!(
        access_record::latest(root.path(), revision, Access::LocalNetwork),
        Some(AccessOutcome::Triggered)
    );
    assert_eq!(access_record::read(root.path(), revision - 1), earlier);
    assert!(!app.debugger_launch.queued());
    assert_eq!(app.debugger_launch.started(), None);
    let saved = access_record::read(root.path(), revision);
    app.note_system_state_debugger_outcome(Outcome::NotChanged(DebuggerChange::On));
    app.settle_access_steps(now + super::ACCESS_RECORD_RETRY);
    app.debugger_launch.record_written_for_test();
    assert_eq!(access_record::read(root.path(), revision), saved);
    assert_eq!(app.next_access_step_deadline(now), None);
}

/// The worker cannot acquire the shared dialog slot in either the trigger or
/// its record's wake. The slot stays owned for the alert hold, a failed trigger
/// releases it for the next step, and an inert headless lane never takes it.
#[test]
fn the_local_network_trigger_and_record_own_the_dialog_slot() {
    use aterm_types::macos_setup::{MacosDialog, macos_dialog_up};

    let _serial = DIALOG_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Our dialog fixtures share this guard, but no callback opens anything
    // outside the process.
    assert!(!macos_dialog_up());
    let now = Instant::now();
    let reach = StepsReach {
        trigger_local_network: || {
            note_dialog_available();
            true
        },
        record: |_, _, _| {
            note_dialog_available();
            Ok(())
        },
        dialog_up: macos_dialog_up,
        holds_dialog: true,
        ..stepping()
    };
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(reach);
    approve(&mut app, 41);
    DIALOG_AVAILABLE.with(|read| read.borrow_mut().clear());
    let worker = MacosDialog::when_none_up(Duration::ZERO).expect("the worker takes the slot");
    app.tick_local_network_step(now);
    assert!(DIALOG_AVAILABLE.with(|read| read.borrow().is_empty()));
    assert!(app.access_steps.dialog.is_none(), "the worker owns it");
    drop(worker);
    app.tick_local_network_step(now);
    assert_eq!(
        DIALOG_AVAILABLE.with(|read| read.borrow().clone()),
        [false, false],
        "the worker cannot start a prompt from the trigger or record wake"
    );
    assert!(app.access_steps.dialog.is_some());
    assert!(MacosDialog::when_none_up(Duration::ZERO).is_none());
    app.settle_access_steps(now);
    assert!(macos_dialog_up(), "end-of-turn bookkeeping keeps ownership");
    let later = now + LOCAL_NETWORK_ALERT_HOLD;
    app.tick_local_network_step(later);
    app.settle_access_steps(later);
    assert!(!macos_dialog_up(), "the expired hold releases ownership");
    drop(app);

    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        trigger_local_network: || {
            note_dialog_available();
            false
        },
        ..reach
    });
    approve(&mut app, 42);
    DIALOG_AVAILABLE.with(|read| read.borrow_mut().clear());
    app.tick_local_network_step(now);
    assert_eq!(DIALOG_AVAILABLE.with(|read| read.borrow().clone()), [false]);
    assert!(app.access_steps.dialog.is_none());
    assert!(
        !macos_dialog_up(),
        "nothing was raised by the failed trigger"
    );
    assert!(app.access_steps.local_network_retry.is_some());
    drop(app);

    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        holds_dialog: false,
        ..reach
    });
    approve(&mut app, 43);
    DIALOG_AVAILABLE.with(|read| read.borrow_mut().clear());
    app.tick_local_network_step(now);
    assert_eq!(
        DIALOG_AVAILABLE.with(|read| read.borrow().clone()),
        [true, true],
        "the inert lane takes no process-wide slot"
    );
    assert!(app.access_steps.dialog.is_none());
    drop(app);
    assert!(!macos_dialog_up());
}

#[cfg(target_os = "macos")]
fn check_handoff_dialog_slot(page: crate::devtools_helper::Page) {
    use crate::devtools_helper::{HandOver, Origin, test_arms};
    use aterm_types::macos_setup::{MacosDialog, macos_dialog_up};

    let _serial = DIALOG_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(!macos_dialog_up());
    let now = Instant::now();
    for opened in [true, false] {
        let open: fn() -> bool = if opened {
            || {
                note_dialog_available();
                true
            }
        } else {
            || {
                note_dialog_available();
                false
            }
        };
        let mut reach = test_arms::opening();
        reach.open_pane = open;
        let mut fda = test_arms::fda_opening();
        fda.open_pane = open;
        let mut app = new_mac(reach, fda);
        app.access_steps.set_reach(StepsReach {
            record: |_, _, _| {
                note_dialog_available();
                Ok(())
            },
            dialog_up: macos_dialog_up,
            holds_dialog: true,
            ..stepping()
        });
        DIALOG_AVAILABLE.with(|read| read.borrow_mut().clear());
        let answer = match page {
            crate::devtools_helper::Page::FullDiskAccess => {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the inert pane callback tests ownership before opening and recording"
                )]
                let answer = app.open_fda_page(Origin::Settings, true, now);
                answer
            }
            crate::devtools_helper::Page::DeveloperTools => {
                app.open_devtools_page(Origin::Settings, None, true, now)
            }
        };
        if opened {
            assert_eq!(answer, HandOver::Opened);
            assert_eq!(
                DIALOG_AVAILABLE.with(|read| read.borrow().clone()),
                [false, false],
                "the worker cannot start a prompt while the pane opens or its record wakes it"
            );
            assert!(MacosDialog::when_none_up(Duration::ZERO).is_none());
            app.settle_access_steps(now);
            assert!(macos_dialog_up(), "the helper retains the reservation");
            app.devtools_helper
                .press_for_test(crate::devtools_helper::PanelEvent::NotNow);
            app.tick_devtools_hand_over(now);
            app.settle_access_steps(now);
            assert!(!macos_dialog_up(), "Not now releases the helper's slot");
        } else {
            assert_eq!(answer, HandOver::OpenFailed);
            assert_eq!(DIALOG_AVAILABLE.with(|read| read.borrow().clone()), [false]);
            assert!(!macos_dialog_up(), "a failed pane leaves no reservation");
        }
        drop(app);
        assert!(!macos_dialog_up(), "dropping the app releases its helper");
    }

    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    app.access_steps.set_reach(StepsReach {
        holds_dialog: true,
        ..stepping()
    });
    let worker = MacosDialog::when_none_up(Duration::ZERO).unwrap();
    let counts = (
        test_arms::opens(),
        test_arms::opened_notes(),
        test_arms::fda_counts(),
    );
    for origin in [
        Origin::Automatic,
        Origin::SystemStateAct,
        Origin::Ctl { now: false },
        Origin::Ctl { now: true },
    ] {
        let answer = match page {
            crate::devtools_helper::Page::FullDiskAccess => {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the inert guarded door tests a worker winning after the moment check"
                )]
                let answer = app.open_fda_page(origin, true, now);
                answer
            }
            crate::devtools_helper::Page::DeveloperTools => {
                app.open_devtools_page(origin, Some(Ok(())), true, now)
            }
        };
        assert_eq!(answer, HandOver::Queued, "{origin:?}");
    }
    assert_eq!(
        (
            test_arms::opens(),
            test_arms::opened_notes(),
            test_arms::fda_counts()
        ),
        counts
    );
    assert!(app.access_steps.dialog.is_none());
    drop(worker);
    let answer = match page {
        crate::devtools_helper::Page::FullDiskAccess => {
            #[expect(
                clippy::disallowed_methods,
                reason = "the inert guarded door retries after the worker releases its slot"
            )]
            let answer = app.open_fda_page(Origin::Automatic, true, now);
            answer
        }
        crate::devtools_helper::Page::DeveloperTools => {
            app.open_devtools_page(Origin::Automatic, Some(Ok(())), true, now)
        }
    };
    assert_eq!(answer, HandOver::Opened);
    assert!(app.devtools_helper.panel().is_some());
    drop(app);
    assert!(!macos_dialog_up());

    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    app.access_steps.set_reach(StepsReach {
        holds_dialog: true,
        ..stepping()
    });
    let worker = MacosDialog::when_none_up(Duration::ZERO).unwrap();
    let answer = match page {
        crate::devtools_helper::Page::FullDiskAccess => {
            #[expect(
                clippy::disallowed_methods,
                reason = "the inert pane tests the explicit owner's own dialog reservation"
            )]
            let answer = app.open_fda_page(Origin::Settings, true, now);
            answer
        }
        crate::devtools_helper::Page::DeveloperTools => {
            app.open_devtools_page(Origin::Digest, None, true, now)
        }
    };
    assert_eq!(
        answer,
        HandOver::Opened,
        "the person's own press stays immediate"
    );
    assert!(app.access_steps.dialog.is_some());
    drop(worker);
    assert!(
        macos_dialog_up(),
        "the person's page keeps its own guard after the worker ends"
    );
    drop(app);
    assert!(!macos_dialog_up());
}

#[cfg(target_os = "macos")]
#[test]
fn full_disk_access_handoff_dialog_slot_is_owned_before_open_and_record() {
    check_handoff_dialog_slot(crate::devtools_helper::Page::FullDiskAccess);
}

#[cfg(target_os = "macos")]
#[test]
fn developer_tools_handoff_dialog_slot_is_owned_before_open_and_record() {
    check_handoff_dialog_slot(crate::devtools_helper::Page::DeveloperTools);
}

#[cfg(target_os = "macos")]
#[test]
fn a_security_pages_person_press_transfers_its_dialog_before_recording() {
    use crate::devtools_helper::test_arms;
    use crate::native_app::{ActionInvocation, AppEvent, AppViewState};
    use crate::native_settings::{ConsentGestures, MACOS_ACCESS_OPEN_FDA, SettingsRoute};
    use crate::native_ui::ActionId;
    use aterm_types::macos_setup::macos_dialog_up;

    let _serial = DIALOG_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    app.access_steps.set_reach(StepsReach {
        record: |_, _, _| {
            note_dialog_available();
            Ok(())
        },
        holds_dialog: true,
        ..stepping()
    });
    assert!(app.open_settings_tab(SettingsRoute::Security));
    let wid = crate::WindowId(0);
    let (_, view) = app.active_native_view(wid).unwrap();
    let Some(AppViewState::Settings(state)) = app.native_runtime.view_state_mut(view) else {
        panic!("Settings view");
    };
    state.arm_consent_gestures(ConsentGestures::guarded_pane_for_test(|_| {
        note_dialog_available();
        crate::menu::SettingsOpen::Anchored
    }));
    DIALOG_AVAILABLE.with(|read| read.borrow_mut().clear());
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::Action(ActionInvocation {
            id: ActionId::new(MACOS_ACCESS_OPEN_FDA),
            value: None,
        }),
    )
    .unwrap();
    assert!(
        macos_dialog_up(),
        "the view owns the open page before the host drains it"
    );
    app.sync_settings_consent_posture();
    assert_eq!(
        DIALOG_AVAILABLE.with(|read| read.borrow().clone()),
        [false, false]
    );
    assert!(app.fda_panel_up());
    assert!(macos_dialog_up(), "the host now owns the helper's guard");
    drop(app);
    assert!(!macos_dialog_up());

    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    app.access_steps.set_reach(StepsReach {
        holds_dialog: true,
        ..stepping()
    });
    assert!(app.open_settings_tab(SettingsRoute::Security));
    let (_, view) = app.active_native_view(wid).unwrap();
    let Some(AppViewState::Settings(state)) = app.native_runtime.view_state_mut(view) else {
        panic!("Settings view");
    };
    state.arm_consent_gestures(ConsentGestures::guarded_pane_for_test(|_| {
        crate::menu::SettingsOpen::Anchored
    }));
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::Action(ActionInvocation {
            id: ActionId::new(MACOS_ACCESS_OPEN_FDA),
            value: None,
        }),
    )
    .unwrap();
    assert!(
        app.access_steps.dialog.is_some(),
        "dispatch transferred the guard before any park"
    );
    app.close_active_native_tab(wid).unwrap();
    assert!(app.native_runtime.view_state(view).is_none());
    let now = Instant::now();
    app.settle_access_steps(now);
    assert!(
        macos_dialog_up(),
        "closing the view cannot lose its already-open pane's hold"
    );
    assert!(
        app.devtools_helper.panel().is_none(),
        "the ordinary notification has not drained"
    );
    app.tick_local_network_step(now + super::PANE_HOLD);
    app.settle_access_steps(now + super::PANE_HOLD);
    assert!(!macos_dialog_up(), "the pane's bounded hold still ends");
    drop(app);

    let (_, dialog) = ConsentGestures::guarded_pane_for_test(|_| {
        note_dialog_available();
        crate::menu::SettingsOpen::Refused
    })
    .open_settings(crate::menu::PrivacyPane::FullDiskAccess);
    assert!(dialog.is_none());
    assert!(
        !macos_dialog_up(),
        "a failed person gesture releases its guard"
    );
}

fn show_accessibility_needs_you(app: &mut App) {
    assert!(app.open_system_state_tab());
    app.publish_system_state(status(
        Selection::all(),
        vec![observed(
            Subject::Access(Access::Accessibility),
            Standing::NeedsYou,
            None,
        )],
    ));
}

#[test]
fn queued_access_panes_own_the_dialog_slot_before_opening() {
    use aterm_types::macos_setup::{MacosDialog, macos_dialog_up};
    let _serial = DIALOG_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    for opened in [true, false] {
        let mut app = App::headless_for_test();
        app.headless = false;
        app.access_steps.set_reach(StepsReach {
            open_pane: if opened {
                |_, _| {
                    note_dialog_available();
                    true
                }
            } else {
                |_, _| {
                    note_dialog_available();
                    false
                }
            },
            holds_dialog: true,
            ..stepping()
        });
        approve(&mut app, 1);
        app.access_steps.local_network_done = Some(1);
        show_accessibility_needs_you(&mut app);
        app.access_steps.pane_act = Some((Access::Accessibility, now));
        let worker = MacosDialog::when_none_up(Duration::ZERO).unwrap();
        DIALOG_AVAILABLE.with(|read| read.borrow_mut().clear());
        // The injected moment deliberately misses a worker that wins after it;
        // the atomic reservation must retain the queued act.
        app.tick_queued_pane(now);
        assert!(app.access_steps.pane_act.is_some());
        assert!(DIALOG_AVAILABLE.with(|read| read.borrow().is_empty()));
        drop(worker);
        app.tick_queued_pane(now);
        assert!(app.access_steps.pane_act.is_none());
        assert_eq!(DIALOG_AVAILABLE.with(|read| read.borrow().clone()), [false]);
        assert_eq!(macos_dialog_up(), opened);
        if opened {
            app.tick_local_network_step(now + super::PANE_HOLD);
            app.settle_access_steps(now + super::PANE_HOLD);
            assert!(!macos_dialog_up(), "the pane's bounded hold ended");
        }
        drop(app);
        assert!(!macos_dialog_up());
    }

    for access in [Access::LocalNetwork, Access::Accessibility] {
        let mut app = App::headless_for_test();
        app.headless = false;
        app.access_steps.set_reach(StepsReach {
            open_pane: |_, _| true,
            holds_dialog: true,
            ..stepping()
        });
        let worker = MacosDialog::when_none_up(Duration::ZERO).unwrap();
        assert!(
            app.open_access_from_system_state(access, true),
            "a person's own press stays immediate"
        );
        assert!(app.access_steps.dialog.is_some());
        drop(worker);
        assert!(macos_dialog_up(), "the person's pane retains its own guard");
        drop(app);
        assert!(!macos_dialog_up());
    }
}

#[cfg(target_os = "macos")]
static FOLDER_SLOT: std::sync::Mutex<Option<std::sync::mpsc::Sender<bool>>> =
    std::sync::Mutex::new(None);

#[cfg(target_os = "macos")]
#[test]
fn folder_worker_owns_the_dialog_slot_before_its_first_probe() {
    use crate::consent_warmup::{ReadDirOutcome, StartOutcome, WarmupState};
    use aterm_types::macos_setup::{MacosDialog, macos_dialog_up};
    let _serial = DIALOG_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = App::headless_for_test();
    app.consent_warmup = WarmupState::with_read_dir_for_test(|_| {
        let available = MacosDialog::when_none_up(Duration::ZERO).is_some();
        FOLDER_SLOT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap()
            .send(available)
            .unwrap();
        ReadDirOutcome::Refused
    });
    app.access_steps.set_reach(StepsReach {
        holds_dialog: true,
        ..stepping()
    });
    let folders = [(aterm_containment::Folder::Documents, "/inert-folder".into())];
    let (send, recv) = std::sync::mpsc::channel();
    *FOLDER_SLOT.lock().unwrap() = Some(send);
    let worker = MacosDialog::when_none_up(Duration::ZERO).unwrap();
    assert_eq!(
        app.start_consent_warmup_pass(&folders, None),
        StartOutcome::DialogUp
    );
    assert!(!app.consent_warmup.is_live());
    drop(worker);
    assert_eq!(
        app.start_consent_warmup_pass(&folders, None),
        StartOutcome::Started
    );
    assert!(!recv.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap());
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while app.consent_warmup_live() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(!app.consent_warmup.is_live(), "the inert pass completed");
    app.settle_access_steps(Instant::now());
    assert!(!macos_dialog_up(), "a completed pass releases the slot");
    assert_eq!(
        app.start_consent_warmup_pass(&[], None),
        StartOutcome::NoFolders
    );
    assert!(
        !macos_dialog_up(),
        "a pass that starts nothing releases it too"
    );
}

/// Recorded already (an earlier process of this revision): not asked again. Not
/// frontmost, or a `[privacy]` switch off: not asked at all.
#[test]
fn the_local_network_step_waits_for_its_moment_and_honours_the_opt_outs() {
    let now = Instant::now();
    let fired = triggers();

    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        recorded: |_, _| Some(AccessOutcome::Triggered),
        ..stepping()
    });
    approve(&mut app, 5);
    app.tick_local_network_step(now);
    assert_eq!(triggers(), fired, "on record for this revision");
    assert!(app.access_step_moment(now));

    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        frontmost: |_| false,
        ..stepping()
    });
    approve(&mut app, 5);
    app.tick_local_network_step(now);
    assert_eq!(triggers(), fired, "System State is not frontmost");
    assert_eq!(app.system_state_access_step(), None);

    for privacy in [
        crate::app_config::PrivacyConfig {
            enabled: Some(false),
            ..Default::default()
        },
        crate::app_config::PrivacyConfig {
            check: Some(false),
            ..Default::default()
        },
        crate::app_config::PrivacyConfig {
            notice: Some(false),
            ..Default::default()
        },
    ] {
        let mut app = App::headless_for_test();
        app.access_steps.set_reach(stepping());
        app.config.privacy = Some(privacy);
        approve(&mut app, 5);
        app.tick_local_network_step(now);
        assert_eq!(triggers(), fired, "a switch is off");
        assert!(!app.access_step_moment(now));
    }

    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        dialog_up: || true,
        ..stepping()
    });
    approve(&mut app, 5);
    app.tick_local_network_step(now);
    assert_eq!(triggers(), fired, "another macOS dialog is up");
}

/// Outcomes are recorded under the approved revision only; an unchanged
/// reading is recorded once.
#[test]
fn an_outcome_is_recorded_once_under_the_approved_revision() {
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(stepping());
    let before = records().len();
    app.record_access_step(Access::FullDiskAccess, AccessOutcome::Granted);
    assert_eq!(records().len(), before, "no approved revision");
    approve(&mut app, 9);
    app.record_access_step(Access::FullDiskAccess, AccessOutcome::Granted);
    app.record_access_step(Access::FullDiskAccess, AccessOutcome::Granted);
    app.record_access_step(Access::DeveloperTools, AccessOutcome::Handed);
    assert_eq!(
        records()[before..],
        [
            (9, Access::FullDiskAccess, AccessOutcome::Granted),
            (9, Access::DeveloperTools, AccessOutcome::Handed),
        ]
    );
}

/// Pending writes keep only the latest outcome of their own row and revision. They have a
/// backoff and keep waking after the page closes or Permissions is held; retrying a history
/// write starts no macOS step.
#[test]
fn pending_access_records_keep_the_latest_outcome_and_retry_while_the_page_is_closed() {
    thread_local! {
        static WRITABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
        static ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        record: |revision, access, outcome| {
            ATTEMPTS.with(|attempts| attempts.set(attempts.get() + 1));
            if !WRITABLE.with(std::cell::Cell::get) {
                return Err(std::io::Error::other(
                    "injected access record write failure",
                ));
            }
            RECORDS.with(|records| records.borrow_mut().push((revision, access, outcome)));
            Ok(())
        },
        ..stepping()
    });
    let before = records().len();
    let fired = triggers();
    app.record_access_at(70, Access::Debugger, AccessOutcome::Granted);
    approve(&mut app, 71);
    assert!(app.open_system_state_tab());
    let wid = app.frontmost_window.unwrap();
    let (_, view) = app.active_native_view(wid).unwrap();

    WRITABLE.with(|writable| writable.set(false));
    app.record_access_at(71, Access::FullDiskAccess, AccessOutcome::Declined);
    app.record_access_at(72, Access::DeveloperTools, AccessOutcome::Declined);
    let failed = ATTEMPTS.with(std::cell::Cell::get);
    let due = app
        .next_access_step_deadline(Instant::now())
        .expect("a failed history write has its own wake");
    app.record_access_at(71, Access::FullDiskAccess, AccessOutcome::Declined);
    app.settle_access_steps(due - Duration::from_nanos(1));
    assert_eq!(
        ATTEMPTS.with(std::cell::Cell::get),
        failed,
        "unchanged callbacks and early turns respect the write backoff"
    );

    WRITABLE.with(|writable| writable.set(true));
    app.record_access_at(71, Access::FullDiskAccess, AccessOutcome::Granted);
    app.hold_initial_setup_permissions();
    app.config.privacy = Some(crate::app_config::PrivacyConfig {
        enabled: Some(false),
        ..Default::default()
    });
    app.close_active_native_tab(wid).unwrap();
    assert!(app.native_runtime.view_state(view).is_none());
    assert_eq!(app.access_revision(), None);
    let due = app
        .next_access_step_deadline(Instant::now())
        .expect("closing the page and holding Permissions do not discard its history");
    let attempts = ATTEMPTS.with(std::cell::Cell::get);
    app.settle_access_steps(due - Duration::from_nanos(1));
    assert_eq!(ATTEMPTS.with(std::cell::Cell::get), attempts);
    app.settle_access_steps(due);
    let recovered = records();
    assert_eq!(
        recovered[before..],
        [
            (70, Access::Debugger, AccessOutcome::Granted),
            (71, Access::FullDiskAccess, AccessOutcome::Granted),
            (72, Access::DeveloperTools, AccessOutcome::Declined),
        ]
    );
    assert_eq!(app.next_access_step_deadline(due), None);
    app.settle_access_steps(due + super::ACCESS_RECORD_RETRY);
    assert_eq!(records(), recovered, "saved outcomes are not replayed");
    assert_eq!(triggers(), fired, "history repair starts no macOS step");
}

/// A later Not now wins over a grant in the same approval. The helper's real
/// answer and grant callback reach the record port; unchanged callbacks, other
/// rows and an earlier approval cannot replace that latest answer.
#[cfg(target_os = "macos")]
#[test]
fn an_access_panels_second_decline_after_a_grant_is_the_current_outcome() {
    use crate::devtools_helper::{Page, PanelEvent, test_arms};
    use crate::native_app::{ActionInvocation, AppEvent, NativeApp};
    use crate::native_ui::ActionId;
    use aterm_containment::DevToolGrant;

    let now = Instant::now();
    let access = Access::DeveloperTools;
    let subject = Subject::Access(access);
    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    test_arms::stand_in(&mut app, test_arms::opening());
    app.access_steps.set_reach(stepping());
    set_front(true);
    assert!(app.open_system_state_tab());
    let wid = app.frontmost_window.unwrap();
    let (instance, _) = app.active_native_view(wid).unwrap();
    let before = records().len();
    let latest = |revision, row| {
        records()[before..]
            .iter()
            .rev()
            .find_map(|(at, access, outcome)| {
                (*at == revision && *access == row).then_some(*outcome)
            })
    };
    let decline = |app: &mut App| {
        // A current read is off after a grant was revoked. The real row offers
        // Open again, and the person's press takes the same reducer/host path.
        app.devtools_helper.note_reading(
            test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
            now,
        );
        let _ = app.devtools_watch.observe(DevToolGrant::NotGranted, true);
        app.publish_system_state(status(
            Selection::all(),
            vec![observed(subject, Standing::NeedsYou, None)],
        ));
        assert!(matches!(app.native_runtime.app(instance),
            Some(NativeApp::SystemState(page)) if page.can_open_access(access)));
        // Retire earlier control-key counts before this independent person press.
        let _ = app.take_system_state_press();
        app.dispatch_native_event(
            wid,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("system-state/row/developer-tools/open"),
                value: None,
            }),
        )
        .unwrap();
        assert_eq!(
            app.devtools_helper.panel().unwrap().page(),
            Page::DeveloperTools
        );
        app.devtools_helper.press_for_test(PanelEvent::NotNow);
        app.tick_devtools_hand_over(now);
        assert!(app.devtools_helper.panel().is_none());
        app.settle_access_steps(now);
    };
    let grant = |app: &mut App| {
        // The same callback as a completed list reading and the helper's
        // granted result: the watcher owes this transition's refresh once.
        let step = app.devtools_watch.observe(DevToolGrant::Granted, true);
        assert!(!app.apply_devtools_step(step));
        app.publish_system_state(status(
            Selection::all(),
            vec![observed(subject, Standing::InPlace, None)],
        ));
        let after = records();
        let step = app.devtools_watch.observe(DevToolGrant::Granted, true);
        assert!(!app.apply_devtools_step(step));
        app.settle_access_steps(now);
        assert_eq!(records(), after, "an unchanged grant is not recorded twice");
    };

    decline(&mut app);
    assert_eq!(latest(1, access), Some(AccessOutcome::Declined));
    let declined = records();
    app.settle_access_steps(now);
    assert_eq!(records(), declined, "a helper answer is taken once");
    grant(&mut app);
    assert_eq!(latest(1, access), Some(AccessOutcome::Granted));

    approve(&mut app, 2);
    decline(&mut app);
    assert_eq!(latest(2, access), Some(AccessOutcome::Declined));
    assert_eq!(latest(1, access), Some(AccessOutcome::Granted));
    grant(&mut app);
    assert_eq!(latest(2, access), Some(AccessOutcome::Granted));

    app.tick_local_network_step(now);
    assert_eq!(
        latest(2, Access::LocalNetwork),
        Some(AccessOutcome::Triggered)
    );
    let independent = records();
    app.tick_local_network_step(now);
    assert_eq!(
        records(),
        independent,
        "the independent trigger is taken once"
    );

    decline(&mut app);
    assert_eq!(
        latest(2, access),
        Some(AccessOutcome::Declined),
        "the latest Not now must replace the earlier grant in this approval"
    );
    assert_eq!(latest(1, access), Some(AccessOutcome::Granted));
    assert_eq!(
        latest(2, Access::LocalNetwork),
        Some(AccessOutcome::Triggered)
    );
    let declined = records();
    app.settle_access_steps(now);
    assert_eq!(records(), declined, "the new helper answer is taken once");

    // The same real helper answer remains owed if its record write fails.
    // Keep the earlier successful-history controls above, then fail only this
    // new same-approval decline after another grant.
    thread_local! {
        static FAIL_DECLINE: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
        static FAILED_DECLINES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    grant(&mut app);
    app.access_steps.set_reach(StepsReach {
        record: |revision, access, outcome| {
            if outcome == AccessOutcome::Declined && FAIL_DECLINE.with(std::cell::Cell::get) {
                FAILED_DECLINES.with(|failed| failed.set(failed.get() + 1));
                return Err(std::io::Error::other("injected decline write failure"));
            }
            RECORDS.with(|records| records.borrow_mut().push((revision, access, outcome)));
            Ok(())
        },
        ..stepping()
    });
    decline(&mut app);
    assert_eq!(FAILED_DECLINES.with(std::cell::Cell::get), 1);
    assert_eq!(latest(2, access), Some(AccessOutcome::Handed));
    assert!(app.devtools_helper.panel().is_none());
    let opened = test_arms::opens();
    let fired = triggers();
    let due = app
        .next_access_step_deadline(Instant::now())
        .expect("the taken helper answer still needs its record");
    FAIL_DECLINE.with(|fail| fail.set(false));
    app.settle_access_steps(due);
    assert_eq!(latest(2, access), Some(AccessOutcome::Declined));
    assert_eq!(latest(1, access), Some(AccessOutcome::Granted));
    assert_eq!(
        latest(2, Access::LocalNetwork),
        Some(AccessOutcome::Triggered)
    );
    let recovered = records();
    app.settle_access_steps(due + super::ACCESS_RECORD_RETRY);
    assert_eq!(records(), recovered);
    assert_eq!(test_arms::opens(), opened, "no new helper page");
    assert_eq!(triggers(), fired, "no new Local Network step");
}

/// The marker words bind to the approved revision; with none they keep the
/// running version, so `aterm doctor`'s own reads still match.
#[test]
fn the_markers_are_bound_to_the_approved_revision() {
    let mut app = App::headless_for_test();
    let marks = app.access_marks();
    assert_eq!(marks.revision, None);
    assert_eq!(marks.devtools, aterm_types::version::app_version());
    approve(&mut app, 12);
    let marks = app.access_marks();
    assert_eq!(marks.revision, Some(12));
    assert_eq!(marks.devtools, "r12");
    assert!(marks.fda.starts_with("r12"), "{}", marks.fda);
}

/// A regressed grant lights the menu line (and ends it when it is settled); the
/// retired digest keys end once, withdrawn: nothing read their causes end.
#[test]
fn a_regressed_grant_lights_the_menu_line() {
    use aterm_messages::exceptions::{Entry, Family};
    use aterm_messages::need::{Reason, Then, Why};
    let mut app = App::headless_for_test();
    let now = Instant::now();
    // What an older aterm left in the digest.
    app.except(&Entry::new(
        Family::Privacy,
        "fda",
        "needed",
        "aterm cannot read your files",
        Why::OnlyYou(Reason::new("only you can turn on Full Disk Access")),
        Then::step("Turn on Full Disk Access"),
    ));
    assert!(
        app.messages
            .digest()
            .entries
            .iter()
            .any(|e| e.key == "privacy.fda.needed"),
        "the stand-in entry is raised"
    );
    app.settle_access_steps(now);
    assert_eq!(app.system_state_menu_line(), None);
    app.note_fda_regression(true);
    assert!(app.access_steps.fda_regressed());
    app.settle_access_steps(now);
    assert_eq!(
        app.system_state_menu_line(),
        line("System State: Full Disk Access needs you", None)
    );
    app.note_fda_regression(false);
    app.settle_access_steps(now);
    assert_eq!(app.system_state_menu_line(), None);
    for key in RETIRED_KEYS {
        assert!(
            !app.messages.digest().entries.iter().any(|e| e.key == key),
            "{key}"
        );
    }
    assert_eq!(
        ended_as(&app, "privacy.fda.needed"),
        Some(aterm_messages::Retired::Withdrawn),
        "never filed as fixed"
    );
}

/// An older aterm's entry under a retired key that the person already put away stays
/// as they filed it: nothing read its cause end, so its filing is not closed as fixed
/// (its quiet still holds). A clear (the negative control) closes the same filing fixed.
#[test]
fn a_retired_key_the_person_filed_stays_filed() {
    use aterm_messages::Retired;
    use aterm_messages::exceptions::{Entry, Family};
    use aterm_messages::need::{Reason, Then, Why};
    const KEY: &str = "privacy.devtools.not-granted";
    assert!(RETIRED_KEYS.contains(&KEY));
    let filed = || {
        let mut app = App::headless_for_test();
        let now = Instant::now();
        app.except(&Entry::new(
            Family::Privacy,
            "devtools",
            "not-granted",
            "aterm is not in Developer Tools",
            Why::OnlyYou(Reason::new("only you can add aterm to Developer Tools")),
            Then::step("Turn on aterm in Developer Tools"),
        ));
        let id = app
            .messages
            .digest()
            .entries
            .iter()
            .find(|e| e.key == KEY)
            .map(|e| e.id)
            .expect("the stand-in entry is raised");
        assert!(
            app.messages
                .file_digest_one(id, crate::messages_host::wall_stamp_now(), now)
        );
        assert_eq!(ended_as(&app, KEY), Some(Retired::Dismissed));
        app
    };
    let mut app = filed();
    app.settle_access_steps(Instant::now());
    assert_eq!(ended_as(&app, KEY), Some(Retired::Dismissed), "still filed");
    let mut app = filed();
    app.except_clear(KEY);
    assert_eq!(
        ended_as(&app, KEY),
        Some(Retired::Resolved(aterm_messages::Outcome::Ok))
    );
}

/// The Accessibility pane's System Settings address.
#[cfg(target_os = "macos")]
const ACCESSIBILITY_PANE: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

/// A Needs-you row's press: a person's opens the Local Network and
/// Accessibility panes at once; a client's act opens nothing at once (it is
/// queued for the step moment), and with no approved revision it is not even
/// queued (it never mints the yes); the debugger's is the consent track's and
/// opens nothing here.
#[cfg(target_os = "macos")]
#[test]
fn a_persons_press_opens_its_pane_at_once() {
    let mut app = App::headless_for_test();
    app.headless = false;
    app.access_steps.set_reach(StepsReach {
        // AppRt's unit-test arm records URLs without starting a process.
        open_pane: super::live_open_access_pane,
        ..stepping()
    });
    let _ = crate::platform::take_opened_urls();
    assert!(!app.open_access_from_system_state(Access::Accessibility, false));
    assert!(
        crate::platform::take_opened_urls().is_empty(),
        "no approved revision: a client's act opens nothing"
    );
    assert_eq!(app.access_steps.pane_act, None, "and waits for nothing");
    approve(&mut app, 1);
    assert!(app.open_access_from_system_state(Access::LocalNetwork, true));
    assert!(app.open_access_from_system_state(Access::Accessibility, true));
    assert_eq!(
        crate::platform::take_opened_urls(),
        [crate::fleet_glance::LOCAL_NETWORK_PANE, ACCESSIBILITY_PANE]
    );
    assert!(!app.open_access_from_system_state(Access::Accessibility, false));
    assert!(
        crate::platform::take_opened_urls().is_empty(),
        "a client's act waits for the step moment"
    );
    assert!(!app.open_access_from_system_state(Access::Debugger, true));
    let mut headless = App::headless_for_test();
    assert!(!headless.open_access_from_system_state(Access::LocalNetwork, true));
}

/// Full Disk Access from its row: a person's press opens the page with the
/// panel past a standing Not now; a client's act does not pass it.
#[cfg(target_os = "macos")]
#[test]
fn a_persons_press_passes_not_now_and_a_clients_act_does_not() {
    use crate::devtools_helper::{Lane, test_arms};
    let mut app = App::headless_for_test();
    app.headless = false;
    approve(&mut app, 1);
    app.devtools_helper = Lane::windowed_with_fda_for_test(
        test_arms::opening(),
        test_arms::fda_declined(),
        test_arms::showing(),
    );
    let opens = test_arms::fda_counts().0;
    assert!(!app.open_access_from_system_state(Access::FullDiskAccess, false));
    assert_eq!(
        test_arms::fda_counts().0,
        opens,
        "Not now holds for a client"
    );
    assert!(app.open_access_from_system_state(Access::FullDiskAccess, true));
    assert_eq!(test_arms::fda_counts().0, opens + 1);
    assert!(app.fda_panel_up());
    assert_eq!(
        app.system_state_access_step(),
        Some(Access::FullDiskAccess),
        "the panel is the step that is up"
    );
}

/// A Local Network trigger that attempted nothing (no LAN to name, no socket)
/// records nothing and takes no hold: the steps after it go on.
#[test]
fn a_trigger_that_asked_nothing_records_nothing_and_looks_again() {
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        trigger_local_network: || false,
        ..stepping()
    });
    let before = records().len();
    approve(&mut app, 6);
    let now = Instant::now();
    app.tick_local_network_step(now);
    assert_eq!(records().len(), before, "nothing recorded");
    assert!(
        app.access_step_moment(now),
        "no hold: the next step may run"
    );
    let retry = now + super::LOCAL_NETWORK_RETRY;
    focus(&mut app, true);
    assert_eq!(
        app.next_access_step_deadline(now),
        Some(retry),
        "it looks again for a LAN, so the run's Permissions stage is not left waiting"
    );
    // Not before the retry is due.
    app.access_steps.set_reach(stepping());
    let fired = triggers();
    app.tick_local_network_step(now);
    assert_eq!(triggers(), fired);
    // The network came up: the retry asks, and records it.
    app.tick_local_network_step(retry);
    assert_eq!(triggers(), fired + 1);
    assert!(records().contains(&(6, Access::LocalNetwork, AccessOutcome::Triggered)));
    app.tick_local_network_step(retry + super::LOCAL_NETWORK_RETRY);
    assert_eq!(
        triggers(),
        fired + 1,
        "once recorded, never again this revision"
    );
}

/// The menu line is composed where what it reads changes — a status
/// publication, a regression — and never on a park.
#[test]
fn the_menu_line_follows_publications_not_parks() {
    let mut app = App::headless_for_test();
    let now = Instant::now();
    let dock = Subject::Setting(Setting::Dock);
    // What the line reads changes behind the park's back: the park leaves it be.
    app.access_steps.fda_regressed = true;
    app.settle_access_steps(now);
    assert_eq!(
        app.system_state_menu_line(),
        None,
        "a park composes nothing"
    );
    app.access_steps.fda_regressed = false;
    app.publish_system_state(status(
        Selection::all(),
        vec![observed(dock, Standing::NeedsYou, None)],
    ));
    assert_eq!(
        app.system_state_menu_line(),
        line("System State: Dock needs you", None)
    );
    app.publish_system_state(status(Selection::all(), Vec::new()));
    assert_eq!(app.system_state_menu_line(), None);
}

/// An older aterm's vault-join entry carries the approval code: it stays until a
/// status has read the Secrets row (whose note carries a waiting code), then ends
/// withdrawn, since the row carries it now and the join may still wait.
#[test]
fn the_join_entry_ends_only_once_the_secrets_row_is_read() {
    use aterm_messages::exceptions::{Entry, Family};
    use aterm_messages::need::{Reason, Then, Why};
    let mut app = App::headless_for_test();
    let now = Instant::now();
    app.except(&Entry::new(
        Family::Privacy,
        "secrets",
        "join-pending",
        "This Mac needs vault approval",
        Why::OnlyYou(Reason::new("an admin Mac approves this Mac")),
        Then::step("On an admin Mac run aterm secrets approve ABCD-EFGH"),
    ));
    let pending = |app: &App| {
        app.messages
            .digest()
            .entries
            .iter()
            .any(|e| e.key == JOIN_PENDING_KEY)
    };
    assert!(pending(&app), "the stand-in entry is raised");
    app.settle_access_steps(now);
    app.publish_system_state(status(
        Selection::all(),
        vec![observed(
            Subject::Setting(Setting::Dock),
            Standing::InPlace,
            None,
        )],
    ));
    assert!(pending(&app), "no reading of the Secrets row yet");
    let code = "Approve on an admin Mac: ABCD-EFGH";
    let mut earlier = status(
        Selection::all(),
        vec![observed(
            Subject::Setting(Setting::Secrets),
            Standing::NeedsYou,
            Some("Earlier vault approval: OLD-CODE"),
        )],
    );
    earlier.observed_vault_url = Some("http://earlier-vault.local:7484/v1/team".into());
    app.publish_system_state(earlier);
    assert!(
        pending(&app),
        "another vault's hidden row cannot replace the approval notice"
    );
    let mut unknown = status(
        Selection::all(),
        vec![observed(
            Subject::Setting(Setting::Secrets),
            Standing::NeedsYou,
            Some(code),
        )],
    );
    unknown.observed_vault_url = None;
    app.publish_system_state(unknown);
    assert!(pending(&app), "an unknown source cannot replace the notice");
    for standing in [Standing::Unknown, Standing::Failed, Standing::NeedsYou] {
        app.publish_system_state(status(
            Selection::all(),
            vec![observed(Subject::Setting(Setting::Secrets), standing, None)],
        ));
        assert!(
            pending(&app),
            "{standing:?} has no replacement approval guidance"
        );
    }
    for (standing, note) in [(Standing::NeedsYou, Some("  ")), (Standing::Waiting, None)] {
        let mut reading = observed(Subject::Setting(Setting::Secrets), standing, note);
        reading.detail = Some(code.into());
        app.publish_system_state(status(Selection::all(), vec![reading]));
        assert!(
            pending(&app),
            "a detail the row hides cannot replace the approval notice"
        );
    }
    let mut unmanaged = Selection::all();
    unmanaged.set(Setting::Secrets, false);
    app.publish_system_state(status(
        unmanaged,
        vec![observed(
            Subject::Setting(Setting::Secrets),
            Standing::NeedsYou,
            Some(code),
        )],
    ));
    assert!(
        pending(&app),
        "a row left as is hides its approval guidance"
    );
    app.publish_system_state(status(
        Selection::all(),
        vec![observed(
            Subject::Setting(Setting::Secrets),
            Standing::NeedsYou,
            Some(code),
        )],
    ));
    assert!(!pending(&app), "the row carries the code now");
    assert_eq!(
        ended_as(&app, JOIN_PENDING_KEY),
        Some(aterm_messages::Retired::Withdrawn),
        "never filed as fixed"
    );
    assert_eq!(
        app.system_state_menu_line(),
        line("System State: Secrets vault needs you", Some(code))
    );
}

/// An older parent can raise an entry under a retired key after its successor
/// launched: folded in at Commit, it ends as one adopted at launch does, withdrawn
/// (at the next settle, and the vault join's at once, its row read). An entry of
/// another family the same catch-up adopts (the negative control) stands.
#[cfg(unix)]
#[test]
fn an_entry_the_parent_raised_before_commit_ends_like_one_adopted_at_launch() {
    use aterm_messages::exceptions::{Entry, Family};
    use aterm_messages::need::{Reason, Then, Why};
    use aterm_messages::{LogLine, Retired};
    let devtools = Entry::new(
        Family::Privacy,
        "devtools",
        "not-granted",
        "aterm is not in Developer Tools",
        Why::OnlyYou(Reason::new("only you can add aterm to Developer Tools")),
        Then::step("Turn on aterm in Developer Tools"),
    );
    let join = Entry::new(
        Family::Privacy,
        "secrets",
        "join-pending",
        "This Mac needs vault approval",
        Why::OnlyYou(Reason::new("an admin Mac approves this Mac")),
        Then::step("On an admin Mac run aterm secrets approve ABCD-EFGH"),
    );
    let upgrade = Entry::new(
        Family::Upgrade,
        "",
        "own-work",
        "Claude in tab 1 is still on 2.1.283",
        Why::OnlyYou(Reason::new(
            "aterm never ends running work, and a job it started 5 h ago still runs",
        )),
        Then::step("End that job in the tab"),
    );
    let open = |app: &App, key: &str| app.messages.digest().entries.iter().any(|e| e.key == key);
    let mut app = App::headless_for_test();
    let now = Instant::now();
    // This process launched: its settle and the first status that reads the
    // Secrets row ran, with nothing yet to end.
    app.settle_access_steps(now);
    app.publish_system_state(status(
        Selection::all(),
        vec![observed(
            Subject::Setting(Setting::Secrets),
            Standing::NeedsYou,
            Some("Approve on an admin Mac: ABCD-EFGH"),
        )],
    ));
    let mut parent = App::headless_for_test();
    parent.messages.raise_next_id(1_000);
    parent.except(&devtools);
    parent.except(&join);
    parent.except_replace(Family::Upgrade, std::slice::from_ref(&upgrade));
    let lines: Vec<LogLine> = parent
        .messages
        .drain_shelved_for_persist()
        .into_iter()
        .map(|(line, _)| line)
        .collect();
    app.catch_up_messages_with(lines);
    assert!(
        open(&app, &devtools.key()),
        "the Commit adopts the parent's"
    );
    assert!(open(&app, &upgrade.key()));
    assert_eq!(
        ended_as(&app, JOIN_PENDING_KEY),
        Some(Retired::Withdrawn),
        "the row read carries the code"
    );
    app.settle_access_steps(now);
    assert_eq!(ended_as(&app, &devtools.key()), Some(Retired::Withdrawn));
    assert!(open(&app, &upgrade.key()), "another family's stands");
}

#[cfg(target_os = "macos")]
thread_local! {
    static FRONT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// System State is frontmost while [`FRONT`] says so.
#[cfg(target_os = "macos")]
fn front(_app: &App) -> bool {
    FRONT.with(std::cell::Cell::get)
}

#[cfg(target_os = "macos")]
fn set_front(front: bool) {
    FRONT.with(|cell| cell.set(front));
}

#[cfg(target_os = "macos")]
fn probe(state: aterm_containment::consent::FdaState) -> aterm_containment::consent::FdaProbe {
    use aterm_containment::consent::{FdaProbe, FdaState, ProbeLabel};
    FdaProbe {
        state,
        label: if state == FdaState::Granted {
            ProbeLabel::OpenOk
        } else {
            ProbeLabel::OpenEperm
        },
    }
}

/// A new Mac at revision 1: the probe reads Full Disk Access off, the pages and
/// panels are counted stand-ins (`reach`, `fda`), Local Network is on record,
/// and System State is frontmost while [`FRONT`] says so, at a quiet moment.
/// Nothing reaches the OS.
#[cfg(target_os = "macos")]
fn new_mac(reach: crate::devtools_helper::Reach, fda: crate::devtools_helper::FdaReach) -> App {
    use crate::consent_launch::{LaunchConsent, LaunchReach};
    use crate::devtools_helper::{Lane, test_arms};
    let mut app = App::headless_for_test();
    app.headless = false;
    app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(probe(
        aterm_containment::consent::FdaState::Denied,
    ));
    app.config.privacy = Some(crate::app_config::PrivacyConfig {
        probe_interval_ms: Some(3_600_000),
        ..Default::default()
    });
    app.devtools_helper = Lane::windowed_with_fda_for_test(reach, fda, test_arms::showing());
    app.consent_launch = LaunchConsent::with_reach(LaunchReach {
        bundle_id: || Some(test_arms::SUBJECT),
        live: true,
        ..LaunchReach::inert()
    });
    app.access_steps.set_reach(StepsReach {
        recorded: |_, _| Some(AccessOutcome::Triggered),
        frontmost: front,
        // AppRt's unit-test arm records URLs without starting a process.
        open_pane: super::live_open_access_pane,
        ..stepping()
    });
    approve(&mut app, 1);
    app
}

/// Through the real tick, the window's own Full Disk Access page waits queued while System State
/// is not the visible view, however many parks pass, and comes up at the first park it is; the
/// folder warm-up after a Not now waits the same way.
#[cfg(target_os = "macos")]
#[test]
fn the_windows_own_steps_wait_for_system_state_through_the_tick() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::{Origin, PanelEvent, RECHECK_EVERY, test_arms};
    set_front(false);
    let t0 = Instant::now();
    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    assert!(app.consent_card.begin_deciding(t0));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
    assert_eq!(app.devtools_helper.fda_queued(), Some(Origin::Automatic));
    let opens = test_arms::fda_counts().0;
    let t1 = Instant::now();
    for park in 0..3 {
        app.tick_consent_lanes(t1 + RECHECK_EVERY * park);
    }
    assert_eq!(
        test_arms::fda_counts().0,
        opens,
        "System State is not in front"
    );
    assert_eq!(app.devtools_helper.fda_queued(), Some(Origin::Automatic));
    assert_eq!(app.system_state_access_step(), None);

    set_front(true);
    let t2 = t1 + RECHECK_EVERY * 3;
    app.tick_consent_lanes(t2);
    assert_eq!(test_arms::fda_counts().0, opens + 1, "the page comes up");
    assert!(app.fda_panel_up());
    assert_eq!(app.system_state_access_step(), Some(Access::FullDiskAccess));

    // The Not now ends the panel; the folders are next, and they wait for System
    // State too.
    set_front(false);
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    app.tick_consent_lanes(t2);
    assert!(!app.fda_panel_up());
    assert_eq!(
        app.consent_launch.fda(),
        FdaLaunch::Ended { owner_acted: false }
    );
    app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(probe(
        aterm_containment::consent::FdaState::Denied,
    ));
    let t3 = t2 + Duration::from_secs(5);
    app.tick_consent_lanes(t3);
    app.tick_consent_lanes(t3 + RECHECK_EVERY);
    assert!(
        !app.consent_launch.walking(),
        "not while System State is away"
    );
    set_front(true);
    app.tick_consent_lanes(t3 + RECHECK_EVERY * 2);
    assert!(app.consent_launch.walking(), "the folders are asked for");
    // Walking, it parks between folders while the person is away from System State.
    let gate = app
        .access_steps
        .warmup_gate
        .clone()
        .expect("the launch pass is gated");
    assert!(gate.is_open());
    set_front(false);
    app.settle_access_steps(t3 + RECHECK_EVERY * 3);
    assert!(!gate.is_open(), "parked while System State is away");
    set_front(true);
    app.settle_access_steps(t3 + RECHECK_EVERY * 4);
    assert!(gate.is_open(), "back at System State, it walks on");
}

/// A control client's act on the Full Disk Access row never mints the yes: with no approved
/// revision or a `[privacy]` switch off it does nothing; otherwise it is queued for the step
/// moment as a System State act, refused by whatever holds the window's own page (the panel's or
/// the card's Not now, an unstable build, the page within ten minutes, a grant) though a person's
/// press passes each, and once up it writes no owner-acted record, so a regressed grant's menu
/// line is never held dark.
#[cfg(target_os = "macos")]
#[test]
fn a_clients_act_on_full_disk_access_is_held_as_the_windows_own() {
    use crate::consent_card::Marker;
    use crate::devtools_helper::{Origin, test_arms};
    set_front(true);
    let opens = || test_arms::fda_counts().0;

    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    app.hold_initial_setup_permissions();
    let before = opens();
    assert!(!app.open_access_from_system_state(Access::FullDiskAccess, false));
    app.tick_consent_lanes(Instant::now());
    assert_eq!(opens(), before, "no approved revision");
    for privacy in ["enabled", "check", "notice"] {
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.config =
            aterm_toml::from_str(&format!("[privacy]\n{privacy} = false\n")).expect("config");
        assert!(!app.open_access_from_system_state(Access::FullDiskAccess, false));
        assert_eq!(app.devtools_helper.fda_queued(), None, "{privacy} off");
    }

    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    let before = opens();
    assert!(
        !app.open_access_from_system_state(Access::FullDiskAccess, false),
        "queued, not opened"
    );
    assert_eq!(
        app.devtools_helper.fda_queued(),
        Some(Origin::SystemStateAct)
    );
    assert_eq!(opens(), before);
    set_front(false);
    app.tick_consent_lanes(Instant::now());
    assert_eq!(opens(), before, "it waits for the step moment");
    set_front(true);
    app.tick_consent_lanes(Instant::now() + crate::devtools_helper::RECHECK_EVERY);
    assert_eq!(opens(), before + 1);
    assert!(app.fda_panel_up());
    assert_eq!(
        app.consent_card_host.marker, None,
        "a client's act is no one's act"
    );

    /// A refusal case: its name, the build's reach, the page's markers, and
    /// what else it sets on the window.
    type Case = (
        &'static str,
        fn() -> crate::devtools_helper::Reach,
        fn() -> crate::devtools_helper::FdaReach,
        fn(&mut App),
    );
    let cases: [Case; 5] = [
        (
            "the panel's Not now",
            test_arms::opening,
            test_arms::fda_declined,
            |_| {},
        ),
        (
            "the card's Not now",
            test_arms::opening,
            test_arms::fda_opening,
            |app| {
                app.consent_card_host.marker = Some(Marker::NotNow);
            },
        ),
        (
            "an unstable build",
            test_arms::unstable,
            test_arms::fda_opening,
            |_| {},
        ),
        (
            "the page within ten minutes",
            test_arms::opening,
            test_arms::fda_opened_just_now,
            |_| {},
        ),
        (
            "a grant",
            test_arms::opening,
            test_arms::fda_opening,
            |app| {
                app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(
                    probe(aterm_containment::consent::FdaState::Granted),
                );
            },
        ),
    ];
    for (why, reach, fda, setup) in cases {
        let mut app = new_mac(reach(), fda());
        setup(&mut app);
        let before = opens();
        assert!(
            !app.open_access_from_system_state(Access::FullDiskAccess, false),
            "{why}"
        );
        assert_eq!(app.devtools_helper.fda_queued(), None, "{why}: refused");
        app.tick_consent_lanes(Instant::now());
        assert_eq!(opens(), before, "{why}: nothing opens");
        // A person's press passes it.
        assert!(
            app.open_access_from_system_state(Access::FullDiskAccess, true),
            "{why}"
        );
        assert_eq!(opens(), before + 1, "{why}: a person's press");
    }
}

/// A client's act on Developer Tools that is only queued answers `false`: no page
/// came up (it may lapse without one).
#[cfg(target_os = "macos")]
#[test]
fn a_queued_developer_tools_act_is_not_reported_opened() {
    use crate::devtools_helper::{Origin, test_arms};
    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    test_arms::stand_in(&mut app, test_arms::opening());
    let before = test_arms::opens();
    assert!(!app.open_access_from_system_state(Access::DeveloperTools, false));
    assert_eq!(app.devtools_helper.queued(), Some(Origin::SystemStateAct));
    assert_eq!(test_arms::opens(), before);
}

/// A debugger turned off by the person's choice, as the observed record says (it was allowed,
/// or aterm or something else turned it off), is the Debugger row's opt-out at launch, before
/// any admission, probe or step. It is a while-off opt-out: the record says what an off reading
/// means, not that the rule reads off now (`initial::access`'s
/// `a_turned_off_debugger_is_said_only_of_an_off_reading`). No record, or a new Mac's first
/// reading, is no opt-out; a `[privacy]` switch off names itself on every row.
#[test]
fn a_debugger_turned_off_on_record_is_the_rows_opt_out_at_launch() {
    use super::{DEBUGGER_OFF, PRIVACY_OFF};
    use crate::debugger_access::{DebuggerChangeLane, ObservedMarker, Origin, RecordsRead};
    use aterm_containment::consent::DebuggerMode;
    use aterm_types::macos_setup::opt_out::OptOut;
    /// A launch whose shared records were read with `observed` on file.
    fn launched(observed: Option<ObservedMarker>) -> App {
        let mut app = App::headless_for_test();
        app.debugger_change = DebuggerChangeLane::with_liveness(|_| false);
        app.debugger_change.settle_records(RecordsRead {
            observed,
            ..RecordsRead::default()
        });
        app.debugger_change.poll(1);
        app
    }
    let marker = |mode, origin| {
        Some(ObservedMarker {
            mode,
            since: 1_790_000_000,
            origin,
        })
    };
    let chosen = Some(OptOut::WhileOff(DEBUGGER_OFF));
    let cases: [(&str, Option<ObservedMarker>, Option<OptOut>); 6] = [
        (
            "was allowed",
            marker(DebuggerMode::Enabled, Origin::Outside),
            chosen,
        ),
        (
            "turned on by aterm",
            marker(DebuggerMode::Enabled, Origin::Aterm),
            chosen,
        ),
        (
            "turned off by aterm",
            marker(DebuggerMode::Disabled, Origin::Aterm),
            chosen,
        ),
        (
            "turned off by something else",
            marker(DebuggerMode::Disabled, Origin::Outside),
            chosen,
        ),
        (
            "a new Mac's first reading",
            marker(DebuggerMode::Disabled, Origin::Unknown),
            None,
        ),
        ("no record", None, None),
    ];
    for (name, observed, want) in cases {
        let app = launched(observed);
        assert!(app.admitted_access().is_none(), "{name}: no admission");
        assert_eq!(
            app.access_opt_outs(),
            [None, None, want, None],
            "{name}: the Debugger row alone"
        );
    }
    let mut app = launched(marker(DebuggerMode::Enabled, Origin::Outside));
    app.config.privacy = Some(crate::app_config::PrivacyConfig {
        notice: Some(false),
        ..Default::default()
    });
    assert_eq!(
        app.access_opt_outs(),
        [Some(OptOut::Always(PRIVACY_OFF)); 4],
        "privacy off"
    );
}

thread_local! {
    static FAILED_TRIGGERS: RefCell<u32> = const { RefCell::new(0) };
}

/// A trigger that finds no LAN to name, counted on this thread.
fn no_lan() -> bool {
    FAILED_TRIGGERS.with(|t| *t.borrow_mut() += 1);
    false
}

fn failed_triggers() -> u32 {
    FAILED_TRIGGERS.with(|t| *t.borrow())
}

/// The Local Network retry never spins the loop: a trigger that found no LAN looks again a
/// minute later, at the step moment only. A retry due while System State is away is moved on a
/// minute (left due, it would wake the loop on every park), an unfocused window arms none, a new
/// approval drops an earlier revision's, and with none approved it is kept but arms nothing.
#[test]
fn a_failed_trigger_away_from_system_state_never_arms_a_deadline_due_now() {
    let retry = super::LOCAL_NETWORK_RETRY;
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        trigger_local_network: no_lan,
        ..stepping()
    });
    approve(&mut app, 7);
    let t0 = Instant::now();
    let tried = failed_triggers();
    app.tick_local_network_step(t0);
    assert_eq!(failed_triggers(), tried + 1, "System State in front: tried");

    // The person goes to a terminal tab, then to another app.
    app.access_steps.set_reach(StepsReach {
        trigger_local_network: no_lan,
        frontmost: |_| false,
        ..stepping()
    });
    focus(&mut app, false);
    let due = t0 + retry + Duration::from_secs(1);
    for park in 0..3 {
        let now = due + Duration::from_millis(park);
        app.tick_local_network_step(now);
        assert!(
            app.next_access_step_deadline(now)
                .is_none_or(|at| at >= due + retry),
            "park {park}: an unfocused window's retry is a minute out or none, never now"
        );
    }
    focus(&mut app, true);
    let now = due + Duration::from_millis(10);
    app.tick_local_network_step(now);
    assert_eq!(
        app.next_access_step_deadline(now),
        Some(due + retry),
        "focused on a terminal tab: the retry was moved on a minute"
    );
    assert_eq!(failed_triggers(), tried + 1, "never away from its moment");

    // Back at System State when it falls due again: it tries, and fails again.
    app.access_steps.set_reach(StepsReach {
        trigger_local_network: no_lan,
        ..stepping()
    });
    let back = due + retry;
    app.tick_local_network_step(back);
    assert_eq!(failed_triggers(), tried + 2);
    assert_eq!(app.next_access_step_deadline(back), Some(back + retry));

    // A new approval drops the old revision's retry.
    app.access_steps.set_reach(StepsReach {
        trigger_local_network: no_lan,
        frontmost: |_| false,
        ..stepping()
    });
    approve(&mut app, 8);
    let later = back + retry * 2;
    app.tick_local_network_step(later);
    assert_eq!(app.access_steps.local_network_retry, None);
    assert!(
        app.next_access_step_deadline(later)
            .is_none_or(|at| at > later),
        "nothing due at once for a revision no longer approved"
    );
    // The approval going away (an Apply holds the gate) keeps revision 8's
    // retry, which arms nothing meanwhile.
    app.access_steps.set_reach(StepsReach {
        trigger_local_network: no_lan,
        ..stepping()
    });
    app.tick_local_network_step(later);
    assert_eq!(failed_triggers(), tried + 3, "revision 8 tried");
    assert!(app.access_steps.local_network_retry.is_some());
    app.hold_initial_setup_permissions();
    app.tick_local_network_step(later + retry * 2);
    assert_eq!(
        app.access_steps
            .local_network_retry
            .map(|(failed, _)| failed),
        Some(8),
        "kept while no revision is approved"
    );
    assert_eq!(
        app.next_access_step_deadline(later + retry * 2),
        None,
        "nothing armed while no revision is approved"
    );
}

/// A Local Network retry outlives an Apply to another group, which holds the Permissions gate
/// until its fresh status names the same access revision: kept and arming nothing through the
/// gap, then armed again, so the Permissions stage does not wait on Local Network until a
/// relaunch.
#[test]
fn a_retry_outlives_an_apply_that_keeps_the_access_revision() {
    let retry = super::LOCAL_NETWORK_RETRY;
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        trigger_local_network: no_lan,
        ..stepping()
    });
    focus(&mut app, true);
    approve(&mut app, 7);
    let t0 = Instant::now();
    let tried = failed_triggers();
    app.tick_local_network_step(t0);
    assert_eq!(failed_triggers(), tried + 1, "no LAN yet");

    // Keyboard & Trackpad → Set up: the Apply holds the gate.
    app.hold_initial_setup_permissions();
    assert_eq!(app.access_revision(), None);
    let gap = t0 + Duration::from_secs(1);
    app.tick_local_network_step(gap);
    assert_eq!(
        app.next_access_step_deadline(gap),
        None,
        "nothing armed while no revision is approved"
    );

    // The fresh status names the same access revision.
    app.accepted_initial_setup_apply();
    approve(&mut app, 7);
    assert_eq!(app.access_revision(), Some(7));
    app.tick_local_network_step(gap);
    let due = t0 + retry;
    assert_eq!(
        app.next_access_step_deadline(gap),
        Some(due),
        "the retry is armed again for revision 7"
    );
    app.tick_local_network_step(due);
    assert_eq!(failed_triggers(), tried + 2, "it looks for a LAN again");
    assert_eq!(app.next_access_step_deadline(due), Some(due + retry));
}

/// A control client's act on a System State row is queued and comes up only at the step moment
/// (System State the visible view of the focused window, no other macOS dialog up), never at a
/// control verb's quiet moment with a terminal tab in front. Developer Tools goes through the
/// hand-over lane (`Origin::SystemStateAct`); a Local Network or Accessibility pane then counts
/// as up for its hold, and an act still waiting after the lapse is dropped.
#[cfg(target_os = "macos")]
#[test]
fn a_clients_act_on_a_row_waits_for_the_step_moment() {
    use crate::devtools_helper::{ASKED_LAPSE, Origin, RECHECK_EVERY, test_arms};
    let with_dialog_up = StepsReach {
        recorded: |_, _| Some(AccessOutcome::Triggered),
        frontmost: front,
        open_pane: super::live_open_access_pane,
        dialog_up: || true,
        ..stepping()
    };
    let at_rest = StepsReach {
        dialog_up: || false,
        ..with_dialog_up
    };

    // Developer Tools.
    set_front(false);
    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    test_arms::stand_in(&mut app, test_arms::opening());
    focus(&mut app, true);
    app.user_input_recent = |within| within == crate::devtools_helper::PRESENT_WITHIN;
    let t0 = Instant::now() + crate::consent_card::LAUNCH_SETTLE;
    assert!(
        app.devtools_moment_is_quiet(t0),
        "precondition: a control verb's quiet moment, a terminal tab in front"
    );
    let opens = test_arms::opens();
    assert!(!app.open_access_from_system_state(Access::DeveloperTools, false));
    assert_eq!(app.devtools_helper.queued(), Some(Origin::SystemStateAct));
    for park in 0..3 {
        app.tick_consent_lanes(t0 + RECHECK_EVERY * park);
    }
    assert_eq!(test_arms::opens(), opens, "System State is not in front");
    set_front(true);
    app.access_steps.set_reach(with_dialog_up);
    let t1 = t0 + RECHECK_EVERY * 3;
    app.tick_consent_lanes(t1);
    assert_eq!(test_arms::opens(), opens, "another macOS dialog is up");
    app.access_steps.set_reach(at_rest);
    app.tick_consent_lanes(t1 + RECHECK_EVERY);
    assert_eq!(test_arms::opens(), opens + 1, "the page at the step moment");

    // Local Network and Accessibility.
    set_front(false);
    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    show_accessibility_needs_you(&mut app);
    let _ = crate::platform::take_opened_urls();
    let t0 = Instant::now();
    assert!(!app.open_access_from_system_state(Access::Accessibility, false));
    assert!(
        crate::platform::take_opened_urls().is_empty(),
        "queued, never opened at once"
    );
    for park in 0..3 {
        app.tick_consent_lanes(t0 + RECHECK_EVERY * park);
    }
    assert!(
        crate::platform::take_opened_urls().is_empty(),
        "System State is not in front"
    );
    set_front(true);
    app.access_steps.set_reach(with_dialog_up);
    let t1 = t0 + RECHECK_EVERY * 3;
    app.tick_consent_lanes(t1);
    assert!(
        crate::platform::take_opened_urls().is_empty(),
        "another macOS dialog is up"
    );
    app.access_steps.set_reach(at_rest);
    let t2 = t1 + RECHECK_EVERY;
    app.tick_consent_lanes(t2);
    assert_eq!(
        crate::platform::take_opened_urls(),
        [ACCESSIBILITY_PANE],
        "the pane at the step moment"
    );
    assert!(
        !app.access_step_moment(t2),
        "one gesture at a time: the pane counts as up"
    );
    assert!(app.access_step_moment(t2 + super::PANE_HOLD));
    app.tick_consent_lanes(t2 + RECHECK_EVERY);
    assert!(
        crate::platform::take_opened_urls().is_empty(),
        "opened once"
    );

    // An act System State never comes to the front for lapses.
    set_front(false);
    assert!(!app.open_access_from_system_state(Access::LocalNetwork, false));
    let asked = t2 + super::PANE_HOLD;
    app.tick_consent_lanes(asked + ASKED_LAPSE);
    set_front(true);
    app.tick_consent_lanes(asked + ASKED_LAPSE + RECHECK_EVERY);
    assert!(
        crate::platform::take_opened_urls().is_empty(),
        "lapsed after {ASKED_LAPSE:?}"
    );
}

/// A new approval asks again: the Full Disk Access panel's Not now answers revision 1's step
/// (recorded `declined`), while under revision 2 the page comes up and nothing records it
/// declined.
#[cfg(target_os = "macos")]
#[test]
fn a_not_now_of_an_earlier_revision_does_not_answer_a_new_approval() {
    use crate::consent_card::PendingDecision;
    use crate::devtools_helper::{Origin, test_arms};
    set_front(true);
    let t0 = Instant::now();
    let fda = Access::FullDiskAccess;

    let mut app = new_mac(test_arms::opening(), test_arms::fda_declined());
    let before = records().len();
    assert!(app.consent_card.begin_deciding(t0));
    app.decide_macos_access_card(PendingDecision::launch(None));
    assert_eq!(app.devtools_helper.fda_queued(), None, "its Not now holds");
    assert_eq!(
        records()[before..],
        [(1, fda, AccessOutcome::Declined)],
        "the step is answered"
    );

    let mut app = new_mac(test_arms::opening(), test_arms::fda_declined());
    approve(&mut app, 2);
    let before = records().len();
    let opens = test_arms::fda_counts().0;
    assert!(app.consent_card.begin_deciding(t0));
    app.decide_macos_access_card(PendingDecision::launch(None));
    assert_eq!(app.devtools_helper.fda_queued(), Some(Origin::Automatic));
    app.tick_consent_lanes(Instant::now());
    assert_eq!(
        test_arms::fda_counts().0,
        opens + 1,
        "the new approval asks again"
    );
    assert!(app.fda_panel_up());
    assert!(!records()[before..].contains(&(2, fda, AccessOutcome::Declined)));
}

/// The access card's Not now (an older aterm's file-access question) is spent on the first
/// approved revision it meets: recorded once as that revision's `declined` unless the row has
/// something on record in it already, and retired either way, so a later revision asks again.
/// Without an approved revision, or with a `[privacy]` switch off, it is neither recorded nor
/// spent.
#[test]
fn the_access_cards_not_now_is_spent_on_the_first_revision_it_meets() {
    use crate::consent_card::Marker;
    let fda = Access::FullDiskAccess;
    let now = Instant::now();
    let unrecorded = StepsReach {
        recorded: |_, access| (access == Access::LocalNetwork).then_some(AccessOutcome::Triggered),
        ..stepping()
    };

    let mut app = App::headless_for_test();
    app.access_steps.set_reach(unrecorded);
    let before = records().len();
    app.consent_card_host.marker = Some(Marker::NotNow);
    app.settle_access_steps(now);
    assert_eq!(records().len(), before, "no approved revision");
    assert_eq!(
        app.consent_card_host.marker,
        Some(Marker::NotNow),
        "kept for the first revision"
    );
    app.consent_card_host.marker = None;
    approve(&mut app, 3);
    app.settle_access_steps(now);
    assert_eq!(records().len(), before, "no Not now on record");
    app.consent_card_host.marker = Some(Marker::NotNow);
    for _ in 0..3 {
        app.settle_access_steps(now);
    }
    assert_eq!(
        records()[before..],
        [(3, fda, AccessOutcome::Declined)],
        "once"
    );
    assert_eq!(app.consent_card_host.marker, None, "spent, and retired");
    approve(&mut app, 4);
    app.settle_access_steps(now);
    assert_eq!(
        records()[before..],
        [(3, fda, AccessOutcome::Declined)],
        "a new approval is not answered by it"
    );

    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        recorded: |_, _| Some(AccessOutcome::Handed),
        ..stepping()
    });
    app.consent_card_host.marker = Some(Marker::NotNow);
    let before = records().len();
    approve(&mut app, 5);
    app.settle_access_steps(now);
    assert_eq!(
        records().len(),
        before,
        "the row's step is on record already"
    );
    assert_eq!(app.consent_card_host.marker, None, "spent all the same");

    let mut app = App::headless_for_test();
    app.access_steps.set_reach(unrecorded);
    app.config.privacy = Some(crate::app_config::PrivacyConfig {
        notice: Some(false),
        ..Default::default()
    });
    app.consent_card_host.marker = Some(Marker::NotNow);
    let before = records().len();
    approve(&mut app, 6);
    app.settle_access_steps(now);
    assert_eq!(records().len(), before, "a [privacy] switch is off");
    assert_eq!(app.consent_card_host.marker, Some(Marker::NotNow));
}

/// The row and the window agree on the access card's Not now ("Setting Permissions up again
/// asks again"): under revision 1 it is recorded `declined` and the marker beside `aterm.toml`
/// is removed, and a later launch under revision 1 brings nothing up; under revision 2 nothing
/// records the row declined and the window's own page comes up at the step moment.
#[cfg(target_os = "macos")]
#[test]
fn the_access_cards_not_now_holds_its_revision_and_a_new_one_asks_again() {
    use crate::consent_card::{self, Marker, PendingDecision};
    use crate::devtools_helper::test_arms;
    let fda = Access::FullDiskAccess;
    let on_record_but_lan = StepsReach {
        recorded: |_, access| (access == Access::LocalNetwork).then_some(AccessOutcome::Triggered),
        frontmost: front,
        ..stepping()
    };
    let dir = aterm_tempfile::tempdir().expect("a scratch config folder");
    let config = dir.path().join("aterm.toml");
    consent_card::record_marker(Some(&config), Marker::NotNow).expect("an older aterm's answer");
    let on_disk = || consent_card::read_marker(Some(&config));
    let launch = |revision: u64| {
        let mut app = new_mac(test_arms::opening(), test_arms::fda_keeping_declines());
        app.access_steps.set_reach(on_record_but_lan);
        app.consent_card_config = Some(config.clone());
        approve(&mut app, revision);
        assert!(app.consent_card.begin_deciding(Instant::now()));
        app.decide_macos_access_card(PendingDecision::launch(on_disk()));
        app
    };
    set_front(true);
    test_arms::forget_fda_declines();
    let opens = test_arms::fda_counts().0;

    // Revision 1 meets the card's Not now: answered, and spent.
    let before = records().len();
    let mut app = launch(1);
    assert_eq!(app.devtools_helper.fda_queued(), None, "its Not now holds");
    app.settle_access_steps(Instant::now());
    assert_eq!(records()[before..], [(1, fda, AccessOutcome::Declined)]);
    assert_eq!(app.consent_card_host.marker, None, "spent");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the spent not-now is removed beside aterm.toml",
        || on_disk().is_none().then_some(()),
    );

    // A later launch under revision 1: its Not now still holds.
    let mut app = launch(1);
    assert_eq!(
        app.devtools_helper.fda_queued(),
        None,
        "revision 1 is answered"
    );
    app.tick_consent_lanes(Instant::now());
    assert_eq!(test_arms::fda_counts().0, opens, "nothing comes up");

    // Permissions turned off and on again: revision 2 asks again.
    let before = records().len();
    let mut app = launch(2);
    // The page's own `opened` marker, written off the UI thread, goes nowhere:
    // the scratch folder goes with the test.
    app.consent_card_config = None;
    app.settle_access_steps(Instant::now());
    app.tick_consent_lanes(Instant::now());
    assert!(
        !records()[before..].contains(&(2, fda, AccessOutcome::Declined)),
        "nothing answers revision 2"
    );
    assert_eq!(
        test_arms::fda_counts().0,
        opens + 1,
        "the new approval asks again"
    );
    assert!(app.fda_panel_up());
}

/// A new approval in this process brings the Full Disk Access page back with no relaunch: held
/// for revision 1 by its Not now, it comes back for revision 2; the same revision after a gap
/// brings nothing, a page within ten minutes delays the next revision's (a wait, not a
/// refusal), and a grant that reads on brings nothing.
#[cfg(target_os = "macos")]
#[test]
fn a_new_access_revision_in_this_process_brings_the_full_disk_access_page_back() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::{Origin, PanelEvent, RECHECK_EVERY, test_arms};
    let fda = Access::FullDiskAccess;
    let opens = || test_arms::fda_counts().0;
    set_front(true);
    test_arms::forget_fda_declines();
    let mut app = new_mac(test_arms::opening(), test_arms::fda_keeping_declines());
    // No folder pass after a Not now: its dialogs would hold the step moment.
    app.config.privacy = Some(crate::app_config::PrivacyConfig {
        probe_interval_ms: Some(3_600_000),
        warmup: Some("never".to_owned()),
        ..Default::default()
    });
    let mut t = Instant::now();
    let mut park = |app: &mut App, by: Duration| {
        t += by;
        app.tick_consent_lanes(t);
        t
    };
    // A page brought up retires the cached probe; here nothing reads it again.
    let reads_off = |app: &mut App| {
        app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(probe(
            aterm_containment::consent::FdaState::Denied,
        ));
    };
    assert!(app.consent_card.begin_deciding(Instant::now()));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
    let first = opens();
    park(&mut app, RECHECK_EVERY);
    assert_eq!(opens(), first + 1, "revision 1's page");
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    park(&mut app, RECHECK_EVERY);
    reads_off(&mut app);
    assert!(!app.fda_panel_up());
    assert!(records().contains(&(1, fda, AccessOutcome::Declined)));
    for _ in 0..3 {
        park(&mut app, RECHECK_EVERY);
    }
    assert_eq!(opens(), first + 1, "its Not now holds revision 1");

    // Permissions turned off and on again on the page: revision 2.
    approve(&mut app, 2);
    park(&mut app, RECHECK_EVERY);
    park(&mut app, RECHECK_EVERY);
    assert_eq!(opens(), first + 2, "the new approval asks again");
    assert!(app.fda_panel_up());
    assert_eq!(
        app.consent_launch.fda(),
        FdaLaunch::Waiting,
        "the folders wait"
    );
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    park(&mut app, RECHECK_EVERY);
    reads_off(&mut app);
    assert!(records().contains(&(2, fda, AccessOutcome::Declined)));

    // A gap with none approved, then the same revision: nothing comes back.
    app.observe_initial_setup_permissions(None);
    park(&mut app, RECHECK_EVERY);
    approve(&mut app, 2);
    for _ in 0..3 {
        park(&mut app, RECHECK_EVERY);
    }
    assert_eq!(app.devtools_helper.fda_queued(), None);
    assert_eq!(opens(), first + 2, "revision 2 is answered");

    // Revision 3 within ten minutes of the last page: it waits them out.
    test_arms::set_fda_opened_ago(Some(30));
    approve(&mut app, 3);
    park(&mut app, RECHECK_EVERY);
    park(&mut app, RECHECK_EVERY);
    assert_eq!(opens(), first + 2, "never two pages within ten minutes");
    assert_eq!(
        app.devtools_helper.fda_queued(),
        Some(Origin::Automatic),
        "a wait, not a refusal"
    );
    test_arms::set_fda_opened_ago(None);
    park(
        &mut app,
        Duration::from_secs(atpkg::devtools::REOPEN_AFTER_S),
    );
    assert_eq!(opens(), first + 3, "then it comes up");
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    park(&mut app, RECHECK_EVERY);

    // A grant that reads on: revision 4 brings nothing.
    app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(probe(
        aterm_containment::consent::FdaState::Granted,
    ));
    approve(&mut app, 4);
    park(&mut app, RECHECK_EVERY);
    park(&mut app, RECHECK_EVERY);
    assert_eq!(app.devtools_helper.fda_queued(), None);
    assert_eq!(opens(), first + 3, "nothing to ask for");
    test_arms::forget_fda_declines();
}

/// A new approval in this process asks again for Developer Tools too: the watch asks once per
/// process, and revision 2 lets that latch go, bringing the page back at the next step moment.
#[cfg(target_os = "macos")]
#[test]
fn a_new_access_revision_in_this_process_brings_the_developer_tools_page_back() {
    use crate::devtools_helper::{HandOver, Origin, PanelEvent, RECHECK_EVERY, test_arms};
    use crate::devtools_watch::Step;
    use aterm_containment::DevToolGrant;
    set_front(true);
    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(probe(
        aterm_containment::consent::FdaState::Granted,
    ));
    let mut t = Instant::now();
    let mut park = |app: &mut App| {
        t += RECHECK_EVERY;
        app.tick_consent_lanes(t);
    };
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
        Instant::now(),
    );
    let first = test_arms::opens();
    assert_eq!(
        app.devtools_watch.observe(DevToolGrant::NotGranted, false),
        Step::NotOn { hand_over: true }
    );
    assert_eq!(
        app.hand_devtools_to_owner(Origin::Automatic),
        HandOver::Queued
    );
    park(&mut app);
    assert_eq!(test_arms::opens(), first + 1, "revision 1's page");
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    park(&mut app);
    assert!(app.devtools_helper.panel().is_none());
    assert_eq!(
        app.devtools_watch.observe(DevToolGrant::NotGranted, false),
        Step::NotOn { hand_over: false },
        "once per process"
    );
    park(&mut app);
    assert_eq!(test_arms::opens(), first + 1);

    approve(&mut app, 2);
    park(&mut app);
    park(&mut app);
    assert_eq!(test_arms::opens(), first + 2, "the new approval asks again");
}

/// The `[privacy]` switches each test below turns off, one at a time.
const SWITCHES: [&str; 3] = ["enabled", "check", "notice"];

/// `[privacy]` with the switch `off` turned off (none: every switch on), a probe
/// planted for an hour, and no folder pass (its dialogs would hold the step
/// moment).
fn switches(off: Option<&str>) -> crate::app_config::PrivacyConfig {
    let mut privacy = crate::app_config::PrivacyConfig {
        probe_interval_ms: Some(3_600_000),
        warmup: Some("never".to_owned()),
        ..Default::default()
    };
    match off {
        Some("enabled") => privacy.enabled = Some(false),
        Some("check") => privacy.check = Some(false),
        Some("notice") => privacy.notice = Some(false),
        _ => {}
    }
    privacy
}

/// A `[privacy]` switch off pauses the Local Network step, never answers it: the trigger runs for
/// the same revision once the switch is on, and a retry that fell due meanwhile is moved on
/// (never left due) and looks for a LAN when it next falls due.
#[test]
fn a_privacy_switch_off_pauses_the_local_network_step() {
    let retry = super::LOCAL_NETWORK_RETRY;
    for off in SWITCHES {
        let t0 = Instant::now();
        let mut app = App::headless_for_test();
        app.access_steps.set_reach(stepping());
        app.config.privacy = Some(switches(Some(off)));
        approve(&mut app, 5);
        let (fired, recorded) = (triggers(), records().len());
        app.tick_local_network_step(t0);
        assert_eq!(triggers(), fired, "{off} off: nothing asked");
        assert_eq!(records().len(), recorded, "{off} off: nothing recorded");
        app.config.privacy = Some(switches(None));
        app.tick_local_network_step(t0);
        assert_eq!(triggers(), fired + 1, "{off} on again: asked");
        assert_eq!(
            records()[recorded..],
            [(5, Access::LocalNetwork, AccessOutcome::Triggered)]
        );

        let mut app = App::headless_for_test();
        app.access_steps.set_reach(StepsReach {
            trigger_local_network: no_lan,
            ..stepping()
        });
        focus(&mut app, true);
        approve(&mut app, 7);
        let tried = failed_triggers();
        app.tick_local_network_step(t0);
        assert_eq!(failed_triggers(), tried + 1, "PRECONDITION: no LAN yet");
        app.config.privacy = Some(switches(Some(off)));
        let due = t0 + retry;
        app.tick_local_network_step(due);
        assert_eq!(failed_triggers(), tried + 1, "{off} off: not looked for");
        assert_eq!(
            app.next_access_step_deadline(due),
            None,
            "{off} off: nothing armed"
        );
        app.config.privacy = Some(switches(None));
        assert_eq!(
            app.next_access_step_deadline(due),
            Some(due + retry),
            "{off} on again: the retry was moved on, never left due"
        );
        app.tick_local_network_step(due + retry);
        assert_eq!(failed_triggers(), tried + 2, "{off} on again: looked for");
    }
}

/// `n` parks of the consent lanes, each [`crate::devtools_helper::RECHECK_EVERY`]
/// after the last.
#[cfg(target_os = "macos")]
fn parks(app: &mut App, t: &mut Instant, n: u32) {
    for _ in 0..n {
        *t += crate::devtools_helper::RECHECK_EVERY;
        app.tick_consent_lanes(*t);
    }
}

/// A `[privacy]` switch off pauses the window's own Full Disk Access page: queued by the launch
/// check, it waits with the folders behind it and comes up at the first step moment once every
/// switch is on again.
#[cfg(target_os = "macos")]
#[test]
fn a_privacy_switch_off_pauses_the_full_disk_access_page() {
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::{Origin, test_arms};
    set_front(true);
    for off in SWITCHES {
        let mut t = Instant::now();
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.config.privacy = Some(switches(None));
        assert!(app.consent_card.begin_deciding(t));
        app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
        assert_eq!(
            app.devtools_helper.fda_queued(),
            Some(Origin::Automatic),
            "PRECONDITION: the launch check queued the page"
        );
        app.config.privacy = Some(switches(Some(off)));
        let opens = test_arms::fda_counts().0;
        parks(&mut app, &mut t, 3);
        assert_eq!(test_arms::fda_counts().0, opens, "{off} off: nothing opens");
        assert_eq!(
            app.devtools_helper.fda_queued(),
            Some(Origin::Automatic),
            "{off} off: still waiting"
        );
        assert_eq!(app.consent_launch.fda(), FdaLaunch::Waiting, "{off} off");
        app.config.privacy = Some(switches(None));
        parks(&mut app, &mut t, 1);
        assert_eq!(
            test_arms::fda_counts().0,
            opens + 1,
            "{off} on again: the page comes up"
        );
        assert!(app.fda_panel_up());
    }
}

/// A switch off pauses the window's queued Developer Tools page too: it comes up once every
/// switch is on again.
#[cfg(target_os = "macos")]
#[test]
fn a_privacy_switch_off_pauses_the_developer_tools_page() {
    use crate::devtools_helper::{HandOver, Origin, test_arms};
    use aterm_containment::DevToolGrant;
    set_front(true);
    for off in SWITCHES {
        let mut t = Instant::now();
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.config.privacy = Some(switches(None));
        app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(probe(
            aterm_containment::consent::FdaState::Granted,
        ));
        app.devtools_helper.note_reading(
            test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
            Instant::now(),
        );
        assert_eq!(
            app.hand_devtools_to_owner(Origin::Automatic),
            HandOver::Queued
        );
        app.config.privacy = Some(switches(Some(off)));
        let opens = test_arms::opens();
        parks(&mut app, &mut t, 3);
        assert_eq!(test_arms::opens(), opens, "{off} off: nothing opens");
        assert_eq!(
            app.devtools_helper.queued(),
            Some(Origin::Automatic),
            "{off} off: still waiting"
        );
        app.config.privacy = Some(switches(None));
        parks(&mut app, &mut t, 1);
        assert_eq!(
            test_arms::opens(),
            opens + 1,
            "{off} on again: the page comes up"
        );
    }
}

/// A switch off holds a client's act on the Accessibility row queued before it went off: no pane
/// while it is off, the pane once every switch is on again.
#[cfg(target_os = "macos")]
#[test]
fn a_privacy_switch_off_pauses_a_clients_queued_act() {
    use crate::devtools_helper::test_arms;
    set_front(true);
    for off in SWITCHES {
        let mut t = Instant::now();
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.config.privacy = Some(switches(None));
        show_accessibility_needs_you(&mut app);
        let _ = crate::platform::take_opened_urls();
        assert!(!app.open_access_from_system_state(Access::Accessibility, false));
        app.config.privacy = Some(switches(Some(off)));
        parks(&mut app, &mut t, 3);
        assert!(
            crate::platform::take_opened_urls().is_empty(),
            "{off} off: nothing opens"
        );
        app.config.privacy = Some(switches(None));
        parks(&mut app, &mut t, 1);
        assert_eq!(
            crate::platform::take_opened_urls(),
            [ACCESSIBILITY_PANE],
            "{off} on again: the pane opens"
        );
    }
}

/// A revision approved while a switch is off brings nothing up until every switch is on again;
/// then its Full Disk Access page comes up at the step moment.
#[cfg(target_os = "macos")]
#[test]
fn a_revision_approved_while_a_switch_is_off_asks_again_once_it_is_on() {
    use crate::devtools_helper::{PanelEvent, RECHECK_EVERY, test_arms};
    let opens = || test_arms::fda_counts().0;
    set_front(true);
    for off in SWITCHES {
        test_arms::forget_fda_declines();
        let mut app = new_mac(test_arms::opening(), test_arms::fda_keeping_declines());
        app.config.privacy = Some(switches(None));
        let mut t = Instant::now();
        let mut park = |app: &mut App| {
            t += RECHECK_EVERY;
            app.tick_consent_lanes(t);
        };
        assert!(app.consent_card.begin_deciding(Instant::now()));
        app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
        let first = opens();
        park(&mut app);
        assert_eq!(opens(), first + 1, "PRECONDITION: revision 1's page");
        app.devtools_helper.press_for_test(PanelEvent::NotNow);
        park(&mut app);
        app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(probe(
            aterm_containment::consent::FdaState::Denied,
        ));

        app.config.privacy = Some(switches(Some(off)));
        approve(&mut app, 2);
        for _ in 0..3 {
            park(&mut app);
        }
        assert_eq!(opens(), first + 1, "{off} off: nothing comes up");
        app.config.privacy = Some(switches(None));
        park(&mut app);
        park(&mut app);
        assert_eq!(opens(), first + 2, "{off} on again: revision 2 asks again");
        assert!(app.fda_panel_up());
    }
    test_arms::forget_fda_declines();
}

/// Walking after a Full Disk Access Not now, the folder pass parks at the next folder once a
/// switch is turned off or `warmup = "never"` is set, and walks on once they are back.
#[cfg(target_os = "macos")]
#[test]
fn a_privacy_switch_turned_off_mid_walk_parks_the_folders() {
    use crate::devtools_helper::{PanelEvent, RECHECK_EVERY, test_arms};
    let walking = || crate::app_config::PrivacyConfig {
        probe_interval_ms: Some(3_600_000),
        ..Default::default()
    };
    let never = || crate::app_config::PrivacyConfig {
        warmup: Some("never".to_owned()),
        ..walking()
    };
    let mut offs: Vec<(&str, crate::app_config::PrivacyConfig)> = SWITCHES
        .iter()
        .map(|off| {
            (
                *off,
                crate::app_config::PrivacyConfig {
                    warmup: None,
                    ..switches(Some(off))
                },
            )
        })
        .collect();
    offs.push(("warmup", never()));
    set_front(true);
    for (off, privacy) in offs {
        let mut t = Instant::now();
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        assert!(app.consent_card.begin_deciding(t));
        app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
        t += RECHECK_EVERY;
        app.tick_consent_lanes(t);
        assert!(app.fda_panel_up(), "PRECONDITION: the page");
        app.devtools_helper.press_for_test(PanelEvent::NotNow);
        app.tick_consent_lanes(t);
        app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(probe(
            aterm_containment::consent::FdaState::Denied,
        ));
        for _ in 0..3 {
            t += RECHECK_EVERY;
            app.tick_consent_lanes(t);
        }
        assert!(
            app.consent_launch.walking(),
            "PRECONDITION: the folders are asked for"
        );
        let gate = app
            .access_steps
            .warmup_gate
            .clone()
            .expect("the launch pass is gated");
        assert!(gate.is_open());
        app.config.privacy = Some(privacy);
        app.settle_access_steps(t);
        assert!(!gate.is_open(), "{off}: parked at the next folder");
        app.config.privacy = Some(walking());
        app.settle_access_steps(t);
        assert!(gate.is_open(), "{off} back: it walks on");
    }
}

#[cfg(target_os = "macos")]
struct WithdrawingFolderProbe {
    first: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    calls: std::sync::mpsc::Sender<String>,
}

#[cfg(target_os = "macos")]
static WITHDRAWING_FOLDER_PROBE: std::sync::Mutex<Option<WithdrawingFolderProbe>> =
    std::sync::Mutex::new(None);

#[cfg(target_os = "macos")]
fn withdrawing_folder_probe(path: &std::path::Path) -> crate::consent_warmup::ReadDirOutcome {
    let (first, calls) = {
        let mut probe = WITHDRAWING_FOLDER_PROBE.lock().unwrap();
        let Some(probe) = probe.as_mut() else {
            return crate::consent_warmup::ReadDirOutcome::Refused;
        };
        (probe.first.take(), probe.calls.clone())
    };
    let _ = calls.send(path.file_name().unwrap().to_string_lossy().into_owned());
    if let Some((entered, release)) = first {
        let _ = entered.send(());
        let _ = release.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    }
    crate::consent_warmup::ReadDirOutcome::Refused
}

#[cfg(target_os = "macos")]
struct ReleaseWithdrawingFolder {
    release: Option<std::sync::mpsc::Sender<()>>,
    gate: crate::consent_warmup::WarmupGate,
}

#[cfg(target_os = "macos")]
impl Drop for ReleaseWithdrawingFolder {
    fn drop(&mut self) {
        self.gate.set(true);
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        *WITHDRAWING_FOLDER_PROBE.lock().unwrap() = None;
    }
}

/// A real worker is held in its first inert read. Revoking its admission
/// retires the pass before another folder; a new approval starts a new pass.
#[cfg(target_os = "macos")]
fn check_folder_permission_withdrawal(withdraw: fn(&mut App)) {
    use crate::consent_launch::FdaLaunch;
    use crate::consent_warmup::WarmupState;
    use crate::devtools_helper::test_arms;
    use aterm_types::macos_setup::{MacosDialog, macos_dialog_up};

    let _serial = DIALOG_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (entered_send, entered) = std::sync::mpsc::channel();
    let (release, release_read) = std::sync::mpsc::channel();
    let (calls_send, calls) = std::sync::mpsc::channel();
    *WITHDRAWING_FOLDER_PROBE.lock().unwrap() = Some(WithdrawingFolderProbe {
        first: Some((entered_send, release_read)),
        calls: calls_send,
    });
    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    app.config.privacy.as_mut().unwrap().warmup_folders =
        Some(vec!["documents".into(), "downloads".into()]);
    app.access_steps.set_reach(StepsReach {
        holds_dialog: true,
        ..stepping()
    });
    app.consent_warmup = WarmupState::with_read_dir_for_test(withdrawing_folder_probe);
    app.consent_launch
        .set_fda(FdaLaunch::Ended { owner_acted: false });
    app.consent_launch
        .note_launch_read(aterm_containment::consent::FdaState::Denied);
    let now = Instant::now();
    app.launch_warmup_look(now, |_, _| true);
    entered.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert_eq!(
        calls.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap(),
        "Documents"
    );
    let gate = app.access_steps.warmup_gate.clone().unwrap();
    let mut cleanup = ReleaseWithdrawingFolder {
        release: Some(release),
        gate: gate.clone(),
    };
    assert!(gate.is_open());
    assert!(macos_dialog_up(), "the first read retains the dialog slot");

    withdraw(&mut app);
    assert!(
        !gate.is_open(),
        "withdrawal closes the walking pass before dispatch returns"
    );
    app.settle_access_steps(now);
    assert!(
        !gate.is_open(),
        "a revoked approval cannot reopen the old worker"
    );
    cleanup.release.take().unwrap().send(()).unwrap();
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while app.consent_warmup_live() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(
        !app.consent_warmup.is_live(),
        "the retired pass exits after its current read"
    );
    app.note_launch_warmup_progress();
    app.settle_access_steps(now);
    assert!(
        calls.try_recv().is_err(),
        "the revoked worker never probes Downloads"
    );
    assert!(
        !macos_dialog_up(),
        "the retired pass cannot strand the dialog slot"
    );
    let other_step = MacosDialog::when_none_up(Duration::ZERO).unwrap();
    drop(other_step);

    // A fresh admission can keep the access revision when another group was
    // changed. It still gets a new worker and gate, never the retired worker.
    app.accepted_initial_setup_apply();
    approve(&mut app, 1);
    app.settle_access_steps(now);
    app.launch_warmup_look(now, |_, _| true);
    assert_eq!(
        calls.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap(),
        "Documents"
    );
    assert_eq!(
        calls.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap(),
        "Downloads"
    );
    let fresh_gate = app.access_steps.warmup_gate.clone().unwrap();
    assert!(fresh_gate.is_open());
    gate.set(true);
    assert!(!gate.is_open(), "a retired worker's gate cannot be revived");
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while app.consent_warmup_live() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(!app.consent_warmup.is_live());
    app.note_launch_warmup_progress();
    app.settle_access_steps(now);
    assert!(!macos_dialog_up());
}

#[cfg(target_os = "macos")]
#[test]
fn a_walking_folder_pass_retires_on_a_local_permission_hold() {
    check_folder_permission_withdrawal(App::hold_initial_setup_permissions);
}

#[cfg(target_os = "macos")]
#[test]
fn a_walking_folder_pass_retires_on_a_revoked_permission_status() {
    check_folder_permission_withdrawal(|app| {
        app.observe_initial_setup_permissions(Some(InitialSetupPermissionPolicy {
            revision: 1,
            allowed: false,
        }));
        app.settle_access_steps(Instant::now());
    });
}

/// THE MENU LINE STAYS DARK FOR A SPENT CARD'S NOT NOW THROUGH ITS REVISION (the
/// checker's case): an older aterm asked again after a verified grant read off
/// (`asked_at` after `verified_at`) and the person answered Not now. Revision 1
/// spends that Not now and retires the marker; the next launch under the same
/// revision 1, whose row still says "You chose Not now", finds the grant
/// regressed but leaves the menu line dark. Revision 2 (Permissions turned off
/// and on again) asks again, and the line names Full Disk Access.
#[cfg(target_os = "macos")]
#[test]
fn a_spent_card_not_now_keeps_the_menu_line_dark_in_its_own_revision() {
    use crate::consent_card::{self, AccessRecord, DecideWhy, Marker, PendingDecision};
    use crate::devtools_helper::test_arms;
    let on_record_but_lan = StepsReach {
        recorded: |_, access| (access == Access::LocalNetwork).then_some(AccessOutcome::Triggered),
        frontmost: front,
        ..stepping()
    };
    let dir = aterm_tempfile::tempdir().expect("a scratch config folder");
    let config = dir.path().join("aterm.toml");
    consent_card::record_marker(Some(&config), Marker::NotNow).expect("an older aterm's answer");
    let on_disk = || consent_card::read_marker(Some(&config));
    let record = AccessRecord {
        asked_at: Some(200),
        verified_at: Some(100),
        ..AccessRecord::default()
    };
    let launch = |revision: u64| {
        let mut app = new_mac(test_arms::opening(), test_arms::fda_keeping_declines());
        app.access_steps.set_reach(on_record_but_lan);
        app.consent_card_config = Some(config.clone());
        approve(&mut app, revision);
        assert!(app.consent_card.begin_deciding(Instant::now()));
        app.decide_macos_access_card(PendingDecision {
            marker: on_disk(),
            record,
            why: DecideWhy::Launch,
            verified_here: Some(true),
        });
        app
    };
    let names_fda = line("System State: Full Disk Access needs you", None);
    set_front(true);
    test_arms::forget_fda_declines();
    let mut app = launch(1);
    assert_ne!(
        app.system_state_menu_line(),
        names_fda,
        "the card's Not now"
    );
    app.settle_access_steps(Instant::now());
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the spent not-now is removed beside aterm.toml",
        || on_disk().is_none().then_some(()),
    );
    assert_ne!(
        app.system_state_menu_line(),
        names_fda,
        "spent in revision 1"
    );

    let app = launch(1);
    assert!(
        app.access_steps.fda_regressed(),
        "PRECONDITION: the grant reads regressed"
    );
    assert_ne!(
        app.system_state_menu_line(),
        names_fda,
        "revision 1's Not now still holds: the line does not name Full Disk Access"
    );

    let mut app = launch(2);
    app.consent_card_config = None;
    assert_eq!(
        app.system_state_menu_line(),
        names_fda,
        "a new approval asks again, and the line follows it"
    );
    test_arms::forget_fda_declines();
}

/// The status item's press with no window asks for a new one, as Cmd-N does, and answers
/// `false`; with no event loop the ask is refused in its own message row, which is how the test
/// sees it. A headless instance asks for none; with a window (the control) System State shows
/// in it at once, raised.
#[test]
fn the_status_item_with_no_window_asks_for_a_new_one() {
    let said_no_window = |app: &App| {
        app.messages
            .live_rows()
            .any(|row| row.msg.title == "Couldn't open a new window")
    };
    let mut app = App::headless_for_test();
    let wid = *app.windows.keys().next().expect("a window");
    let _ = crate::app_tabs::take_raised_windows();
    assert!(app.open_system_state_from_status_item());
    assert_eq!(crate::app_tabs::take_raised_windows(), vec![wid]);

    let mut app = App::headless_for_test();
    app.windows.clear();
    app.frontmost_window = None;
    assert!(!app.open_system_state_from_status_item());
    assert!(!said_no_window(&app), "headless: no window asked for");

    app.headless = false;
    let next = app.next_window_id;
    assert!(!app.open_system_state_from_status_item());
    assert_eq!(app.next_window_id, next + 1, "a new window was asked for");
    assert!(said_no_window(&app), "the ask's own refusal row");
}

/// The window the status item asked for shows System State, raised, once its first shell lands;
/// a window that never came shows nothing. Unix only: the session writes into
/// `tab_spawn::tests`' libc pipe.
#[cfg(unix)]
#[test]
fn the_status_items_new_window_shows_system_state_once_it_lands() {
    use crate::tab_spawn::{SpawnAdmission, TabSpawned};
    let mut app = App::headless_for_test();
    let _ = crate::app_tabs::take_raised_windows();
    let (id, wid) = crate::tab_spawn::test_support::book_window(
        &mut app,
        None,
        SpawnAdmission::Plain,
        App::system_state_once_landed(),
    );
    assert!(
        !app.windows.contains_key(&wid),
        "no window before its shell"
    );
    let (session, _pty) = crate::tab_spawn::tests::registered_piped_session(id);
    assert!(app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(session),
        reader_gate: None,
    }));
    let (instance, _) = app
        .active_native_view(wid)
        .expect("System State is the new window's view");
    assert_eq!(
        app.native_runtime.app(instance).map(|a| a.kind()),
        Some(crate::native_app::AppKind::SystemState)
    );
    assert!(crate::app_tabs::take_raised_windows().contains(&wid));

    let windows = app.windows.len();
    let (id, wid) = crate::tab_spawn::test_support::book_window(
        &mut app,
        None,
        SpawnAdmission::Plain,
        App::system_state_once_landed(),
    );
    assert!(!app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Err(std::io::Error::other("exec refused")),
        reader_gate: None,
    }));
    assert_eq!(app.windows.len(), windows, "no window, so nothing shown");
    assert!(!crate::app_tabs::take_raised_windows().contains(&wid));
}

/// The window's probe once a switch is on again: `enabled` or `check` off retired the cached
/// Full Disk Access answer, and the next read answers `fda`, as `Wake::ConsentProbeReady` would
/// publish it.
#[cfg(target_os = "macos")]
fn reread(fda: aterm_containment::consent::FdaState) -> crate::consent_state::ConsentState {
    crate::consent_state::ConsentState::with_cached_probe_for_test(probe(fda))
}

/// A `[privacy]` switch off at launch holds the access card's launch check with no verdict and
/// nothing queued. Once every switch is on, the check runs again with the Full Disk Access
/// hand-over undecided and its verdict queues the page for the step moment; the card's Not now
/// still answers it, and a decided check does not run again for a later toggle.
#[cfg(target_os = "macos")]
#[test]
fn a_switch_off_at_launch_holds_the_access_check_until_it_is_on() {
    use crate::consent_card::{CardPhase, Marker, PendingDecision};
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::{Origin, test_arms};
    use aterm_containment::consent::FdaState;
    for off in SWITCHES {
        set_front(true);
        let mut t = Instant::now();
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.set_privacy_for_test(switches(Some(off)));
        let opens = test_arms::fda_counts().0;
        parks(&mut app, &mut t, 3);
        assert_eq!(
            app.consent_card.phase(),
            CardPhase::Settled,
            "{off} off at launch: the check is held"
        );
        assert_eq!(app.devtools_helper.fda_queued(), None, "{off} off");
        assert_eq!(app.consent_launch.fda(), FdaLaunch::NotAsked, "{off} off");
        assert_eq!(test_arms::fda_counts().0, opens, "{off} off: nothing opens");

        // On again; the probe a switch off retired reads again
        // (`Wake::ConsentProbeReady`).
        app.consent = reread(FdaState::Denied);
        app.set_privacy_for_test(switches(None));
        app.resume_held_launch_steps(t);
        assert_eq!(
            app.consent_card.phase(),
            CardPhase::Idle,
            "{off} on again: the check runs again"
        );
        assert_eq!(
            app.consent_launch.fda(),
            FdaLaunch::Undecided,
            "{off} on again: what waits on it waits again"
        );
        // Its worker's verdict, as `Wake::MacosAccessCardDecided` brings it.
        assert!(app.consent_card.begin_deciding(t));
        app.decide_macos_access_card(PendingDecision::launch(None));
        assert_eq!(
            app.devtools_helper.fda_queued(),
            Some(Origin::Automatic),
            "{off} on again: the page is queued"
        );
        assert_eq!(app.consent_launch.fda(), FdaLaunch::Waiting, "{off}");
        set_front(false);
        parks(&mut app, &mut t, 2);
        assert_eq!(
            test_arms::fda_counts().0,
            opens,
            "{off}: not before the step moment"
        );
        set_front(true);
        parks(&mut app, &mut t, 1);
        assert_eq!(
            test_arms::fda_counts().0,
            opens + 1,
            "{off}: the page comes up"
        );
        assert!(app.fda_panel_up());

        // Decided: a switch off and on later does not run it twice.
        app.set_privacy_for_test(switches(Some(off)));
        parks(&mut app, &mut t, 1);
        app.set_privacy_for_test(switches(None));
        app.resume_held_launch_steps(t);
        assert_eq!(
            app.consent_card.phase(),
            CardPhase::Settled,
            "{off}: decided once"
        );

        // The access card's Not now answers the check run again, as at launch.
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.set_privacy_for_test(switches(Some(off)));
        parks(&mut app, &mut t, 1);
        app.consent = reread(FdaState::Denied);
        app.set_privacy_for_test(switches(None));
        app.resume_held_launch_steps(t);
        assert!(app.consent_card.begin_deciding(t));
        app.decide_macos_access_card(PendingDecision::launch(Some(Marker::NotNow)));
        assert_eq!(app.devtools_helper.fda_queued(), None, "{off}: Not now");
        let opens = test_arms::fda_counts().0;
        parks(&mut app, &mut t, 2);
        assert_eq!(test_arms::fda_counts().0, opens, "{off}: Not now holds");
    }
}

/// Once every switch is on, a re-run access check that can give no verdict (no worker starts
/// here; one that never answers runs out of attempts the same way) leaves the Full Disk Access
/// hand-over unasked, so the debugger step and the folders that wait on it go on.
#[cfg(target_os = "macos")]
#[test]
fn a_check_run_again_that_gives_no_verdict_holds_nothing_back() {
    use crate::consent_card::CardPhase;
    use crate::consent_launch::FdaLaunch;
    use crate::devtools_helper::test_arms;
    for off in SWITCHES {
        set_front(true);
        let mut t = Instant::now();
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.set_privacy_for_test(switches(Some(off)));
        parks(&mut app, &mut t, 2);
        assert!(app.consent_card_host.launch_held, "PRECONDITION: {off} off");
        app.consent = reread(aterm_containment::consent::FdaState::Denied);
        app.set_privacy_for_test(switches(None));
        parks(&mut app, &mut t, 1);
        assert_eq!(
            app.consent_card.phase(),
            CardPhase::Settled,
            "PRECONDITION: {off} on again: no worker, no verdict"
        );
        assert_eq!(
            app.consent_launch.fda(),
            FdaLaunch::NotAsked,
            "{off} on again: nothing waits on a verdict that cannot come"
        );
    }
}

/// `[privacy]` with the switch `off` turned off and the folder pass on
/// (`warmup` automatic).
#[cfg(target_os = "macos")]
fn walking(off: Option<&str>) -> crate::app_config::PrivacyConfig {
    crate::app_config::PrivacyConfig {
        warmup: None,
        ..switches(off)
    }
}

/// A switch off at launch holds the folder warm-up too: `enabled` or `check` off leaves it
/// unsettled, and it walks at the first step moment once every switch is on again.
#[cfg(target_os = "macos")]
#[test]
fn a_switch_off_at_launch_holds_the_folder_warm_up_until_it_is_on() {
    use crate::devtools_helper::test_arms;
    for off in ["enabled", "check"] {
        set_front(true);
        let mut t = Instant::now();
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.set_privacy_for_test(walking(Some(off)));
        parks(&mut app, &mut t, 3);
        assert!(
            !app.consent_launch.walking(),
            "{off} off at launch: no walk"
        );
        assert!(!app.consent_launch.settled(), "{off} off: it waits");

        app.consent = reread(aterm_containment::consent::FdaState::Denied);
        app.set_privacy_for_test(walking(None));
        parks(&mut app, &mut t, 1);
        assert!(
            app.consent_launch.walking(),
            "{off} on again: the folders are asked for"
        );
    }
}

/// After the Full Disk Access page's Not now, a `[privacy]` switch turned off holds the waiting
/// folder pass however many parks pass (`notice` at the step moment, `enabled` and `check`
/// before it), and the pass walks once every switch is on again.
#[cfg(target_os = "macos")]
#[test]
fn a_waiting_folder_pass_goes_on_after_a_switch() {
    use crate::devtools_helper::{PanelEvent, RECHECK_EVERY, test_arms};
    set_front(true);
    for off in SWITCHES {
        let mut t = Instant::now();
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.set_privacy_for_test(walking(None));
        assert!(app.consent_card.begin_deciding(t));
        app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
        t += RECHECK_EVERY;
        app.tick_consent_lanes(t);
        assert!(app.fda_panel_up(), "PRECONDITION: the page");
        app.devtools_helper.press_for_test(PanelEvent::NotNow);
        app.set_privacy_for_test(walking(Some(off)));
        parks(&mut app, &mut t, 3);
        assert!(!app.consent_launch.walking(), "{off} off: nothing asked");
        assert!(!app.consent_launch.settled(), "{off} off: it waits");
        app.consent = reread(aterm_containment::consent::FdaState::Denied);
        app.set_privacy_for_test(walking(None));
        parks(&mut app, &mut t, 3);
        assert!(app.consent_launch.walking(), "{off} on again: it walks");
    }
}

/// A switch off while the access check's worker reads settles the card, so the late verdict
/// finds it settled; the check runs again at the first park, the page comes once, and a new
/// approval brings it back. With every switch on (`None`, the control) the worker's own verdict
/// brings the page.
#[cfg(target_os = "macos")]
#[test]
fn a_switch_off_during_the_access_check_keeps_the_page() {
    use crate::consent_card::PendingDecision;
    use crate::devtools_helper::{PanelEvent, test_arms};
    use aterm_containment::consent::FdaState;
    set_front(true);
    for off in [None, Some("enabled"), Some("check"), Some("notice")] {
        let mut t = Instant::now();
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.set_privacy_for_test(switches(None));
        assert!(app.consent_card.begin_deciding(t));
        app.set_privacy_for_test(switches(off));
        parks(&mut app, &mut t, 1);
        app.consent = reread(FdaState::Denied);
        app.set_privacy_for_test(switches(None));
        app.decide_macos_access_card(PendingDecision::launch(None));
        if off.is_some() {
            assert_eq!(
                app.devtools_helper.fda_queued(),
                None,
                "{off:?}: the verdict finds the card settled"
            );
            // The first park runs the check again; its worker's verdict, as
            // `Wake::MacosAccessCardDecided` brings it.
            app.resume_held_launch_steps(t);
            assert!(app.consent_card.begin_deciding(t), "{off:?}: it runs again");
            app.decide_macos_access_card(PendingDecision::launch(None));
        }
        let opens = test_arms::fda_counts().0;
        parks(&mut app, &mut t, 3);
        assert_eq!(
            test_arms::fda_counts().0 - opens,
            1,
            "{off:?}: the page for revision 1"
        );
        app.devtools_helper.press_for_test(PanelEvent::NotNow);
        parks(&mut app, &mut t, 1);
        app.consent = reread(FdaState::Denied);
        approve(&mut app, 2);
        parks(&mut app, &mut t, 3);
        assert_eq!(
            test_arms::fda_counts().0 - opens,
            2,
            "{off:?}: a new approval asks again"
        );
    }
}

/// With the folder pass on, a switch off during the access check still brings the Full Disk
/// Access page first once it is on again, the folders waiting behind the check run again.
#[cfg(target_os = "macos")]
#[test]
fn after_a_switch_during_the_access_check_the_page_comes_before_the_folders() {
    use crate::consent_card::PendingDecision;
    use crate::devtools_helper::test_arms;
    use aterm_containment::consent::FdaState;
    set_front(true);
    for off in [None, Some("enabled"), Some("check"), Some("notice")] {
        let mut t = Instant::now();
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.set_privacy_for_test(walking(None));
        assert!(app.consent_card.begin_deciding(t));
        app.set_privacy_for_test(walking(off));
        parks(&mut app, &mut t, 1);
        app.consent = reread(FdaState::Denied);
        app.set_privacy_for_test(walking(None));
        if off.is_some() {
            app.resume_held_launch_steps(t);
            assert!(
                !app.consent_launch.walking(),
                "{off:?}: not before the check"
            );
            assert!(app.consent_card.begin_deciding(t));
        }
        app.decide_macos_access_card(PendingDecision::launch(None));
        let opens = test_arms::fda_counts().0;
        parks(&mut app, &mut t, 4);
        assert_eq!(test_arms::fda_counts().0 - opens, 1, "{off:?}: the page");
        assert!(app.fda_panel_up(), "{off:?}");
        assert!(
            !app.consent_launch.walking(),
            "{off:?}: no folder dialog over the page"
        );
    }
}

/// A switch off at launch holds the Developer Tools launch read too: with `enabled` or `check`
/// off at the first focus gain nothing is read and the read stays owed; once every switch is on,
/// the first park forgets the off reading and reads, and `not_granted` brings the page up at the
/// step moment.
#[cfg(target_os = "macos")]
#[test]
fn a_switch_off_at_launch_holds_the_developer_tools_read_until_it_is_on() {
    use crate::consent_state::SlotKind;
    use crate::devtools_helper::{Origin, test_arms};
    use aterm_containment::consent::FdaState;
    use aterm_containment::{DevToolProbe, DevToolProbeLabel, DevToolState, ProbeGate};
    use std::sync::atomic::{AtomicUsize, Ordering};
    static READS: AtomicUsize = AtomicUsize::new(0);
    fn not_on(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
        READS.fetch_add(1, Ordering::AcqRel);
        DevToolProbe {
            state: DevToolState::Unlisted,
            label: DevToolProbeLabel::Observed,
            requests: Some(1),
            not_allowed: Some(1),
            answered_at_ms: Some(1),
        }
    }
    fn subject() -> Option<&'static str> {
        Some(test_arms::SUBJECT)
    }
    for off in ["enabled", "check"] {
        set_front(true);
        let mut t = Instant::now();
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        // Full Disk Access reads on: the Developer Tools page is the next step.
        // A switch off retires that answer, and the probe reads it again once
        // every switch is on.
        let (tx, rx) = std::sync::mpsc::channel();
        let consent = || {
            let mut consent = reread(FdaState::Granted);
            consent.set_devtools_arms_for_test(not_on, subject);
            let tx = tx.clone();
            consent.set_slot_wake(std::sync::Arc::new(move |kind: SlotKind| {
                if kind == SlotKind::Devtools {
                    let _ = tx.send(());
                }
            }));
            consent
        };
        app.consent = consent();
        focus(&mut app, true);
        assert!(
            app.any_os_window_focused(),
            "PRECONDITION: a focused window"
        );
        app.set_privacy_for_test(switches(Some(off)));
        let reads = READS.load(Ordering::Acquire);
        // The launch's first focus gain, and a reading taken while it is off.
        app.read_devtools_list(t);
        let _ = app.devtools_reading();
        assert_eq!(
            app.devtools_helper
                .reading()
                .map(|reading| reading.log.label),
            Some(DevToolProbeLabel::RefusedDisabled),
            "PRECONDITION: {off} off reads that it is off"
        );
        parks(&mut app, &mut t, 3);
        assert_eq!(READS.load(Ordering::Acquire), reads, "{off} off: no read");
        assert_eq!(app.devtools_helper.queued(), None, "{off} off");

        app.consent = consent();
        app.set_privacy_for_test(switches(None));
        parks(&mut app, &mut t, 1);
        assert_ne!(
            app.devtools_helper
                .reading()
                .map(|reading| reading.log.label),
            Some(DevToolProbeLabel::RefusedDisabled),
            "{off} on again: the off reading decides nothing"
        );
        rx.recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("{off} on again: the launch read runs"));
        assert_eq!(READS.load(Ordering::Acquire), reads + 1, "{off}: one read");
        // Its completion, as `Wake::DevToolsProbeReady` brings it.
        app.on_devtools_probe_ready();
        assert_eq!(
            app.devtools_helper.queued(),
            Some(Origin::Automatic),
            "{off}: not_granted asks for the page"
        );
        let opens = test_arms::opens();
        parks(&mut app, &mut t, 1);
        assert_eq!(test_arms::opens(), opens + 1, "{off}: the page comes up");
    }
}

/// A Developer Tools launch read whose completion lands after `enabled` or `check` went off is
/// owed again: once every switch is on, the first park with a focused window reads once more and
/// `not_granted` asks for the page. Without `hold_read` in `App::resume_held_launch_steps`,
/// nothing reads again.
#[cfg(target_os = "macos")]
#[test]
fn a_launch_read_that_lands_while_a_switch_is_off_is_made_again() {
    use crate::consent_state::SlotKind;
    use crate::devtools_helper::{Origin, test_arms};
    use aterm_containment::consent::FdaState;
    use aterm_containment::{DevToolProbe, DevToolProbeLabel, DevToolState, ProbeGate};
    use std::sync::atomic::{AtomicUsize, Ordering};
    static READS: AtomicUsize = AtomicUsize::new(0);
    fn not_on(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
        READS.fetch_add(1, Ordering::AcqRel);
        DevToolProbe {
            state: DevToolState::Unlisted,
            label: DevToolProbeLabel::Observed,
            requests: Some(1),
            not_allowed: Some(1),
            answered_at_ms: Some(1),
        }
    }
    fn subject() -> Option<&'static str> {
        Some(test_arms::SUBJECT)
    }
    for off in ["enabled", "check"] {
        set_front(true);
        let mut t = Instant::now();
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        let (tx, rx) = std::sync::mpsc::channel();
        let consent = || {
            let mut consent = reread(FdaState::Granted);
            consent.set_devtools_arms_for_test(not_on, subject);
            let tx = tx.clone();
            consent.set_slot_wake(std::sync::Arc::new(move |kind: SlotKind| {
                if kind == SlotKind::Devtools {
                    let _ = tx.send(());
                }
            }));
            consent
        };
        app.consent = consent();
        focus(&mut app, true);
        app.set_privacy_for_test(switches(None));
        let reads = READS.load(Ordering::Acquire);
        // The launch's first focus gain, every switch on: the read goes out.
        app.read_devtools_list(t);
        rx.recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("{off}: PRECONDITION the launch read runs"));
        assert_eq!(READS.load(Ordering::Acquire), reads + 1, "PRECONDITION");
        // The switch goes off before `Wake::DevToolsProbeReady` is handled.
        app.set_privacy_for_test(switches(Some(off)));
        app.on_devtools_probe_ready();
        assert_eq!(
            app.devtools_helper
                .reading()
                .map(|reading| reading.log.label),
            Some(DevToolProbeLabel::RefusedDisabled),
            "PRECONDITION: {off} off: the completion reads that it is off"
        );
        // On again: the first park makes the read again.
        app.consent = consent();
        app.set_privacy_for_test(switches(None));
        parks(&mut app, &mut t, 1);
        rx.recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("{off} on again: the spent read is made again"));
        assert_eq!(READS.load(Ordering::Acquire), reads + 2, "{off}: once more");
        app.on_devtools_probe_ready();
        assert_eq!(
            app.devtools_helper.queued(),
            Some(Origin::Automatic),
            "{off}: not_granted asks for the page"
        );
        // Once: later parks and a focus gain read no more.
        parks(&mut app, &mut t, 3);
        app.read_devtools_list(t);
        assert_eq!(READS.load(Ordering::Acquire), reads + 2, "{off}: once");
    }
}

/// A switch toggled twice while the re-run access check is out starts a second worker; the two
/// verdicts bring one Full Disk Access page, and nothing is left held.
#[cfg(target_os = "macos")]
#[test]
fn a_switch_toggled_twice_brings_the_page_once() {
    use crate::consent_card::{CardPhase, PendingDecision};
    use crate::devtools_helper::test_arms;
    use aterm_containment::consent::FdaState;
    set_front(true);
    for off in SWITCHES {
        let mut t = Instant::now();
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.set_privacy_for_test(switches(Some(off)));
        parks(&mut app, &mut t, 2);
        assert!(app.consent_card_host.launch_held, "PRECONDITION: {off}");
        app.consent = reread(FdaState::Denied);
        app.set_privacy_for_test(switches(None));
        app.resume_held_launch_steps(t);
        assert!(app.consent_card.begin_deciding(t), "{off}: worker A");
        app.set_privacy_for_test(switches(Some(off)));
        parks(&mut app, &mut t, 1);
        assert!(app.consent_card_host.launch_held, "{off}: held again");
        app.consent = reread(FdaState::Denied);
        app.set_privacy_for_test(switches(None));
        app.resume_held_launch_steps(t);
        assert_eq!(app.consent_card.phase(), CardPhase::Idle, "{off}");
        assert!(app.consent_card.begin_deciding(t), "{off}: worker B");
        let opens = test_arms::fda_counts().0;
        app.decide_macos_access_card(PendingDecision::launch(None));
        app.decide_macos_access_card(PendingDecision::launch(None));
        parks(&mut app, &mut t, 4);
        assert_eq!(test_arms::fda_counts().0 - opens, 1, "{off}: one page");
        assert!(!app.consent_card_host.launch_held, "{off}: nothing held");
    }
}
