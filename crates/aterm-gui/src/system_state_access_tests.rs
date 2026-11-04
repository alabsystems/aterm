// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! System State's access steps: the Local Network trigger's target, the menu
//! line, the step gating and a Needs-you row's press. Nothing reaches a LAN, a
//! person's state directory or System Settings. The dialog-ownership regressions use
//! the real process-wide guard, without raising a dialog.

use std::cell::RefCell;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_types::macos_setup::{
    Access, AccessOutcome, Observation, Plan, Selection, Setting, Standing, Subject,
};

use super::{
    JOIN_PENDING_KEY, LOCAL_NETWORK_ALERT_HOLD, LanHere, LanInterface, RETIRED_KEYS, StepsReach,
    lan_target, menu_line, setup_field, watch_network_moves,
};
use crate::status_item::SystemStateLine;
use crate::{App, InitialSetupPermissionPolicy, SetupStatus};

#[cfg(target_os = "macos")]
#[path = "system_state_access/not_now_retry_tests.rs"]
mod not_now_retry_tests;

#[cfg(target_os = "macos")]
#[path = "system_state_access/permission_walk_tests.rs"]
mod permission_walk_tests;

#[cfg(target_os = "macos")]
#[path = "system_state_access/current_revision_tests.rs"]
mod current_revision_tests;

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

/// The Local Network target: an attached subnet's first host (its second when the first is this
/// Mac), private first, then link-local, then any other (a public one), and none on a subnet
/// with no room for another host, a mask outside /8–/30 or a tunnel (no broadcast: a VPN's
/// `utun`).
#[test]
fn the_lan_target_is_an_attached_subnets_other_host_private_first() {
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
        "private before public"
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
        lan_target(&[
            lan(ip(17, 5, 7, 8), slash24),
            lan(ip(169, 254, 3, 4), ip(255, 255, 0, 0))
        ]),
        Some(ip(169, 254, 0, 1)),
        "link-local before public"
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
        (tunnel, "a tunnel only"),
    ] {
        assert_eq!(lan_target(&[interface]), None, "{why}");
    }
    assert_eq!(lan_target(&[]), None);
}

/// TN3179 counts any host on a subnet attached to one of the Mac's interfaces as local network,
/// private or not: a campus LAN that hands out public IPv4 addresses is a network the step asks
/// macOS about with a connect to its subnet, not one with "nothing to ask".
#[test]
fn a_public_ipv4_lan_is_still_a_local_network_to_ask_about() {
    use super::{LanAsk, lan_ask};
    let campus = LanInterface {
        address: ip(128, 30, 2, 17),
        mask: ip(255, 255, 255, 0),
        broadcast: true,
    };
    assert_eq!(
        lan_target(&[campus]),
        Some(ip(128, 30, 2, 1)),
        "a broadcast LAN on a public /24 is a local network to macOS"
    );
    assert_eq!(
        lan_ask(&[campus], false),
        Some(LanAsk::Connect(ip(128, 30, 2, 1)))
    );
    assert_eq!(
        lan_ask(&[campus], true),
        Some(LanAsk::Connect(ip(128, 30, 2, 1))),
        "an IPv6 network beside it changes nothing"
    );
}

/// A network with no IPv4 subnet to name (IPv6 alone, a /31 or /32 link, an IPv6 network under
/// a VPN's tunnel) still has macOS's Local Network question to ask: the step asks it through a
/// short Bonjour browse of its own. Only a Mac on no network at all has nothing to ask with.
#[test]
fn an_ipv6_only_network_asks_through_a_bonjour_browse_of_the_steps_own() {
    use super::{LanAsk, lan_ask};
    let tunnel = LanInterface {
        broadcast: false,
        ..lan(ip(10, 8, 0, 2), ip(255, 255, 255, 0))
    };
    let point = lan(ip(10, 0, 0, 4), ip(255, 255, 255, 254));
    let browse: [(&[LanInterface], bool, &str); 4] = [
        (&[], true, "an IPv6-only network"),
        (&[tunnel], true, "an IPv6 network under a VPN's tunnel"),
        (&[point], false, "a /31 link"),
        (&[point], true, "a /31 link beside IPv6"),
    ];
    for (interfaces, ipv6, why) in browse {
        assert_eq!(lan_ask(interfaces, ipv6), Some(LanAsk::Browse), "{why}");
    }
    let none: [(&[LanInterface], &str); 2] = [
        (&[], "no network at all"),
        (&[tunnel], "a VPN's tunnel and no network"),
    ];
    for (interfaces, why) in none {
        assert_eq!(lan_ask(interfaces, false), None, "{why}");
    }
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

/// `status`'s `setup=` names each managed row a person, a wait or a failure still holds, from the
/// worker's last status alone; a finished plan with none left is `done`, and a row a finished
/// setup set once that differs now is the person's, not pending.
#[test]
fn setup_field_names_what_is_left_and_done_once_the_plan_finished() {
    let fda = Subject::Access(Access::FullDiskAccess);
    let devtools = Subject::Access(Access::DeveloperTools);
    let accessibility = Subject::Access(Access::Accessibility);
    let vault = Subject::Setting(Setting::Secrets);
    let dock = Subject::Setting(Setting::Dock);
    assert_eq!(setup_field(&SetupStatus::default()), "-", "no worker spoke");
    let mut permissions_and_vault = Selection::none();
    permissions_and_vault.set(Setting::Permissions, true);
    permissions_and_vault.set(Setting::Secrets, true);
    permissions_and_vault.set(Setting::Dock, true);
    let pending = status(
        permissions_and_vault,
        vec![
            observed(fda, Standing::NeedsYou, None),
            observed(devtools, Standing::Waiting, None),
            observed(vault, Standing::Failed, None),
            observed(dock, Standing::InPlace, None),
            // Accessibility is unmanaged without a sign-out.
            observed(accessibility, Standing::NeedsYou, None),
        ],
    );
    // `;needs:` names only the rows that wait on a press: Developer Tools waits and the vault
    // failed, so neither is one.
    assert_eq!(
        setup_field(&pending),
        "pending:fda,devtools,vault;needs:fda"
    );
    let finished = SetupStatus {
        finished: true,
        ..status(
            permissions_and_vault,
            vec![
                observed(fda, Standing::InPlace, None),
                observed(devtools, Standing::NotNeeded, None),
                observed(vault, Standing::InPlace, None),
                observed(dock, Standing::Differs, None),
            ],
        )
    };
    assert_eq!(setup_field(&finished), "done");
    let running = status(
        permissions_and_vault,
        vec![observed(dock, Standing::Differs, None)],
    );
    assert_eq!(
        setup_field(&running),
        "pending:dock",
        "still to set while the run goes"
    );
}

/// A run parked to ask again by itself reads `done`, nothing being left for anyone to do now,
/// and names what it still asks for (`;asking:`), so `done` is never read as a finished record;
/// grants on offer ride behind it without making it pending.
#[test]
fn setup_field_names_what_a_parked_run_asks_for_and_what_is_on_offer() {
    use aterm_types::macos_setup::{ACCESS_OFFERED, SECRETS_VAULT_ROW};
    let mut selection = Selection::none();
    selection.set(Setting::Permissions, true);
    selection.set(Setting::Secrets, true);
    let debugger = Subject::Access(Access::Debugger);
    let vault = Subject::Setting(Setting::Secrets);
    let offer = observed(debugger, Standing::NotNeeded, Some(ACCESS_OFFERED));
    let finished = SetupStatus {
        finished: true,
        ..status(
            selection,
            vec![offer.clone(), observed(vault, Standing::InPlace, None)],
        )
    };
    assert_eq!(setup_field(&finished), "done;offered:debugger");
    let no_offer = SetupStatus {
        finished: true,
        ..status(selection, vec![observed(vault, Standing::InPlace, None)])
    };
    assert_eq!(setup_field(&no_offer), "done", "CONTROL: nothing on offer");
    // A vault that did not answer (`unreached`), or none on this network (`waiting`), on a Mac
    // off the ALab LAN: the headline counts it settled and the run asks again by itself.
    for parked_as in ["unreached", "waiting"] {
        let mut parked = crate::system_state::fixture_status("non-alab").unwrap();
        assert!(!parked.finished);
        parked.rows = vec![crate::system_state::SetupStatusRow {
            name: SECRETS_VAULT_ROW.to_owned(),
            status: parked_as.to_owned(),
            detail: "The ALab vault did not answer on this network".to_owned(),
        }];
        assert_eq!(setup_field(&parked), "done;asking:vault", "{parked_as}");
        let offered = parked
            .observed
            .iter_mut()
            .find(|seen| seen.subject == debugger)
            .unwrap();
        *offered = offer.clone();
        assert_eq!(
            setup_field(&parked),
            "done;asking:vault;offered:debugger",
            "{parked_as}"
        );
    }
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
    assert!(!held.person_permission_admission_stands());
}

/// The Local Network step: nothing before an approved revision; then once per revision, first,
/// recorded `triggered`, its alert counted as up for its hold, the person's 30 s with it from the
/// first look after the ask (no later step meanwhile); a new revision asks again.
#[test]
fn the_local_network_step_runs_first_and_once_per_approved_revision() {
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(stepping());
    // The person at the Mac throughout.
    app.user_input_recent = |_| true;
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
        None,
        "a step that has not asked macOS is not a request for a person"
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
        Some(now + crate::devtools_helper::RECHECK_EVERY),
        "a look to find the person since the ask"
    );
    app.tick_local_network_step(now);
    assert_eq!(
        app.next_access_step_deadline(now),
        Some(now + LOCAL_NETWORK_ALERT_HOLD),
        "the person's time with the alert counts from that look"
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

#[test]
fn a_recorded_lan_ask_rechecks_and_recovers_without_system_state_or_focus() {
    use super::{LOCAL_NETWORK_RETRY, LocalNetworkProbe};
    use std::cell::Cell;
    thread_local! {
        static PRIOR: Cell<Option<AccessOutcome>> = const { Cell::new(None) };
        static CURRENT: Cell<Option<AccessOutcome>> = const { Cell::new(None) };
        static STARTS: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
        static OUTCOME: Cell<Option<(u64, LocalNetworkProbe)>> = const { Cell::new(None) };
    }
    for (current, prior) in [
        (None, Some(AccessOutcome::Triggered)),
        (None, Some(AccessOutcome::Granted)),
        (Some(AccessOutcome::Triggered), None),
        (Some(AccessOutcome::Granted), None),
        (None, None),
    ] {
        PRIOR.with(|slot| slot.set(prior));
        CURRENT.with(|slot| slot.set(current));
        STARTS.with(|slot| slot.borrow_mut().clear());
        OUTCOME.with(|slot| slot.set(None));
        let mut app = App::headless_for_test();
        app.access_steps.set_reach(StepsReach {
            local_network_recorded_before: || PRIOR.with(Cell::get),
            recorded: |_, access| {
                (access == Access::LocalNetwork)
                    .then(|| CURRENT.with(Cell::get))
                    .flatten()
            },
            start_local_network_probe: |revision| {
                STARTS.with(|slot| slot.borrow_mut().push(revision))
            },
            take_local_network_probe: || OUTCOME.with(Cell::take),
            frontmost: |_| false,
            quiet: |_, _| false,
            ..stepping()
        });
        focus(&mut app, false);
        approve(&mut app, 91);
        let before = triggers();
        let now = Instant::now();
        if prior.is_none() && current.is_none() {
            app.tick_local_network_step(now);
            assert!(STARTS.with(|slot| slot.borrow().is_empty()));
            // A grant arriving after a cached negative record read must wake progress too.
            app.publish_system_state(SetupStatus {
                observed: vec![observed(
                    Subject::Access(Access::LocalNetwork),
                    Standing::InPlace,
                    None,
                )],
                ..SetupStatus::default()
            });
        }
        app.tick_local_network_step(now);
        app.tick_local_network_step(now);
        assert_eq!(STARTS.with(|slot| slot.borrow().clone()), [91]);
        assert_eq!(triggers(), before, "no new OS question");
        assert!(!app.local_network_pending());
        assert_eq!(app.system_state_access_step(), None);
        assert!(app.access_steps.local_network_alert.is_none());

        // No peer answered the first bounded check. Its retry must wake without focus.
        OUTCOME.with(|slot| slot.set(Some((91, LocalNetworkProbe::Unanswered))));
        app.on_local_network_probe_ready(now);
        assert_eq!(
            app.next_access_step_deadline(now),
            Some(now + LOCAL_NETWORK_RETRY)
        );
        app.tick_local_network_step(now + LOCAL_NETWORK_RETRY);
        assert_eq!(STARTS.with(|slot| slot.borrow().clone()), [91, 91]);
        assert_eq!(triggers(), before);
        OUTCOME.with(|slot| slot.set(Some((91, LocalNetworkProbe::Granted))));
        app.on_local_network_probe_ready(now + LOCAL_NETWORK_RETRY);
        assert_eq!(
            records().last(),
            Some(&(91, Access::LocalNetwork, AccessOutcome::Granted))
        );
        assert!(app.access_steps.local_network_recheck.is_none());
        assert_eq!(app.system_state_access_step(), None);
    }
}

#[test]
fn an_unfocused_unasked_lan_step_does_not_raise_or_claim_a_permission_question() {
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        start_local_network_probe: |_| panic!("no prior ask authorizes a probe"),
        frontmost: |_| false,
        quiet: |_, _| false,
        ..stepping()
    });
    focus(&mut app, false);
    approve(&mut app, 92);
    let before = triggers();
    app.tick_local_network_step(Instant::now());
    assert_eq!(triggers(), before);
    assert!(app.local_network_pending());
    assert_eq!(app.system_state_access_step(), None);
}

#[test]
fn a_background_lan_recheck_respects_privacy_holds_and_a_refused_answer() {
    use super::LocalNetworkProbe;
    use std::cell::Cell;
    thread_local! {
        static STARTS: Cell<usize> = const { Cell::new(0) };
        static OUTCOME: Cell<Option<(u64, LocalNetworkProbe)>> = const { Cell::new(None) };
    }
    for off in SWITCHES {
        STARTS.with(|slot| slot.set(0));
        OUTCOME.with(|slot| slot.set(None));
        let mut app = App::headless_for_test();
        app.access_steps.set_reach(StepsReach {
            local_network_recorded_before: || Some(AccessOutcome::Triggered),
            start_local_network_probe: |_| STARTS.with(|slot| slot.set(slot.get() + 1)),
            take_local_network_probe: || OUTCOME.with(Cell::take),
            frontmost: |_| false,
            quiet: |_, _| false,
            ..stepping()
        });
        app.config.privacy = Some(switches(Some(off)));
        approve(&mut app, 93);
        let now = Instant::now();
        let before = records().len();
        app.tick_local_network_step(now);
        assert_eq!(STARTS.with(Cell::get), 0, "{off} holds the check");
        assert_eq!(records().len(), before);
        assert_eq!(app.next_access_step_deadline(now), None);
        app.config.privacy = Some(switches(None));
        let resumed = Instant::now() + Duration::from_secs(1);
        app.tick_local_network_step(resumed);
        assert_eq!(STARTS.with(Cell::get), 1, "resumes without focus");
        OUTCOME.with(|slot| slot.set(Some((93, LocalNetworkProbe::Refused))));
        app.on_local_network_probe_ready(resumed);
        assert_eq!(records().len(), before, "a refusal never records Granted");
        assert_eq!(app.next_access_step_deadline(resumed), None);
        app.tick_local_network_step(resumed + Duration::from_secs(300));
        assert_eq!(STARTS.with(Cell::get), 1, "no retry against a refusal");
    }
}

/// Once macOS's step started, a failed record write is still owed after the disk recovers:
/// normal turns save that same outcome without starting another macOS step.
#[test]
fn a_local_network_step_recovers_its_record_without_triggering_again() {
    use aterm_types::macos_setup::access_record;

    access_record::code_subject_from(|| Some([0x54; 32]));

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
    // The person at the Mac throughout: their time with the alert counts from the first look
    // after the ask.
    app.user_input_recent = |_| true;
    app.tick_local_network_step(now);
    let later = now + LOCAL_NETWORK_ALERT_HOLD;
    app.tick_local_network_step(later - Duration::from_secs(1));
    app.settle_access_steps(later - Duration::from_secs(1));
    assert!(macos_dialog_up(), "the hold stands for the person's 30 s");
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

thread_local! {
    /// Each hand-over of the LAN to the Commons runtime: its revision, and whether a macOS dialog
    /// of this process counted as up then.
    static HANDED: RefCell<Vec<(u64, bool)>> = const { RefCell::new(Vec::new()) };
}

fn handed() -> Vec<(u64, bool)> {
    HANDED.with(|handed| handed.borrow().clone())
}

/// The Local Network step hands this process's LAN to the Commons runtime once its trigger asked
/// macOS, while its own dialog slot is held (`docs/DESIGN-system-state-2026-10-21.md` §6, step 1),
/// so Commons' first LAN use never raises the question first: never before the step's moment,
/// never twice, never for a trigger that asked nothing (only for the look once the network moved
/// that does), and never for a revision the record already holds.
#[test]
fn the_local_network_step_hands_the_lan_to_commons_under_its_slot() {
    use aterm_types::macos_setup::macos_dialog_up;

    let _serial = DIALOG_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(!macos_dialog_up());
    HANDED.with(|handed| handed.borrow_mut().clear());
    let reach = StepsReach {
        hand_lan_over: |revision| {
            HANDED.with(|handed| handed.borrow_mut().push((revision, macos_dialog_up())));
        },
        dialog_up: macos_dialog_up,
        holds_dialog: true,
        ..stepping()
    };
    let now = Instant::now();
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        quiet: |_, _| false,
        ..reach
    });
    approve(&mut app, 51);
    app.tick_local_network_step(now);
    assert_eq!(handed(), [], "not before the step's moment");

    app.access_steps.set_reach(reach);
    let recorded = records().len();
    app.tick_local_network_step(now);
    assert_eq!(handed(), [(51, true)], "handed over under the step's slot");
    assert_eq!(
        records()[recorded..],
        [(51, Access::LocalNetwork, AccessOutcome::Triggered)]
    );
    app.settle_access_steps(now);
    let later = now + LOCAL_NETWORK_ALERT_HOLD;
    app.tick_local_network_step(later);
    app.settle_access_steps(later);
    app.tick_local_network_step(later + super::LOCAL_NETWORK_RETRY);
    assert_eq!(handed(), [(51, true)], "once");
    drop(app);
    assert!(!macos_dialog_up());

    // A trigger that asked nothing hands nothing over; the look once the network moved, which
    // finds a LAN, does, once.
    HANDED.with(|handed| handed.borrow_mut().clear());
    NETWORK_WATCHES.with(|watches| watches.borrow_mut().clear());
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        trigger_local_network: no_lan,
        watch_network: watching,
        ..reach
    });
    approve(&mut app, 52);
    app.tick_local_network_step(now);
    assert_eq!(handed(), [], "no LAN was named");
    assert!(!macos_dialog_up());
    app.access_steps.set_reach(reach);
    network_moves();
    let retry = now + crate::devtools_helper::RECHECK_EVERY;
    app.tick_local_network_step(retry);
    assert_eq!(handed(), [(52, true)], "the look after the move asked");
    app.settle_access_steps(retry);
    let after = retry + LOCAL_NETWORK_ALERT_HOLD;
    app.tick_local_network_step(after);
    app.settle_access_steps(after);
    app.tick_local_network_step(after + super::LOCAL_NETWORK_RETRY);
    assert_eq!(handed(), [(52, true)], "once");
    drop(app);
    assert!(!macos_dialog_up());

    // A revision the record holds is done: nothing asked, nothing handed over.
    HANDED.with(|handed| handed.borrow_mut().clear());
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        recorded: |_, _| Some(AccessOutcome::Triggered),
        ..reach
    });
    approve(&mut app, 53);
    app.tick_local_network_step(now);
    assert_eq!(handed(), [], "on record");
    drop(app);
    assert!(!macos_dialog_up());
}

/// A first LAN use of this window asked macOS through the door past the wait for the step
/// (System State behind a terminal tab for a minute of the person's time): the step reads that
/// ask as its question, whether or not it is the step's moment. It records `triggered`, raises
/// nothing, hands nothing over and holds no alert of its own, since the door holds the dialog
/// slot for the question's hold: one question, one hold. NEGATIVE CONTROL: with no door ask the
/// step outside its moment does nothing.
#[test]
fn the_local_network_step_reads_a_door_ask_as_its_question() {
    use aterm_types::macos_setup::macos_dialog_up;

    let _serial = DIALOG_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    HANDED.with(|handed| handed.borrow_mut().clear());
    let reach = StepsReach {
        hand_lan_over: |revision| {
            HANDED.with(|handed| handed.borrow_mut().push((revision, macos_dialog_up())));
        },
        dialog_up: macos_dialog_up,
        holds_dialog: true,
        quiet: |_, _| false,
        ..stepping()
    };
    let now = Instant::now();
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(reach);
    approve(&mut app, 61);
    let (recorded, triggered) = (records().len(), triggers());
    app.tick_local_network_step(now);
    assert_eq!(
        records().len(),
        recorded,
        "not the step's moment, nothing asked"
    );
    assert_eq!(app.access_steps.local_network_done, None);

    app.access_steps.set_reach(StepsReach {
        door_asked: || true,
        ..reach
    });
    app.tick_local_network_step(now);
    assert_eq!(
        records()[recorded..],
        [(61, Access::LocalNetwork, AccessOutcome::Triggered)],
        "the door's ask is the step's question"
    );
    assert_eq!(
        triggers(),
        triggered,
        "the step raised no question of its own"
    );
    assert_eq!(handed(), [], "and handed nothing over");
    assert!(
        app.access_steps.local_network_alert.is_none(),
        "no hold beside the door's"
    );
    assert!(!macos_dialog_up(), "the step took no slot");
    assert_eq!(app.access_steps.local_network_done, Some(61));
    app.tick_local_network_step(now + LOCAL_NETWORK_ALERT_HOLD);
    assert_eq!(records().len(), recorded + 1, "once");
}

/// A first LAN use asked macOS through the door, and the step, reading that ask as its question,
/// started the LAN re-probe: once the re-probe hears the answer (a LAN server answered, or macOS
/// refused a `.local` lookup), the door's hold of the question ends, so a walk waiting on the
/// dialog slot goes on. A re-probe that heard nothing in its 30 s, or another revision's, ends
/// nothing: the hold is left to the person's time or the door's watch. NEGATIVE CONTROLS: with
/// the `end_door_hold` call taken out of `on_local_network_probe_ready` no answer ends the
/// door's hold; with it moved out of the `probe_heard_an_answer` branch an unanswered re-probe
/// ends it.
#[test]
fn the_re_probe_hearing_the_answer_ends_the_doors_hold() {
    use super::LocalNetworkProbe;
    use std::cell::Cell;

    type Heard = Option<(u64, LocalNetworkProbe)>;
    thread_local! {
        static OUTCOME: Cell<Heard> = const { Cell::new(None) };
        static PROBES: Cell<u32> = const { Cell::new(0) };
        static ENDED: Cell<u32> = const { Cell::new(0) };
    }
    let reach = StepsReach {
        door_asked: || true,
        start_local_network_probe: |_| PROBES.with(|probes| probes.set(probes.get() + 1)),
        take_local_network_probe: || OUTCOME.with(Cell::take),
        end_door_hold: || {
            ENDED.with(|ended| ended.set(ended.get() + 1));
            true
        },
        ..stepping()
    };
    let now = Instant::now();
    for (probe, ends) in [
        (LocalNetworkProbe::Granted, 1),
        (LocalNetworkProbe::Refused, 1),
        (LocalNetworkProbe::Unanswered, 0),
    ] {
        let mut app = App::headless_for_test();
        app.access_steps.set_reach(reach);
        approve(&mut app, 71);
        PROBES.with(|probes| probes.set(0));
        ENDED.with(|ended| ended.set(0));
        app.tick_local_network_step(now);
        assert_eq!(
            PROBES.with(Cell::get),
            1,
            "{probe:?}: the step read the door's ask and started the re-probe"
        );
        OUTCOME.with(|outcome| outcome.set(Some((70, probe))));
        app.on_local_network_probe_ready(now + Duration::from_secs(1));
        assert_eq!(ENDED.with(Cell::get), 0, "{probe:?}: another revision's");
        OUTCOME.with(|outcome| outcome.set(Some((71, probe))));
        app.on_local_network_probe_ready(now + Duration::from_secs(31));
        assert_eq!(ENDED.with(Cell::get), ends, "{probe:?}");
    }
}

/// The `[privacy]` switches that hold System State's steps, and the Local Network step's reason
/// not to run that the Commons runtime reads: each switch off holds them; `[swarm] enabled =
/// false` holds only the Local Network step; the defaults hold nothing.
#[test]
fn a_privacy_switch_or_swarm_off_says_why_the_local_network_step_will_not_run() {
    use crate::app_config::{Config, LOCAL_NETWORK_STEP_PRIVACY_OFF, LOCAL_NETWORK_STEP_SWARM_OFF};
    let read = |text: &str| aterm_toml::from_str::<Config>(text).unwrap();
    let defaults = read("");
    assert!(defaults.access_steps_allowed());
    assert_eq!(defaults.local_network_step_off(), None);
    for switch in ["enabled", "check", "notice"] {
        let config = read(&format!("[privacy]\n{switch} = false\n"));
        assert!(!config.access_steps_allowed(), "{switch}");
        assert_eq!(
            config.local_network_step_off(),
            Some(LOCAL_NETWORK_STEP_PRIVACY_OFF),
            "{switch}"
        );
        let on = read(&format!("[privacy]\n{switch} = true\n"));
        assert!(on.access_steps_allowed(), "{switch} on");
    }
    let swarm_off = read("[swarm]\nenabled = false\n");
    assert!(swarm_off.access_steps_allowed());
    assert_eq!(
        swarm_off.local_network_step_off(),
        Some(LOCAL_NETWORK_STEP_SWARM_OFF)
    );
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
            crate::devtools_helper::Page::Accessibility => unreachable!("only the two pages"),
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
    for origin in [Origin::Automatic, Origin::SystemStateAct, Origin::Ctl] {
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
            crate::devtools_helper::Page::Accessibility => unreachable!("only the two pages"),
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
        crate::devtools_helper::Page::Accessibility => unreachable!("only the two pages"),
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
        crate::devtools_helper::Page::Accessibility => unreachable!("only the two pages"),
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

/// A Local Network step held from its moment says in aterm.log what holds it, once per change of
/// that set and never once per look, on an instance whose step can run: the revision, which part
/// of System State in front fails, and, with System State in front, which part of the quiet
/// moment. A lasting hold (no approved revision) is said alone, and System State out of view in
/// the window last focused is said alone too, so keys typed and focus moved meanwhile say nothing
/// more. Its moment coming clears it, so a later hold is said again.
/// NEGATIVE CONTROL: the same App with an instance's inert arms says nothing at all.
#[test]
fn a_held_local_network_step_names_its_hold_once_per_change() {
    let fired = triggers();
    let mut app = App::headless_for_test();
    app.devtools_helper.note_cold_start();
    let now = Instant::now() + crate::devtools_helper::COLD_START_SETTLE;
    let names = StepsReach {
        frontmost: App::system_state_frontmost,
        quiet: App::step_moment_is_quiet,
        names_holds: true,
        ..stepping()
    };
    app.access_steps.set_reach(StepsReach {
        names_holds: false,
        ..names
    });
    focus(&mut app, false);
    app.user_input_recent = |_| true;
    app.last_keystroke_at = Some(now);
    app.tick_local_network_step(now);
    assert_eq!(
        app.note_local_network_hold(now, None),
        None,
        "control: an instance whose step cannot run says nothing"
    );
    assert_eq!(app.access_steps.local_network_held, None);

    app.access_steps.set_reach(names);
    let line = app
        .note_local_network_hold(now, None)
        .expect("a step with no approved revision waits");
    assert_eq!(
        line,
        "system state: the Local Network step waits: no approved setup revision admits the \
         Permissions group",
        "a lasting hold is said alone"
    );
    focus(&mut app, true);
    app.last_keystroke_at = None;
    assert_eq!(
        app.note_local_network_hold(now, None),
        None,
        "the same hold is said once, whatever the keys and focus do meanwhile"
    );

    approve(&mut app, 5);
    app.last_keystroke_at = Some(now);
    assert!(app.open_system_state_tab_quietly(), "System State behind");
    app.tick_local_network_step(now);
    assert_eq!(triggers(), fired, "held: nothing asked");
    assert_eq!(
        app.access_steps.local_network_held.as_deref(),
        Some(&["System State is not in view in the aterm window last focused".to_owned()][..]),
        "the revision no longer holds it; out of view, only that is said"
    );
    for (focused, typed) in [(false, None), (true, Some(now)), (false, Some(now))] {
        focus(&mut app, focused);
        app.last_keystroke_at = typed;
        assert_eq!(
            app.note_local_network_hold(now, Some(5)),
            None,
            "typing and focus elsewhere say nothing: focused {focused}, typed {typed:?}"
        );
    }

    assert!(app.open_system_state_tab(), "System State in front");
    let wid = app.frontmost_window.expect("a front window");
    assert!(app.commit_glassless_native_frame(wid));
    let line = app
        .note_local_network_hold(now, Some(5))
        .expect("the hold changed");
    assert_eq!(
        line,
        "system state: the Local Network step waits (revision 5): no aterm window has keyboard \
         focus",
        "System State in view: the front's own part, and no quiet part"
    );
    focus(&mut app, true);
    assert!(app.system_state_frontmost(), "precondition: in front");
    let line = app
        .note_local_network_hold(now, Some(5))
        .expect("the hold changed");
    assert_eq!(
        line,
        "system state: the Local Network step waits (revision 5): a key was typed into aterm in \
         the last 10 s"
    );
    assert_eq!(
        app.note_local_network_hold(now, Some(5)),
        None,
        "a look that finds the same says nothing"
    );

    app.last_keystroke_at = None;
    app.tick_local_network_step(now);
    assert_eq!(triggers(), fired + 1, "its moment came");
    assert_eq!(
        app.access_steps.local_network_held, None,
        "the moment clears the hold"
    );
}

/// System State out of view in the aterm window last focused holds the Local Network step with
/// those words alone: behind another tab, a macOS dialog of this process counting up adds
/// nothing, and laid out beside the active view of a split, the person switching apps adds no
/// line, focused or not. NEGATIVE CONTROL: with System State that window's active view, the
/// focus it lacks is named.
#[test]
fn system_state_out_of_view_is_the_local_network_steps_one_hold() {
    use crate::native_settings::{SettingsRoute, SettingsViewState};
    let reach = StepsReach {
        frontmost: App::system_state_frontmost,
        quiet: App::step_moment_is_quiet,
        names_holds: true,
        dialog_up: || true,
        ..stepping()
    };
    let fresh = || {
        let mut app = App::headless_for_test();
        app.devtools_helper.note_cold_start();
        app.access_steps.set_reach(reach);
        app.user_input_recent = |_| true;
        approve(&mut app, 5);
        app
    };
    let now = Instant::now() + crate::devtools_helper::COLD_START_SETTLE;
    let said = |app: &App| app.local_network_holds(now);

    let mut behind = fresh();
    assert!(
        behind.open_system_state_tab_quietly(),
        "System State behind"
    );
    for focused in [false, true] {
        focus(&mut behind, focused);
        assert_eq!(
            said(&behind),
            ["System State is not in view in the aterm window last focused"],
            "behind, a dialog counting up, focused {focused}"
        );
    }

    let mut split = fresh();
    let wid = split.frontmost_window.expect("a window");
    assert!(split.open_settings_tab(SettingsRoute::Home));
    let (settings, _) = split.active_native_view(wid).expect("Settings' view");
    assert!(split.open_system_state_tab(), "System State in front");
    let state = SettingsViewState::new(&split.config);
    split
        .split_active_with_native(
            wid,
            crate::tab_model::SplitAxis::Horizontal,
            settings,
            crate::native_app::AppViewState::Settings(Box::new(state)),
        )
        .expect("split");
    for focused in [false, true, false] {
        focus(&mut split, focused);
        assert_eq!(
            said(&split),
            ["System State is not the active view in the aterm window last focused"],
            "laid out beside the active pane, focused {focused}"
        );
    }

    let mut active = fresh();
    assert!(active.open_system_state_tab(), "System State in front");
    focus(&mut active, false);
    assert_eq!(said(&active), ["no aterm window has keyboard focus"]);
}

/// While System State's Local Network step is still to run and nothing but a lasting reason
/// holds it, a `[privacy]` switch off, aterm.log says that reason alone, once, however the person
/// types or moves between windows meanwhile.
#[test]
fn a_privacy_switch_off_is_the_local_network_steps_one_hold() {
    let mut app = App::headless_for_test();
    app.devtools_helper.note_cold_start();
    let now = Instant::now() + crate::devtools_helper::COLD_START_SETTLE;
    app.access_steps.set_reach(StepsReach {
        quiet: App::step_moment_is_quiet,
        names_holds: true,
        ..stepping()
    });
    app.user_input_recent = |_| true;
    app.set_privacy_for_test(switches(Some("notice")));
    approve(&mut app, 5);
    focus(&mut app, false);
    app.tick_local_network_step(now);
    assert_eq!(
        app.access_steps.local_network_held.as_deref(),
        Some(&["a [privacy] switch (enabled, check or notice) is off".to_owned()][..])
    );
    for (focused, typed) in [(true, Some(now)), (true, None), (false, None)] {
        focus(&mut app, focused);
        app.last_keystroke_at = typed;
        assert_eq!(
            app.note_local_network_hold(now, Some(5)),
            None,
            "focused {focused}, typed {typed:?}"
        );
    }
}

/// System State's steps' quiet moment reads aterm's own keys at rest, not the pointer: with the
/// person at the Mac moving the pointer every few seconds (macOS's idle time never past
/// `QUIET_FOR`, as the lab's person nudges it every 5 s), the Local Network step asks once no key
/// was typed into aterm for `QUIET_FOR`, and a key typed within it holds the step and is named.
/// NEGATIVE CONTROL: the control verb's moment, keyboard and pointer both, reads that person as
/// busy, as the steps' moment did.
#[test]
fn the_local_network_step_asks_while_the_person_moves_the_pointer() {
    use crate::devtools_helper::{COLD_START_SETTLE, QUIET_FOR};
    let fired = triggers();
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        quiet: StepsReach::live().quiet,
        names_holds: true,
        ..stepping()
    });
    app.devtools_helper.note_cold_start();
    focus(&mut app, true);
    app.user_input_recent = |_| true;
    approve(&mut app, 5);
    let now = Instant::now() + COLD_START_SETTLE + Duration::from_secs(1);
    app.last_keystroke_at = Some(now - Duration::from_secs(3));
    assert!(
        !app.devtools_moment_is_quiet(now),
        "control: the pointer moves, so the keyboard-and-pointer moment never comes"
    );
    app.tick_local_network_step(now);
    assert_eq!(triggers(), fired, "a key 3 s ago holds the step");
    assert_eq!(
        app.access_steps.local_network_held.as_deref(),
        Some(&["a key was typed into aterm in the last 10 s".to_owned()][..])
    );
    let later = now + QUIET_FOR;
    assert!(!app.devtools_moment_is_quiet(later));
    app.tick_local_network_step(later);
    assert_eq!(
        triggers(),
        fired + 1,
        "keys at rest, the pointer still moving: the step asks"
    );
}

/// The quiet moment's parts in aterm.log's words, the control verb's and the steps', are empty
/// exactly when that moment is quiet, over focus, input, keys, a focus taken back, and macOS's
/// idle time read or not.
#[test]
fn the_quiet_moments_gaps_are_empty_only_when_it_is_quiet() {
    let inputs: [fn(Duration) -> bool; 3] = [
        |_| true,
        |_| false,
        |within| within > crate::devtools_helper::QUIET_FOR,
    ];
    let readings: [fn() -> bool; 2] = [|| true, || false];
    let mut quiet_seen = false;
    for focused in [false, true] {
        for input in inputs {
            for readable in readings {
                let mut app = App::headless_for_test();
                // A second past each app's own settling, however long the apps before it took
                // to make: a clock read once, before them all, left a slow host's later apps
                // unsettled, and then no case was quiet.
                let now = Instant::now()
                    + crate::devtools_helper::COLD_START_SETTLE
                    + Duration::from_secs(1);
                app.devtools_helper.note_cold_start();
                focus(&mut app, focused);
                app.user_input_recent = input;
                app.user_input_readable = readable;
                let quiet = app.devtools_moment_is_quiet(now);
                quiet_seen |= quiet;
                assert_eq!(
                    app.quiet_gaps(now, false).is_empty(),
                    quiet,
                    "focused {focused}: {:?}",
                    app.quiet_gaps(now, false)
                );
                for typed in [None, Some(now), Some(now - Duration::from_secs(11))] {
                    for back in [None, Some(now - Duration::from_secs(1))] {
                        app.last_keystroke_at = typed;
                        app.keyboard_focus_back_at = back;
                        let quiet = app.step_moment_is_quiet(now);
                        quiet_seen |= quiet;
                        assert_eq!(
                            app.step_quiet_gaps(now).is_empty(),
                            quiet,
                            "steps, focused {focused}, typed {typed:?}, back {back:?}: {:?}",
                            app.step_quiet_gaps(now)
                        );
                    }
                }
            }
        }
    }
    assert!(quiet_seen, "one case is quiet");
}

/// System State's steps never take macOS's unreadable idle time for a person at the Mac: the live
/// input reading answers "input just now" when `HIDIdleTime` cannot be read, which holds no step
/// that reads only aterm's keys, so the step holds and names it until the person is seen in aterm
/// (a click in an aterm window within `PRESENT_WITHIN`, which aterm sees itself), never for good.
/// NEGATIVE CONTROL: the idle time read again lets the step ask with no click.
#[test]
fn the_steps_wait_while_macos_idle_time_cannot_be_read() {
    let fired = triggers();
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        quiet: StepsReach::live().quiet,
        names_holds: true,
        ..stepping()
    });
    app.devtools_helper.note_cold_start();
    focus(&mut app, true);
    app.user_input_recent = |_| true;
    app.user_input_readable = || false;
    approve(&mut app, 5);
    let now = Instant::now() + crate::devtools_helper::COLD_START_SETTLE + Duration::from_secs(1);
    app.last_person_input_at = now.checked_sub(crate::devtools_helper::PRESENT_WITHIN);
    app.tick_local_network_step(now);
    assert_eq!(triggers(), fired, "nobody known to be at the Mac: no alert");
    assert_eq!(
        app.access_steps.local_network_held.as_deref(),
        Some(
            &[
                "macOS's idle time could not be read, and nobody typed, clicked or scrolled in \
               aterm in the last 5 min"
                    .to_owned()
            ][..]
        )
    );
    let clicked = now + Duration::from_secs(1);
    app.last_person_input_at = Some(clicked);
    app.tick_local_network_step(clicked);
    assert_eq!(triggers(), fired + 1, "a click in aterm: the step asks");

    let mut read = App::headless_for_test();
    read.access_steps.set_reach(app.access_steps.reach);
    read.devtools_helper.note_cold_start();
    focus(&mut read, true);
    read.user_input_recent = |_| true;
    approve(&mut read, 5);
    read.tick_local_network_step(now);
    assert_eq!(
        triggers(),
        fired + 2,
        "control: the idle time read, no click"
    );
}

/// Keys typed while no aterm window had keyboard focus (into another app before a click brought
/// aterm's System State to the front) are not seen as aterm's keys, so the steps' key rest counts
/// from the window taking focus back: the Local Network step waits `QUIET_FOR` after that return
/// and names it. NEGATIVE CONTROL: focus passing for a moment (shorter than `FOCUS_AWAY_FOR`)
/// holds nothing.
#[test]
fn the_steps_key_rest_counts_from_aterm_taking_focus_back() {
    use crate::devtools_helper::{COLD_START_SETTLE, QUIET_FOR};
    let fired = triggers();
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        quiet: StepsReach::live().quiet,
        names_holds: true,
        ..stepping()
    });
    app.devtools_helper.note_cold_start();
    focus(&mut app, true);
    app.user_input_recent = |_| true;
    approve(&mut app, 5);
    let wid = crate::WindowId(0);
    let settled = Instant::now() + COLD_START_SETTLE + Duration::from_secs(1);

    // A flick on the test's own clock: focus gone 5 ms, then back.
    let flick = settled - Duration::from_secs(2);
    focus(&mut app, false);
    app.note_keyboard_focus(true, flick);
    focus(&mut app, true);
    app.note_keyboard_focus(false, flick + Duration::from_millis(5));
    assert_eq!(app.keyboard_focus_back_at, None, "a flick is no return");
    assert!(
        app.step_moment_is_quiet(settled),
        "control: a flick of focus holds no typing"
    );

    // The window's own Focused events note the edges: the time each read is the edge's own.
    app.window_focus_event(wid, false);
    assert!(!app.any_os_window_focused(), "precondition: away");
    let lost = app.keyboard_focus_lost_at.expect("the loss is noted");
    // The person was in another app, typing, for 20 s.
    app.keyboard_focus_lost_at = lost.checked_sub(Duration::from_secs(20));
    app.window_focus_event(wid, true);
    let back = app
        .keyboard_focus_back_at
        .expect("back after 20 s away: the return is noted");
    let now = back.max(settled);
    app.tick_local_network_step(now);
    assert_eq!(
        triggers(),
        fired,
        "keys typed elsewhere may still be coming"
    );
    assert_eq!(
        app.access_steps.local_network_held.as_deref(),
        Some(
            &[
                "an aterm window took keyboard focus back in the last 10 s (keys typed while none \
               had it were not seen)"
                    .to_owned()
            ][..]
        )
    );
    app.tick_local_network_step(back + QUIET_FOR);
    assert_eq!(
        triggers(),
        fired + 1,
        "at rest since the return: the step asks"
    );
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

/// A grant an update left off says so under the menu line while it reads off, and the note ends
/// with the regression. Negative control: the same regression with no update says nothing more.
#[test]
fn a_grant_an_update_left_off_says_the_update_needs_it_re_allowed() {
    use super::FDA_AFTER_UPDATE;
    let now = Instant::now();
    let mut app = App::headless_for_test();
    app.note_fda_regression(true);
    app.settle_access_steps(now);
    assert_eq!(
        app.system_state_menu_line(),
        line("System State: Full Disk Access needs you", None),
        "the control: no update, no note"
    );

    let mut app = App::headless_for_test();
    app.note_fda_off_after_update();
    app.settle_access_steps(now);
    assert_eq!(app.system_state_menu_line(), None, "not regressed: no line");
    app.note_fda_regression(true);
    app.settle_access_steps(now);
    assert_eq!(
        app.system_state_menu_line(),
        line(
            "System State: Full Disk Access needs you",
            Some(FDA_AFTER_UPDATE)
        )
    );
    app.note_fda_regression(false);
    app.note_fda_regression(true);
    app.settle_access_steps(now);
    assert_eq!(
        app.system_state_menu_line(),
        line("System State: Full Disk Access needs you", None),
        "the note ended with the regression it was said of"
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

/// A Local Network trigger that attempted nothing (here a LAN named but no socket; no LAN to name
/// is `a_trigger_that_found_no_lan_looks_again_once_the_network_moves_at_its_moment`) records
/// nothing and takes no hold: the steps after it go on.
#[test]
fn a_trigger_that_asked_nothing_records_nothing_and_looks_again() {
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        trigger_local_network: || false,
        lan_here: || super::LanHere::Lan,
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
    // The socket came up: the retry asks, and records it.
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
    let code = "On an admin Mac: aterm secrets approve ABCD-EFGH";
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
            Some("On an admin Mac: aterm secrets approve ABCD-EFGH"),
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
        assert!(
            !app.person_permission_admission_stands(),
            "{name}: no admission"
        );
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

thread_local! {
    /// The wakes the Local Network step's network watch was started with on this thread
    /// (`StepsReach::watch_network`).
    static NETWORK_WATCHES: RefCell<Vec<crate::devtools_helper::Wake>> =
        const { RefCell::new(Vec::new()) };
}

/// A network watch that keeps its wake for [`network_moves`].
fn watching(wake: crate::devtools_helper::Wake) {
    NETWORK_WATCHES.with(|watches| watches.borrow_mut().push(wake));
}

/// How many network watches were started on this thread.
fn watches() -> usize {
    NETWORK_WATCHES.with(|watches| watches.borrow().len())
}

/// This Mac's network moves: every watch started on this thread hears it.
fn network_moves() {
    for wake in NETWORK_WATCHES.with(|watches| watches.borrow().clone()) {
        wake();
    }
}

/// The initial subscription must reconcile current local state: a Route already included in
/// the watch's starting generation cannot require a second event to release the no-LAN retry.
#[test]
fn a_network_retry_reconciles_a_lan_returned_before_subscription() {
    use crate::network_moved::{Moved, Woke};
    use std::cell::Cell;
    let signal = Arc::new(Moved::new());
    let mut watch = signal.watch();
    let calls = Cell::new(0);
    let baseline = Cell::new(0);
    let moves = std::iter::once_with(|| {
        baseline.set(calls.get());
        signal.tell(Woke::Route);
        watch.wait(Duration::ZERO)
    })
    .chain(std::iter::once(Woke::Timer));
    watch_network_moves(moves, || calls.set(calls.get() + 1));
    assert_eq!(
        calls.get() - baseline.get(),
        1,
        "an ordinary route tells once after any startup reconciliation; Timer tells nothing"
    );

    let signal = Arc::new(Moved::new());
    signal.tell(Woke::Route);
    let mut watch = signal.watch();
    let wake = watch.wait(Duration::ZERO);
    assert_eq!(
        wake,
        Woke::Timer,
        "the starting generation includes that route"
    );
    let calls = Cell::new(0);
    watch_network_moves([wake], || calls.set(calls.get() + 1));
    assert_eq!(
        calls.get(),
        1,
        "a LAN that returned before subscription needs no second route"
    );
}

/// The real signal coalesces by generation, and a route move told before a later source still
/// wakes the watcher as a Route (`network_moved::Moved` keeps a route-moved bit), so the retry
/// requests one fresh reading of the current local state.
#[test]
fn a_network_retry_keeps_a_route_coalesced_under_another_source() {
    use crate::network_moved::{Moved, Woke};
    use std::cell::Cell;
    let mut missed = Vec::new();
    for last in [Woke::Beacon, Woke::Browse, Woke::LocalNetwork] {
        let signal = Arc::new(Moved::new());
        let mut watch = signal.watch();
        let calls = Cell::new(0);
        let baseline = Cell::new(0);
        let moves = std::iter::once_with(|| {
            baseline.set(calls.get());
            signal.tell(Woke::Route);
            signal.tell(last);
            let wake = watch.wait(Duration::ZERO);
            assert_eq!(
                wake,
                Woke::Route,
                "the route move outlives the later source"
            );
            wake
        });
        watch_network_moves(moves, || calls.set(calls.get() + 1));
        if calls.get() - baseline.get() != 1 {
            missed.push(last);
        }
    }
    assert!(
        missed.is_empty(),
        "coalesced recoveries were lost: {missed:?}"
    );
}

/// Every move requests a fresh App reading, including an Offline status change. Timer
/// does not. The same observer stays usable through later flaps; startup is one level wake.
#[test]
fn the_network_retry_observer_ignores_timeouts_and_survives_a_lan_flap() {
    use crate::network_moved::Woke;
    use std::cell::Cell;
    let here = Cell::new(LanHere::Offline);
    let observed = RefCell::new(Vec::new());
    let stages = [
        (LanHere::Offline, Woke::Timer),
        (LanHere::Offline, Woke::Beacon),
        (LanHere::Lan, Woke::Route),
        (LanHere::Offline, Woke::Beacon),
        (LanHere::Offline, Woke::Timer),
        (LanHere::Lan, Woke::Route),
    ];
    let moves = stages.into_iter().map(|(next, wake)| {
        here.set(next);
        wake
    });
    watch_network_moves(moves, || observed.borrow_mut().push(here.get()));
    assert_eq!(
        observed.into_inner(),
        [
            LanHere::Offline,
            LanHere::Offline,
            LanHere::Lan,
            LanHere::Offline,
            LanHere::Lan
        ],
        "startup and state-only changes reach the App; timeouts do not"
    );
}

/// A Local Network trigger on no network at all never spins the loop and never looks on a
/// timer: it starts one network watch and its row reads calm until this Mac's network moves;
/// then the move's fresh look reads the network it joined, the row is calm no more, and a
/// network it could not ask on (no socket) looks again a retry later.
#[test]
fn network_reconciliation_refreshes_the_offline_row() {
    use super::{LOCAL_NETWORK_NOTHING_TO_ASK, LOCAL_NETWORK_RETRY, OPT_OUT_ROWS};
    use crate::network_moved::{Moved, Woke};
    use aterm_types::macos_setup::opt_out::OptOut;
    use std::cell::Cell;

    let row = OPT_OUT_ROWS
        .iter()
        .position(|access| *access == Access::LocalNetwork)
        .expect("Local Network row");
    let calm = Some(OptOut::Always(LOCAL_NETWORK_NOTHING_TO_ASK));
    NETWORK_WATCHES.with(|watches| watches.borrow_mut().clear());
    RECORDS.with(|records| records.borrow_mut().clear());
    let reach = StepsReach {
        trigger_local_network: no_lan,
        lan_here: || LanHere::Offline,
        watch_network: watching,
        ..stepping()
    };
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(reach);
    focus(&mut app, true);
    approve(&mut app, 9);
    let t0 = Instant::now();
    let asked = triggers();
    app.tick_local_network_step(t0);
    assert_eq!(app.access_opt_outs()[row], calm);
    assert_eq!(watches(), 1);

    let wake = NETWORK_WATCHES.with(|watches| watches.borrow()[0].clone());
    let signal = Arc::new(Moved::new());
    let mut watch = signal.watch();
    let baseline = Cell::new(0);
    let moves = std::iter::once_with(|| {
        // Consume startup reconciliation before changing the state, so only the
        // following actual Route can make the final status assertion pass.
        app.tick_local_network_step(t0 + Duration::from_secs(1));
        assert_eq!(app.access_opt_outs()[row], calm);
        baseline.set(failed_triggers());
        app.access_steps.set_reach(StepsReach {
            lan_here: || LanHere::Lan,
            ..reach
        });
        signal.tell(Woke::Route);
        let woke = watch.wait(Duration::ZERO);
        assert_eq!(woke, Woke::Route, "actual private signal");
        woke
    });
    watch_network_moves(moves, || wake());

    let now = t0 + Duration::from_secs(2);
    app.tick_local_network_step(now);
    assert_eq!(
        failed_triggers(),
        baseline.get() + 1,
        "the move reached the App"
    );
    assert_eq!(app.access_opt_outs()[row], None, "calm no more");
    assert_eq!(
        app.next_access_step_deadline(now),
        Some(now + LOCAL_NETWORK_RETRY),
        "a network with no socket looks again a retry later"
    );
    assert_eq!(watches(), 1, "no watcher restart");
    assert_eq!(triggers(), asked, "no prompt-capable trigger");
    assert!(
        records()
            .iter()
            .all(|(_, access, _)| *access != Access::LocalNetwork)
    );
}

/// A Local Network trigger that found no LAN to name never spins the loop and never looks on a
/// timer: it starts one network watch and arms nothing, hours on, until this Mac's network moves;
/// then it looks again at the step's moment only. A move heard while System State is away waits
/// for it: an unfocused window arms nothing, a focused one the next look for the moment, never a
/// deadline due now. A new approval drops an earlier revision's retry, and with none approved it
/// is kept but arms nothing, a move or not.
#[test]
fn a_trigger_that_found_no_lan_looks_again_once_the_network_moves_at_its_moment() {
    use crate::devtools_helper::RECHECK_EVERY;
    NETWORK_WATCHES.with(|watches| watches.borrow_mut().clear());
    let reach = StepsReach {
        trigger_local_network: no_lan,
        watch_network: watching,
        ..stepping()
    };
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(reach);
    focus(&mut app, true);
    approve(&mut app, 7);
    let t0 = Instant::now();
    let tried = failed_triggers();
    app.tick_local_network_step(t0);
    assert_eq!(failed_triggers(), tried + 1, "System State in front: tried");
    assert_eq!(watches(), 1, "the network watch starts");
    for hour in 1..=3 {
        let now = t0 + Duration::from_secs(3600 * hour);
        app.tick_local_network_step(now);
        assert_eq!(
            app.next_access_step_deadline(now),
            None,
            "hour {hour}: no timer"
        );
    }
    assert_eq!(failed_triggers(), tried + 1, "no look without a move");

    // The network moves while the person is in a terminal tab, then in another app.
    network_moves();
    app.access_steps.set_reach(StepsReach {
        frontmost: |_| false,
        ..reach
    });
    let moved = t0 + Duration::from_secs(4 * 3600);
    focus(&mut app, false);
    for park in 0..3 {
        let now = moved + Duration::from_millis(park);
        app.tick_local_network_step(now);
        assert_eq!(
            app.next_access_step_deadline(now),
            None,
            "park {park}: an unfocused window arms nothing"
        );
    }
    focus(&mut app, true);
    app.tick_local_network_step(moved);
    assert_eq!(
        app.next_access_step_deadline(moved),
        Some(moved + RECHECK_EVERY),
        "focused on a terminal tab: the next look for its moment"
    );
    assert_eq!(failed_triggers(), tried + 1, "never away from its moment");

    // Back at System State: it looks, finds no LAN again, and waits for the next move.
    app.access_steps.set_reach(reach);
    let back = moved + RECHECK_EVERY;
    app.tick_local_network_step(back);
    assert_eq!(failed_triggers(), tried + 2, "the move's look");
    assert_eq!(
        app.next_access_step_deadline(back),
        None,
        "and no timer after it"
    );
    app.tick_local_network_step(back + RECHECK_EVERY);
    assert_eq!(failed_triggers(), tried + 2, "one look per move");
    assert_eq!(watches(), 1, "one watch per process");

    // A new approval drops the old revision's retry.
    app.access_steps.set_reach(StepsReach {
        frontmost: |_| false,
        ..reach
    });
    approve(&mut app, 8);
    app.tick_local_network_step(back);
    assert_eq!(app.access_steps.local_network_retry, None);
    assert!(
        app.next_access_step_deadline(back)
            .is_none_or(|at| at > back),
        "nothing due at once for a revision no longer approved"
    );
    // The approval going away (an Apply holds the gate) keeps revision 8's retry, which arms
    // nothing meanwhile, whatever moves.
    app.access_steps.set_reach(reach);
    app.tick_local_network_step(back);
    assert_eq!(failed_triggers(), tried + 3, "revision 8 tried");
    app.hold_initial_setup_permissions();
    network_moves();
    let later = back + Duration::from_secs(3600);
    app.tick_local_network_step(later);
    assert_eq!(
        app.access_steps
            .local_network_retry
            .map(|(failed, _)| failed),
        Some(8),
        "kept while no revision is approved"
    );
    assert_eq!(
        app.next_access_step_deadline(later),
        None,
        "nothing armed while no revision is approved"
    );
    assert_eq!(failed_triggers(), tried + 3);
}

/// A trigger that named a LAN but could not make its socket looks again a minute later, at the
/// step's moment only: a retry due while System State is away is moved on a minute (left due, it
/// would wake the loop on every park).
#[test]
fn a_trigger_with_no_socket_looks_again_a_minute_later_at_its_moment() {
    let retry = super::LOCAL_NETWORK_RETRY;
    NETWORK_WATCHES.with(|watches| watches.borrow_mut().clear());
    let reach = StepsReach {
        trigger_local_network: no_lan,
        lan_here: || super::LanHere::Lan,
        watch_network: watching,
        ..stepping()
    };
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(reach);
    focus(&mut app, true);
    approve(&mut app, 7);
    let t0 = Instant::now();
    let tried = failed_triggers();
    app.tick_local_network_step(t0);
    assert_eq!(failed_triggers(), tried + 1);
    assert_eq!(watches(), 0, "a LAN was named: no watch");
    assert_eq!(app.next_access_step_deadline(t0), Some(t0 + retry));

    app.access_steps.set_reach(StepsReach {
        frontmost: |_| false,
        ..reach
    });
    let due = t0 + retry;
    app.tick_local_network_step(due);
    assert_eq!(
        app.next_access_step_deadline(due),
        Some(due + retry),
        "focused on a terminal tab: the retry was moved on a minute"
    );
    assert_eq!(failed_triggers(), tried + 1, "never away from its moment");
    app.access_steps.set_reach(reach);
    app.tick_local_network_step(due + retry);
    assert_eq!(
        failed_triggers(),
        tried + 2,
        "back at its moment: it looks again"
    );
}

/// A Local Network retry outlives an Apply to another group, which holds the Permissions gate
/// until its fresh status names the same access revision: kept and arming nothing through the
/// gap, a network move told in it included, then looking at once once the revision stands again,
/// so the Permissions stage does not wait on Local Network until a relaunch.
#[test]
fn a_retry_outlives_an_apply_that_keeps_the_access_revision() {
    use crate::devtools_helper::RECHECK_EVERY;
    NETWORK_WATCHES.with(|watches| watches.borrow_mut().clear());
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        trigger_local_network: no_lan,
        watch_network: watching,
        ..stepping()
    });
    focus(&mut app, true);
    approve(&mut app, 7);
    let t0 = Instant::now();
    let tried = failed_triggers();
    app.tick_local_network_step(t0);
    assert_eq!(failed_triggers(), tried + 1, "no LAN yet");

    // Keyboard & Trackpad → Set up: the Apply holds the gate, and the Wi-Fi joins meanwhile.
    app.hold_initial_setup_permissions();
    assert_eq!(app.access_revision(), None);
    let gap = t0 + Duration::from_secs(1);
    network_moves();
    app.tick_local_network_step(gap);
    assert_eq!(
        app.next_access_step_deadline(gap),
        None,
        "nothing armed while no revision is approved"
    );
    assert_eq!(failed_triggers(), tried + 1);

    // The fresh status names the same access revision.
    app.accepted_initial_setup_apply();
    approve(&mut app, 7);
    assert_eq!(app.access_revision(), Some(7));
    assert_eq!(
        app.next_access_step_deadline(gap),
        Some(gap + RECHECK_EVERY),
        "the retry is armed again for revision 7"
    );
    app.tick_local_network_step(gap);
    assert_eq!(failed_triggers(), tried + 2, "it looks for a LAN again");
    assert_eq!(app.next_access_step_deadline(gap), None);
}

/// On no network at all its row reads calm for the Permissions stage, an opt-out that says
/// there is nothing to ask yet ([`super::LOCAL_NETWORK_NOTHING_TO_ASK`]), and nothing is
/// recorded; once this Mac joins a network, the step asks, records it, and the row is no longer
/// calm. NEGATIVE CONTROL: a network the step could not ask on (no socket) reads no such row:
/// it looks again a retry later.
#[test]
fn no_network_settles_the_local_network_row_calm_until_this_mac_joins_one() {
    use super::{LOCAL_NETWORK_NOTHING_TO_ASK, LanHere, OPT_OUT_ROWS};
    use aterm_types::macos_setup::opt_out::OptOut;
    let lan = OPT_OUT_ROWS
        .iter()
        .position(|access| *access == Access::LocalNetwork)
        .expect("the Local Network row");
    NETWORK_WATCHES.with(|watches| watches.borrow_mut().clear());
    RECORDS.with(|records| records.borrow_mut().clear());
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        trigger_local_network: no_lan,
        lan_here: || LanHere::Offline,
        watch_network: watching,
        ..stepping()
    });
    focus(&mut app, true);
    approve(&mut app, 9);
    assert_eq!(app.access_opt_outs()[lan], None, "before the step looks");
    let t0 = Instant::now();
    app.tick_local_network_step(t0);
    assert_eq!(
        app.access_opt_outs()[lan],
        Some(OptOut::Always(LOCAL_NETWORK_NOTHING_TO_ASK)),
        "nothing to ask on no network: the row reads calm"
    );
    assert!(
        records()
            .iter()
            .all(|(_, access, _)| *access != Access::LocalNetwork),
        "nothing recorded"
    );
    let hour = t0 + Duration::from_secs(3600);
    app.tick_local_network_step(hour);
    assert_eq!(
        app.access_opt_outs()[lan],
        Some(OptOut::Always(LOCAL_NETWORK_NOTHING_TO_ASK)),
        "an hour on, still calm"
    );

    // The Mac joins a network.
    app.access_steps.set_reach(StepsReach {
        lan_here: || LanHere::Lan,
        watch_network: watching,
        ..stepping()
    });
    let asked = triggers();
    network_moves();
    app.tick_local_network_step(hour);
    assert_eq!(triggers(), asked + 1, "the move's look asks");
    assert!(
        records().contains(&(9, Access::LocalNetwork, AccessOutcome::Triggered)),
        "and records it: {:?}",
        records()
    );
    assert_eq!(
        app.access_opt_outs()[lan],
        None,
        "the row is no longer calm"
    );

    let mut no_socket = App::headless_for_test();
    no_socket.access_steps.set_reach(StepsReach {
        trigger_local_network: no_lan,
        lan_here: || LanHere::Lan,
        watch_network: watching,
        ..stepping()
    });
    focus(&mut no_socket, true);
    approve(&mut no_socket, 9);
    no_socket.tick_local_network_step(t0);
    assert_eq!(
        no_socket.access_opt_outs()[lan],
        None,
        "on a network the step could not ask on yet, the row waits for it"
    );

    // An earlier revision's calm opt-out is dropped when a later revision is already on record.
    let mut next_rev = App::headless_for_test();
    next_rev.access_steps.set_reach(StepsReach {
        trigger_local_network: no_lan,
        lan_here: || LanHere::Offline,
        watch_network: watching,
        recorded: |rev, access| {
            (rev == 10 && access == Access::LocalNetwork).then_some(AccessOutcome::Triggered)
        },
        ..stepping()
    });
    focus(&mut next_rev, true);
    approve(&mut next_rev, 9);
    next_rev.tick_local_network_step(t0);
    assert_eq!(
        next_rev.access_opt_outs()[lan],
        Some(OptOut::Always(LOCAL_NETWORK_NOTHING_TO_ASK)),
        "revision 9 reads calm on no network"
    );
    approve(&mut next_rev, 10);
    next_rev.tick_local_network_step(t0);
    assert_eq!(
        next_rev.access_opt_outs()[lan],
        None,
        "a later revision already on record clears the old revision's calm opt-out"
    );
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
    current_devtools_not_granted(&mut app, 2);
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
    app.on_devtools_probe_ready();
    park(&mut app);
    assert_eq!(test_arms::opens(), first + 2, "the new approval asks again");
}

/// The window's own Full Disk Access page comes up once per revision even after a person's press
/// while no revision stood (after Stop, during a decision hold), which used to rewrite the page's
/// marker under the running version: the press keeps the revision's step, so a later launch under
/// that revision (Set up keeps it) brings nothing, and a new revision still asks.
#[cfg(target_os = "macos")]
#[test]
fn a_press_while_no_revision_stands_keeps_the_full_disk_access_page_shown_for_its_revision() {
    use crate::consent_card::PendingDecision;
    use crate::devtools_helper::test_arms;
    thread_local! {
        static MARK: RefCell<Option<String>> = const { RefCell::new(None) };
    }
    let fda_marked = test_arms::fda_marking(
        |_| MARK.with(|mark| mark.borrow().clone().map(|word| (word, 1))),
        |word| MARK.with(|mark| *mark.borrow_mut() = Some(word.to_owned())),
    );
    let on_record = StepsReach {
        recorded: |_, access| (access == Access::LocalNetwork).then_some(AccessOutcome::Triggered),
        frontmost: front,
        ..stepping()
    };
    let launch = |revision: u64| {
        let mut app = new_mac(test_arms::opening(), fda_marked);
        app.access_steps.set_reach(on_record);
        approve(&mut app, revision);
        assert!(app.consent_card.begin_deciding(Instant::now()));
        app.decide_macos_access_card(PendingDecision::launch(None));
        app
    };
    let mark = || MARK.with(|mark| mark.borrow().clone());
    set_front(true);
    let opens = || test_arms::fda_counts().0;
    let first = opens();
    let mut t = Instant::now();

    let mut app = launch(1);
    parks(&mut app, &mut t, 2);
    assert_eq!(opens(), first + 1, "revision 1's page");
    let shown = mark().expect("its marker");
    // Stop: no revision stands, and the person opens the page from its row.
    app.observe_initial_setup_permissions(None);
    assert!(app.open_access_from_system_state(Access::FullDiskAccess, true));
    assert_eq!(opens(), first + 2, "the press opens the page");
    assert_eq!(mark(), Some(shown), "the press keeps revision 1's step");
    drop(app);

    let mut app = launch(1);
    parks(&mut app, &mut t, 4);
    assert_eq!(opens(), first + 2, "revision 1 brought its page up already");
    drop(app);

    let mut app = launch(2);
    parks(&mut app, &mut t, 4);
    assert_eq!(opens(), first + 3, "the new approval asks again");
}

/// A Developer Tools reading that finds aterm missing asks for the window's own page only while
/// the record holds no `handed` for the revision: `aterm doctor` writes the one-line
/// marker under the running version, so the marker alone would ask again.
#[cfg(target_os = "macos")]
#[test]
fn a_developer_tools_reading_asks_for_the_page_only_while_the_record_holds_no_handed() {
    use crate::consent_state::SlotKind;
    use crate::devtools_helper::{Origin, test_arms};
    use aterm_containment::consent::FdaState;
    use aterm_containment::{DevToolProbe, DevToolProbeLabel, DevToolState, ProbeGate};
    fn not_on(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
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
    let doctors = test_arms::marked(|| Some((aterm_types::version::app_version().to_owned(), 1)));
    for (handed, asks) in [(false, true), (true, false)] {
        set_front(true);
        let mut app = new_mac(doctors, test_arms::fda_opening());
        let handed_arm: fn(u64, Access) -> bool = if handed {
            |revision, access| (revision, access) == (1, Access::DeveloperTools)
        } else {
            |_, _| false
        };
        app.access_steps.set_reach(StepsReach {
            handed: handed_arm,
            ..app.access_steps.reach
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let mut consent = reread(FdaState::Granted);
        consent.set_devtools_arms_for_test(not_on, subject);
        consent.set_slot_wake(std::sync::Arc::new(move |kind: SlotKind| {
            if kind == SlotKind::Devtools {
                let _ = tx.send(());
            }
        }));
        app.consent = consent;
        focus(&mut app, true);
        app.set_privacy_for_test(switches(None));
        app.read_devtools_list(Instant::now());
        rx.recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("handed {handed}: the launch read runs"));
        app.on_devtools_probe_ready();
        assert_eq!(
            app.devtools_helper.queued(),
            asks.then_some(Origin::Automatic),
            "handed {handed}"
        );
    }
}

/// A revision approved in this process brings the Developer Tools page back only while the
/// record holds no `handed` for it: one whose page came up already stays down whatever the
/// marker says.
#[cfg(target_os = "macos")]
#[test]
fn a_revision_whose_developer_tools_page_came_up_is_not_asked_again_in_this_process() {
    use crate::devtools_helper::{Origin, test_arms};
    use aterm_containment::DevToolGrant;
    set_front(true);
    let doctors = test_arms::marked(|| Some((aterm_types::version::app_version().to_owned(), 1)));
    let mut app = new_mac(doctors, test_arms::fda_opening());
    app.access_steps.set_reach(StepsReach {
        handed: |revision, access| (revision, access) == (2, Access::DeveloperTools),
        ..app.access_steps.reach
    });
    app.consent = reread(aterm_containment::consent::FdaState::Granted);
    current_devtools_not_granted(&mut app, 2);
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
        Instant::now(),
    );
    app.settle_access_steps(Instant::now());
    approve(&mut app, 2);
    app.settle_access_steps(Instant::now());
    app.on_devtools_probe_ready();
    assert_eq!(
        app.devtools_helper.queued(),
        None,
        "revision 2's page came up already"
    );
    approve(&mut app, 3);
    app.settle_access_steps(Instant::now());
    app.on_devtools_probe_ready();
    assert_eq!(
        app.devtools_helper.queued(),
        Some(Origin::Automatic),
        "revision 3 asks"
    );
}

/// A Developer Tools hand-over asked while no revision stands (a reading after Stop) waits for
/// one, and when the same revision is back it is judged again as the page would open: a page
/// that revision already brought up stays down, and one it never brought up comes up.
#[cfg(target_os = "macos")]
#[test]
fn a_developer_tools_page_asked_with_no_revision_is_judged_again_when_the_revision_is_back() {
    use crate::devtools_helper::{HandOver, Origin, test_arms};
    use aterm_containment::DevToolGrant;
    for (handed, more) in [(false, 1), (true, 0)] {
        set_front(true);
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        let handed_arm: fn(u64, Access) -> bool = if handed {
            |revision, access| (revision, access) == (1, Access::DeveloperTools)
        } else {
            |_, _| false
        };
        app.access_steps.set_reach(StepsReach {
            handed: handed_arm,
            ..app.access_steps.reach
        });
        app.consent = reread(aterm_containment::consent::FdaState::Granted);
        app.devtools_helper.note_reading(
            test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
            Instant::now(),
        );
        let mut t = Instant::now();
        app.settle_access_steps(t);
        app.observe_initial_setup_permissions(None);
        assert_eq!(
            app.hand_devtools_to_owner(Origin::Automatic),
            HandOver::Queued,
            "handed {handed}: no revision, so it waits"
        );
        approve(&mut app, 1);
        let opens = test_arms::opens();
        parks(&mut app, &mut t, 3);
        assert_eq!(test_arms::opens(), opens + more, "handed {handed}");
    }
}

/// The window's own Developer Tools step, asked while no revision stood (before the first, or
/// between a revision's Stop and its return) or brought back by a revision approved in this
/// process, starts its `spctl` run as soon as that revision admits it when its list reads whole
/// and empty, so its moment needs no run of its own. Negative controls: with no revision nothing
/// runs, and a list that names Terminal needs no run.
#[cfg(target_os = "macos")]
#[test]
fn a_developer_tools_step_a_revision_admits_starts_its_spctl_run_ahead() {
    use crate::devtools_helper::{HandOver, Origin, test_arms};
    use aterm_containment::DevToolGrant;
    #[derive(Clone, Copy, Debug)]
    enum Path {
        First,
        Back,
        Rearmed,
    }
    for rows in [0, 1] {
        for path in [Path::First, Path::Back, Path::Rearmed] {
            set_front(true);
            let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
            app.access_steps.set_reach(StepsReach {
                handed: |revision, access| (revision, access) == (2, Access::DeveloperTools),
                ..app.access_steps.reach
            });
            app.consent = reread(aterm_containment::consent::FdaState::Granted);
            current_devtools_not_granted(&mut app, rows);
            app.devtools_helper.note_reading(
                test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(rows)),
                Instant::now(),
            );
            let t = Instant::now();
            let revision = match path {
                Path::First | Path::Back => {
                    if matches!(path, Path::First) {
                        app.observe_initial_setup_permissions(None);
                    }
                    app.settle_access_steps(t);
                    app.observe_initial_setup_permissions(None);
                    app.settle_access_steps(t);
                    assert_eq!(
                        app.hand_devtools_to_owner(Origin::Automatic),
                        HandOver::Queued
                    );
                    1
                }
                Path::Rearmed => {
                    approve(&mut app, 2);
                    app.settle_access_steps(t);
                    app.on_devtools_probe_ready();
                    assert_eq!(
                        app.devtools_helper.queued(),
                        None,
                        "PRECONDITION: revision 2's page came up already"
                    );
                    3
                }
            };
            assert!(
                !app.devtools_helper.listing_ahead(),
                "the control: no revision admits it yet, nothing runs ({rows}, {path:?})"
            );
            approve(&mut app, revision);
            app.settle_access_steps(t);
            if matches!(path, Path::Rearmed) {
                app.on_devtools_probe_ready();
            }
            assert_eq!(
                app.devtools_helper.queued(),
                Some(Origin::Automatic),
                "PRECONDITION: the step waits for its moment ({rows}, {path:?})"
            );
            assert_eq!(
                app.devtools_helper.listing_ahead(),
                rows == 0,
                "an empty list: `spctl` runs ahead ({rows}, {path:?})"
            );
        }
    }
}

/// An ask the window's own Developer Tools step carried while it waited for a revision (a control
/// client's `hand-over` before Set up, or its act on the row once Set up is back) is judged by its
/// own rules when the same revision is back and refuses the step because it already brought the
/// page up: the client gets the page it was answered `queued` for.
#[cfg(target_os = "macos")]
#[test]
fn an_ask_the_waiting_developer_tools_step_carried_is_judged_when_the_step_is_refused() {
    use crate::devtools_helper::{HandOver, Origin, test_arms};
    use aterm_containment::DevToolGrant;
    for ask in [None, Some(Origin::Ctl), Some(Origin::SystemStateAct)] {
        set_front(true);
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.access_steps.set_reach(StepsReach {
            handed: |revision, access| (revision, access) == (1, Access::DeveloperTools),
            ..app.access_steps.reach
        });
        app.consent = reread(aterm_containment::consent::FdaState::Granted);
        app.devtools_helper.note_reading(
            test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
            Instant::now(),
        );
        let mut t = Instant::now();
        app.settle_access_steps(t);
        app.observe_initial_setup_permissions(None);
        assert_eq!(
            app.hand_devtools_to_owner(Origin::Automatic),
            HandOver::Queued
        );
        if ask == Some(Origin::Ctl) {
            assert_eq!(
                app.hand_devtools_to_owner(Origin::Ctl),
                HandOver::Queued,
                "the client is told it waits"
            );
        }
        approve(&mut app, 1);
        if ask == Some(Origin::SystemStateAct) {
            assert!(
                !app.open_access_from_system_state(Access::DeveloperTools, false),
                "the act waits for the step moment"
            );
        }
        let opens = test_arms::opens();
        parks(&mut app, &mut t, 3);
        assert_eq!(
            test_arms::opens(),
            opens + u32::from(ask.is_some()),
            "{ask:?}"
        );
    }
}

/// [`an_ask_the_waiting_developer_tools_step_carried_is_judged_when_the_step_is_refused`] for the
/// Full Disk Access page: the window's own step waits with no revision, and a client's act on the
/// row once Set up is back rides with it; revision 1 brought the page up already, which refuses
/// the step only, so the act brings it up.
#[cfg(target_os = "macos")]
#[test]
fn an_ask_the_waiting_full_disk_access_step_carried_is_judged_when_the_step_is_refused() {
    use crate::devtools_helper::{HandOver, Moment, Origin, test_arms};
    use aterm_containment::consent::FdaState;
    let shown = test_arms::fda_marking(|_| Some((atpkg::devtools::revision_key(1), 1)), |_| {});
    let on_record = StepsReach {
        recorded: |_, access| (access == Access::LocalNetwork).then_some(AccessOutcome::Triggered),
        frontmost: front,
        ..stepping()
    };
    set_front(true);
    for act in [false, true] {
        let mut app = new_mac(test_arms::opening(), shown);
        app.access_steps.set_reach(on_record);
        let mut t = Instant::now();
        app.observe_initial_setup_permissions(None);
        assert_eq!(
            app.fda_hand_over(Origin::Automatic, Moment::Asked, FdaState::Denied),
            HandOver::Queued,
            "no revision, so it waits"
        );
        approve(&mut app, 1);
        if act {
            assert!(
                !app.open_access_from_system_state(Access::FullDiskAccess, false),
                "the act waits for the step moment"
            );
        }
        let opens = test_arms::fda_counts().0;
        parks(&mut app, &mut t, 3);
        assert_eq!(
            test_arms::fda_counts().0,
            opens + u32::from(act),
            "act {act}"
        );
    }
}

/// An ask the waiting Developer Tools step carried waits with it when the step waits again at
/// its moment (its Full Disk Access reading still out), and is judged by its own rules once the
/// step is refused: the window's own step needs Full Disk Access, a control verb's ask does not.
#[cfg(target_os = "macos")]
#[test]
fn an_ask_the_developer_tools_step_carried_waits_with_it_when_it_waits_again() {
    use crate::consent_state::ConsentState;
    use crate::devtools_helper::{HandOver, Origin, test_arms};
    use aterm_containment::DevToolGrant;
    use aterm_containment::consent::{FdaProbe, FdaState, ProbeLabel};
    for ask in [false, true] {
        set_front(true);
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.consent =
            ConsentState::with_cached_probe_for_test(FdaProbe::refused(ProbeLabel::Pending));
        app.devtools_helper.note_reading(
            test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
            Instant::now(),
        );
        let mut t = Instant::now();
        app.settle_access_steps(t);
        app.observe_initial_setup_permissions(None);
        assert_eq!(
            app.hand_devtools_to_owner(Origin::Automatic),
            HandOver::Queued
        );
        if ask {
            assert_eq!(app.hand_devtools_to_owner(Origin::Ctl), HandOver::Queued);
        }
        approve(&mut app, 1);
        let opens = test_arms::opens();
        parks(&mut app, &mut t, 2);
        assert_eq!(
            (test_arms::opens(), app.devtools_helper.queued()),
            (opens, Some(Origin::Automatic)),
            "ask {ask}: the step waits for its Full Disk Access reading"
        );
        app.consent = reread(FdaState::Denied);
        parks(&mut app, &mut t, 3);
        assert_eq!(test_arms::opens(), opens + u32::from(ask), "ask {ask}");
    }
}

/// The control verb's ask that rides the waiting Developer Tools step keeps its own quiet moment:
/// while the step waits for an approved revision (after Stop) the page comes up at the first quiet
/// moment with a terminal tab in front, as it would for the ask alone. That page is not the
/// revision's, so the step waits on: when the same revision is back it brings the page up, once
/// the verb's page is ten minutes old ([`crate::devtools_helper::Lane::page_came_up`]); a look
/// within them would find the page recent and spend the step. Where the record says that
/// revision brought the page up already (`handed`, before the Stop), the step is refused as shown
/// at once rather than holding the steps behind it for those ten minutes. No panel holds the
/// step moment here (System Settings quit, say), so only the step's own wait does.
#[cfg(target_os = "macos")]
#[test]
fn the_control_verbs_ask_that_rides_the_step_keeps_its_own_quiet_moment() {
    use crate::devtools_helper::{
        DevToolsHelperArm, HandOver, Lane, Origin, RECHECK_EVERY, test_arms,
    };
    use aterm_containment::DevToolGrant;
    use std::cell::Cell;
    thread_local! {
        static RECENT: Cell<bool> = const { Cell::new(false) };
    }
    let mut marking = test_arms::opening();
    marking.note_opened = |_, _| RECENT.with(|recent| recent.set(true));
    marking.opened_within = |_| RECENT.with(Cell::get).then_some(0);
    for (ask, handed) in [(false, false), (true, false), (true, true)] {
        set_front(false);
        RECENT.with(|recent| recent.set(false));
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        if handed {
            app.access_steps.set_reach(StepsReach {
                handed: |revision, access| (revision, access) == (1, Access::DeveloperTools),
                ..app.access_steps.reach
            });
        }
        app.consent = reread(aterm_containment::consent::FdaState::Granted);
        test_arms::stand_in(&mut app, marking);
        app.devtools_helper = Lane::windowed_for_test(marking, DevToolsHelperArm::inert());
        app.devtools_helper.note_reading(
            test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
            Instant::now(),
        );
        focus(&mut app, true);
        app.user_input_recent = |within| within == crate::devtools_helper::PRESENT_WITHIN;
        let t0 = Instant::now() + crate::consent_card::LAUNCH_SETTLE;
        assert!(
            app.devtools_moment_is_quiet(t0),
            "precondition: a control verb's quiet moment, a terminal tab in front"
        );
        app.observe_initial_setup_permissions(None);
        assert_eq!(
            app.hand_devtools_to_owner(Origin::Automatic),
            HandOver::Queued
        );
        if ask {
            assert_eq!(app.hand_devtools_to_owner(Origin::Ctl), HandOver::Queued);
        }
        let opens = test_arms::opens();
        for park in 0..3 {
            app.tick_consent_lanes(t0 + RECHECK_EVERY * park);
        }
        assert_eq!(
            (test_arms::opens(), app.devtools_helper.queued()),
            (opens + u32::from(ask), Some(Origin::Automatic)),
            "ask {ask}: the step waits on for its revision"
        );
        set_front(true);
        approve(&mut app, 1);
        let mut t = t0 + RECHECK_EVERY * 3;
        parks(&mut app, &mut t, 3);
        assert_eq!(
            (test_arms::opens(), app.devtools_helper.queued()),
            (opens + 1, (ask && !handed).then_some(Origin::Automatic)),
            "ask {ask}, handed {handed}: one page so far"
        );
        // Ten minutes on.
        RECENT.with(|recent| recent.set(false));
        let mut t = t0 + Duration::from_secs(atpkg::devtools::REOPEN_AFTER_S);
        parks(&mut app, &mut t, 3);
        assert_eq!(
            (test_arms::opens(), app.devtools_helper.queued()),
            (opens + 1 + u32::from(ask && !handed), None),
            "ask {ask}, handed {handed}: the revision's own page, unless it came up already"
        );
    }
}

/// A control verb's Developer Tools ask, queued alone or riding the window's own step, waits
/// while the Local Network alert the window raised holds the screen: its moment keeps the steps'
/// one-gesture rule (`App::control_ask_moment`), so its page does not come up during the alert's
/// hold, and comes up once the hold is over: the person at the Mac for
/// [`LOCAL_NETWORK_ALERT_HOLD`] from the first look that reads a touch since the ask.
#[cfg(target_os = "macos")]
#[test]
fn a_control_verbs_ask_waits_out_the_local_network_alert() {
    use crate::devtools_helper::{
        DevToolsHelperArm, HandOver, Lane, Origin, RECHECK_EVERY, test_arms,
    };
    use aterm_containment::DevToolGrant;
    set_front(true);
    for riding in [false, true] {
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.access_steps.set_reach(StepsReach {
            recorded: |_, _| None,
            ..app.access_steps.reach
        });
        app.consent = reread(aterm_containment::consent::FdaState::Granted);
        test_arms::stand_in(&mut app, test_arms::opening());
        app.devtools_helper =
            Lane::windowed_for_test(test_arms::opening(), DevToolsHelperArm::inert());
        app.devtools_helper.note_reading(
            test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
            Instant::now(),
        );
        focus(&mut app, true);
        // The last touch reads just over the verb's key rest old at every look: none since the
        // ask until that long after it.
        app.user_input_recent = |within| within > crate::devtools_helper::QUIET_FOR;
        let t0 = Instant::now() + crate::consent_card::LAUNCH_SETTLE;
        assert!(
            app.devtools_moment_is_quiet(t0),
            "precondition: the verb's quiet moment"
        );
        if riding {
            assert_eq!(
                app.hand_devtools_to_owner(Origin::Automatic),
                HandOver::Queued
            );
        }
        assert_eq!(app.hand_devtools_to_owner(Origin::Ctl), HandOver::Queued);
        let (opens, asked) = (test_arms::opens(), triggers());
        app.tick_consent_lanes(t0);
        let touched = t0 + crate::devtools_helper::QUIET_FOR + Duration::from_secs(1);
        let hold = LOCAL_NETWORK_ALERT_HOLD;
        let mut park = Duration::ZERO;
        while park < hold {
            app.tick_consent_lanes(touched + park);
            park += RECHECK_EVERY;
        }
        assert_eq!(
            (triggers(), test_arms::opens()),
            (asked + 1, opens),
            "riding {riding}: the alert first, and no page beside it"
        );
        app.tick_consent_lanes(touched + hold);
        assert_eq!(
            test_arms::opens(),
            opens + 1,
            "riding {riding}: the page once the alert's hold is over"
        );
    }
}

/// [`a_control_verbs_ask_waits_out_the_local_network_alert`] for an ask whose own `spctl` run
/// ends while the alert's hold is up: judged again at the run's end
/// (`App::devtools_hand_over_after_listing`), it waits, and brings the page up with that run's
/// answer once the hold is over.
#[cfg(target_os = "macos")]
#[test]
fn a_control_verbs_run_that_ends_under_the_local_network_alert_waits_it_out() {
    use crate::devtools_helper::{DevToolsHelperArm, HandOver, Lane, Moment, Origin, test_arms};
    use aterm_containment::DevToolGrant;
    use std::sync::atomic::{AtomicU32, Ordering};
    // `spctl` runs on the lane's worker thread.
    static RUNS: AtomicU32 = AtomicU32::new(0);
    let mut counted = test_arms::opening();
    counted.list_terminal = || {
        RUNS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    let run_out = |app: &mut App, at: Instant| {
        let start = Instant::now();
        while app.devtools_helper.listing().is_some() {
            assert!(start.elapsed() < Duration::from_secs(60), "hang");
            app.tick_devtools_hand_over(at);
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    set_front(true);
    let mut app = new_mac(counted, test_arms::fda_opening());
    app.access_steps.set_reach(StepsReach {
        recorded: |_, _| None,
        ..app.access_steps.reach
    });
    app.consent = reread(aterm_containment::consent::FdaState::Granted);
    test_arms::stand_in(&mut app, counted);
    app.devtools_helper = Lane::windowed_for_test(counted, DevToolsHelperArm::inert());
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(0)),
        Instant::now(),
    );
    focus(&mut app, true);
    // The last touch reads just over the verb's key rest old at every look.
    app.user_input_recent = |within| within > crate::devtools_helper::QUIET_FOR;
    let t0 = Instant::now() + crate::consent_card::LAUNCH_SETTLE;
    assert_eq!(
        app.devtools_hand_over(Origin::Ctl, Moment::Quiet),
        HandOver::Listing
    );
    let asked = triggers();
    app.tick_local_network_step(t0);
    assert_eq!(
        triggers(),
        asked + 1,
        "the alert comes up while the run is out"
    );
    let opens = test_arms::opens();
    run_out(&mut app, t0);
    assert_eq!(
        (test_arms::opens(), app.devtools_helper.queued()),
        (opens, Some(Origin::Ctl)),
        "the run's end finds the alert up: the ask waits"
    );
    // The first look that reads a touch since the ask starts the person's time with the alert.
    let touched = t0 + crate::devtools_helper::QUIET_FOR + Duration::from_secs(1);
    app.tick_consent_lanes(touched);
    assert_eq!(test_arms::opens(), opens, "the hold stands");
    app.tick_consent_lanes(touched + LOCAL_NETWORK_ALERT_HOLD);
    run_out(&mut app, touched + LOCAL_NETWORK_ALERT_HOLD);
    assert_eq!(
        (test_arms::opens(), RUNS.load(Ordering::SeqCst)),
        (opens + 1, 1),
        "its page once the hold is over, with its own run's answer"
    );
}

/// A Developer Tools reading made while no revision stands (the launch read before the first
/// setup status) takes no version-keyed marker (`aterm doctor`'s) for a revision's page:
/// it asks, the window's own step waits queued for the revision, and its page comes up at the
/// step moment once the revision stands.
#[cfg(target_os = "macos")]
#[test]
fn a_developer_tools_reading_with_no_revision_queues_the_step_for_the_revision() {
    use crate::consent_state::SlotKind;
    use crate::devtools_helper::{Origin, test_arms};
    use aterm_containment::consent::FdaState;
    use aterm_containment::{DevToolProbe, DevToolProbeLabel, DevToolState, ProbeGate};
    fn not_on(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
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
    set_front(true);
    let doctors = test_arms::marked(|| Some((aterm_types::version::app_version().to_owned(), 1)));
    let mut app = new_mac(doctors, test_arms::fda_opening());
    let (tx, rx) = std::sync::mpsc::channel();
    let mut consent = reread(FdaState::Granted);
    consent.set_devtools_arms_for_test(not_on, subject);
    consent.set_slot_wake(std::sync::Arc::new(move |kind: SlotKind| {
        if kind == SlotKind::Devtools {
            let _ = tx.send(());
        }
    }));
    app.consent = consent;
    focus(&mut app, true);
    app.set_privacy_for_test(switches(None));
    app.observe_initial_setup_permissions(None);
    app.read_devtools_list(Instant::now());
    rx.recv_timeout(Duration::from_secs(60))
        .unwrap_or_else(|_| panic!("the launch read runs"));
    app.on_devtools_probe_ready();
    assert_eq!(
        app.devtools_helper.queued(),
        Some(Origin::Automatic),
        "the step waits for its revision"
    );
    approve(&mut app, 1);
    let opens = test_arms::opens();
    let mut t = Instant::now();
    parks(&mut app, &mut t, 3);
    assert_eq!(test_arms::opens(), opens + 1, "the revision's own page");
}

/// The helper panel's Show it is a door the window opens: a page a person's press brought up
/// before any revision stood, shown again by Show it once a revision is approved, writes the
/// revision's marker and `handed` and answers the window's own step waiting it out, so that
/// step does not bring the page up a second time in the revision.
#[cfg(target_os = "macos")]
#[test]
fn the_panels_show_it_puts_its_page_on_record() {
    use crate::devtools_helper::{HandOver, Origin, PanelEvent, test_arms};
    use aterm_containment::consent::FdaState;
    use std::cell::RefCell;
    thread_local! {
        static MARK: RefCell<Option<String>> = const { RefCell::new(None) };
    }
    fn recorded(revision: u64, access: Access) -> Option<AccessOutcome> {
        if access != Access::DeveloperTools {
            return Some(AccessOutcome::Triggered);
        }
        records()
            .into_iter()
            .rev()
            .find(|&(r, a, _)| (r, a) == (revision, access))
            .map(|(_, _, outcome)| outcome)
    }
    fn handed(revision: u64, access: Access) -> bool {
        records().contains(&(revision, access, AccessOutcome::Handed))
    }
    let mut marking = test_arms::opening();
    // The panel's own list reads keep aterm off, so only Show it answers the step, and the panel
    // never turns Done (whose real-time close would race the loop below).
    marking.read_list = |_, _| test_arms::absent(2);
    marking.last_opened = || MARK.with(|mark| mark.borrow().clone().map(|word| (word, 1)));
    marking.note_opened = |word, _| MARK.with(|mark| *mark.borrow_mut() = Some(word.to_owned()));
    RECORDS.with(|r| r.borrow_mut().clear());
    set_front(true);
    let mut app = new_mac(marking, test_arms::fda_opening());
    app.access_steps.set_reach(StepsReach {
        recorded,
        handed,
        ..app.access_steps.reach
    });
    app.consent = reread(FdaState::Granted);
    test_arms::stand_in(&mut app, marking);
    app.observe_initial_setup_permissions(None);
    assert_eq!(
        app.hand_devtools_to_owner(Origin::Automatic),
        HandOver::Queued
    );
    let pressed = Instant::now();
    assert_eq!(
        app.hand_devtools_to_owner(Origin::Settings),
        HandOver::Opened
    );
    approve(&mut app, 1);
    let mut t = pressed;
    parks(&mut app, &mut t, 1);
    assert_eq!(
        (
            app.devtools_helper.queued(),
            handed(1, Access::DeveloperTools)
        ),
        (Some(Origin::Automatic), false),
        "precondition: the step waits out a page that is not the revision's"
    );
    let opens = test_arms::opens();
    app.devtools_helper.press_for_test(PanelEvent::ShowList);
    let start = Instant::now();
    while test_arms::opens() == opens {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "Show it brings the page up"
        );
        app.tick_devtools_hand_over(Instant::now());
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        MARK.with(|mark| mark.borrow().clone()),
        Some(atpkg::devtools::revision_key(1)),
        "the revision's marker"
    );
    assert!(handed(1, Access::DeveloperTools), "and its `handed`");
    assert_eq!(app.devtools_helper.queued(), None, "the step is answered");
    let mut t = pressed + Duration::from_secs(atpkg::devtools::REOPEN_AFTER_S);
    parks(&mut app, &mut t, 3);
    assert_eq!(test_arms::opens(), opens + 1, "once in the revision");
}

/// Show it while no revision stands marks its page as the door does: the marker under the
/// running version, and nothing on record, so the window's own step waits on for a revision and
/// brings that revision's page up once the page's ten minutes are out.
#[cfg(target_os = "macos")]
#[test]
fn the_panels_show_it_with_no_revision_marks_its_page_and_leaves_the_step() {
    use crate::devtools_helper::{HandOver, Origin, PanelEvent, test_arms};
    use aterm_containment::consent::FdaState;
    use std::cell::RefCell;
    thread_local! {
        static MARK: RefCell<Option<String>> = const { RefCell::new(None) };
    }
    fn recorded(revision: u64, access: Access) -> Option<AccessOutcome> {
        if access != Access::DeveloperTools {
            return Some(AccessOutcome::Triggered);
        }
        records()
            .into_iter()
            .rev()
            .find(|&(r, a, _)| (r, a) == (revision, access))
            .map(|(_, _, outcome)| outcome)
    }
    fn handed(revision: u64, access: Access) -> bool {
        records().contains(&(revision, access, AccessOutcome::Handed))
    }
    let mut marking = test_arms::opening();
    // As in `the_panels_show_it_puts_its_page_on_record`: the panel never turns Done.
    marking.read_list = |_, _| test_arms::absent(2);
    marking.last_opened = || MARK.with(|mark| mark.borrow().clone().map(|word| (word, 1)));
    marking.note_opened = |word, _| MARK.with(|mark| *mark.borrow_mut() = Some(word.to_owned()));
    RECORDS.with(|r| r.borrow_mut().clear());
    set_front(true);
    let mut app = new_mac(marking, test_arms::fda_opening());
    app.access_steps.set_reach(StepsReach {
        recorded,
        handed,
        ..app.access_steps.reach
    });
    app.consent = reread(FdaState::Granted);
    test_arms::stand_in(&mut app, marking);
    app.observe_initial_setup_permissions(None);
    assert_eq!(
        app.hand_devtools_to_owner(Origin::Automatic),
        HandOver::Queued
    );
    assert_eq!(
        app.hand_devtools_to_owner(Origin::Settings),
        HandOver::Opened
    );
    MARK.with(|mark| *mark.borrow_mut() = None);
    let opens = test_arms::opens();
    app.devtools_helper.press_for_test(PanelEvent::ShowList);
    let start = Instant::now();
    while test_arms::opens() == opens {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "Show it brings the page up"
        );
        app.tick_devtools_hand_over(Instant::now());
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        MARK.with(|mark| mark.borrow().clone()),
        Some(app.access_marks().devtools),
        "the marker under the running version, as the door writes it"
    );
    assert_ne!(
        app.access_marks().devtools,
        atpkg::devtools::revision_key(1),
        "precondition: no revision's key"
    );
    assert_eq!(
        (
            app.devtools_helper.queued(),
            records()
                .iter()
                .any(|&(_, access, _)| access == Access::DeveloperTools)
        ),
        (Some(Origin::Automatic), false),
        "the step waits on for a revision, with nothing on record"
    );
    approve(&mut app, 1);
    let mut t = Instant::now() + Duration::from_secs(atpkg::devtools::REOPEN_AFTER_S);
    let opens = test_arms::opens();
    parks(&mut app, &mut t, 3);
    assert_eq!(
        test_arms::opens(),
        opens + 1,
        "the revision's own page, once"
    );
}

/// A new access revision's Developer Tools step that waits out a page under ten minutes old
/// keeps that wait through the window's own reading while its Full Disk Access probe is still
/// out (`App::hold_devtools_for_reading`): the hold's look a panel tick on is the reading's,
/// and a look within the page's ten minutes would refuse the step as recent and spend it.
#[cfg(target_os = "macos")]
#[test]
fn a_new_revisions_developer_tools_step_waits_out_a_recent_page_through_a_reading_still_out() {
    use crate::devtools_helper::{DevToolsHelperArm, HandOver, Lane, Origin, test_arms};
    use aterm_containment::DevToolGrant;
    use aterm_containment::consent::{FdaProbe, FdaState, ProbeLabel};
    use std::cell::Cell;
    thread_local! {
        static AGO: Cell<Option<u64>> = const { Cell::new(None) };
    }
    let mut marking = test_arms::opening();
    marking.opened_within = |_| AGO.with(Cell::get);
    set_front(true);
    AGO.with(|ago| ago.set(None));
    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    app.consent = reread(FdaState::Granted);
    current_devtools_not_granted(&mut app, 2);
    app.devtools_helper = Lane::windowed_for_test(marking, DevToolsHelperArm::inert());
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
        Instant::now(),
    );
    let opens = test_arms::opens();
    let mut t = Instant::now();
    approve(&mut app, 1);
    parks(&mut app, &mut t, 1);
    // A page aterm brought up 30 seconds ago (`aterm doctor`'s, say), then revision 2.
    AGO.with(|ago| ago.set(Some(30)));
    approve(&mut app, 2);
    parks(&mut app, &mut t, 1);
    app.on_devtools_probe_ready();
    assert_eq!(
        (test_arms::opens(), app.devtools_helper.queued()),
        (opens, Some(Origin::Automatic)),
        "a wait, not a refusal"
    );
    app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(
        FdaProbe::refused(ProbeLabel::Pending),
    );
    assert_eq!(
        app.hand_devtools_to_owner(Origin::Automatic),
        HandOver::Queued,
        "the Full Disk Access reading is still out"
    );
    app.consent = reread(FdaState::Granted);
    parks(&mut app, &mut t, 3);
    assert_eq!(
        (test_arms::opens(), app.devtools_helper.queued()),
        (opens, Some(Origin::Automatic)),
        "the page's ten minutes still hold it"
    );
    AGO.with(|ago| ago.set(None));
    let mut t = Instant::now() + Duration::from_secs(atpkg::devtools::REOPEN_AFTER_S);
    parks(&mut app, &mut t, 3);
    assert_eq!(
        (test_arms::opens(), app.devtools_helper.queued()),
        (opens + 1, None),
        "then revision 2's page"
    );
}

/// The control verb's ask whose own `spctl` run ended after its quiet moment passed rides the
/// waiting Developer Tools step with that run's answer, and at its next quiet moment brings the
/// page up with no run of its own.
#[cfg(target_os = "macos")]
#[test]
fn the_control_verbs_ask_rides_the_step_with_its_own_runs_answer() {
    use crate::devtools_helper::{
        DevToolsHelperArm, HandOver, Lane, Moment, Origin, RECHECK_EVERY, test_arms,
    };
    use aterm_containment::DevToolGrant;
    use std::sync::atomic::{AtomicU32, Ordering};
    // `spctl` runs on the lane's worker thread.
    static RUNS: AtomicU32 = AtomicU32::new(0);
    let mut counted = test_arms::opening();
    counted.list_terminal = || {
        RUNS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    let run_out = |app: &mut App, at: Instant| {
        let start = Instant::now();
        while app.devtools_helper.listing().is_some() {
            assert!(start.elapsed() < Duration::from_secs(60), "hang");
            app.tick_devtools_hand_over(at);
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    set_front(false);
    let mut app = new_mac(counted, test_arms::fda_opening());
    app.consent = reread(aterm_containment::consent::FdaState::Granted);
    test_arms::stand_in(&mut app, counted);
    app.devtools_helper = Lane::windowed_for_test(counted, DevToolsHelperArm::inert());
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(0)),
        Instant::now(),
    );
    focus(&mut app, true);
    app.user_input_recent = |within| within == crate::devtools_helper::PRESENT_WITHIN;
    let t0 = Instant::now() + crate::consent_card::LAUNCH_SETTLE;
    app.observe_initial_setup_permissions(None);
    assert_eq!(
        app.hand_devtools_to_owner(Origin::Automatic),
        HandOver::Queued
    );
    assert_eq!(
        app.devtools_hand_over(Origin::Ctl, Moment::Quiet),
        HandOver::Listing
    );
    // The run ends with no aterm window in front: the verb's moment passed.
    focus(&mut app, false);
    run_out(&mut app, t0);
    let opens = test_arms::opens();
    assert_eq!(
        (RUNS.load(Ordering::SeqCst), app.devtools_helper.queued()),
        (1, Some(Origin::Automatic)),
        "the ask rides the step"
    );
    focus(&mut app, true);
    for park in 0..3 {
        app.tick_consent_lanes(t0 + RECHECK_EVERY * park);
    }
    run_out(&mut app, t0 + RECHECK_EVERY * 3);
    assert_eq!(
        (test_arms::opens(), RUNS.load(Ordering::SeqCst)),
        (opens + 1, 1),
        "its page, with its own run's answer"
    );
}

/// [`the_control_verbs_ask_that_rides_the_step_keeps_its_own_quiet_moment`] for the Full Disk
/// Access page: the window's own step waits for a revision, and a person's press on the row
/// brings the page up meanwhile. The step waits on, and when the same revision is back it brings
/// its own page up once the press's page is ten minutes old. Where that revision brought the
/// page up already (the press keeps its word), the step is refused as shown at once instead of
/// holding the steps behind it for those ten minutes. No panel holds the step moment here.
#[cfg(target_os = "macos")]
#[test]
fn a_press_with_no_revision_leaves_the_full_disk_access_step_waiting_for_it() {
    use crate::devtools_helper::{DevToolsHelperArm, HandOver, Lane, Moment, Origin, test_arms};
    use aterm_containment::consent::FdaState;
    use std::cell::{Cell, RefCell};
    thread_local! {
        static RECENT: Cell<bool> = const { Cell::new(false) };
        static MARK: RefCell<Option<String>> = const { RefCell::new(None) };
    }
    let mut marking = test_arms::fda_marking(
        |_| MARK.with(|mark| mark.borrow().clone().map(|word| (word, 1))),
        |word| {
            RECENT.with(|recent| recent.set(true));
            MARK.with(|mark| *mark.borrow_mut() = Some(word.to_owned()));
        },
    );
    marking.opened_within = |_| RECENT.with(Cell::get).then_some(0);
    set_front(true);
    for shown in [false, true] {
        RECENT.with(|recent| recent.set(false));
        MARK.with(|mark| *mark.borrow_mut() = shown.then(|| atpkg::devtools::revision_key(1)));
        let mut app = new_mac(test_arms::opening(), marking);
        app.devtools_helper = Lane::windowed_with_fda_for_test(
            test_arms::opening(),
            marking,
            DevToolsHelperArm::inert(),
        );
        app.access_steps.set_reach(StepsReach {
            recorded: |_, access| {
                (access == Access::LocalNetwork).then_some(AccessOutcome::Triggered)
            },
            frontmost: front,
            ..stepping()
        });
        app.observe_initial_setup_permissions(None);
        assert_eq!(
            app.fda_hand_over(Origin::Automatic, Moment::Asked, FdaState::Denied),
            HandOver::Queued,
            "no revision, so it waits"
        );
        let opens = test_arms::fda_counts().0;
        assert!(app.open_access_from_system_state(Access::FullDiskAccess, true));
        assert_eq!(
            (test_arms::fda_counts().0, app.devtools_helper.fda_queued()),
            (opens + 1, Some(Origin::Automatic)),
            "shown {shown}: the press brings the page up, and the step waits on for its revision"
        );
        // The page asks for a new reading; it answers still off.
        app.consent = reread(FdaState::Denied);
        approve(&mut app, 1);
        // Past the press's pane hold, within the page's ten minutes.
        let mut t = Instant::now() + super::PANE_HOLD;
        parks(&mut app, &mut t, 3);
        assert_eq!(
            (test_arms::fda_counts().0, app.devtools_helper.fda_queued()),
            if shown {
                (opens + 1, None)
            } else {
                (opens + 1, Some(Origin::Automatic))
            },
            "shown {shown}: within the page's ten minutes"
        );
        // Ten minutes on.
        RECENT.with(|recent| recent.set(false));
        let mut t = Instant::now() + Duration::from_secs(atpkg::devtools::REOPEN_AFTER_S);
        parks(&mut app, &mut t, 3);
        assert_eq!(
            (test_arms::fda_counts().0, app.devtools_helper.fda_queued()),
            (opens + 1 + u32::from(!shown), None),
            "shown {shown}: then revision 1's own page, unless it came up already"
        );
    }
}

/// An ask the waiting Developer Tools step carried waits for the step's `spctl` run too, and
/// takes its answer. The run starts when the step is queued, ahead of its moment, and here it
/// ends after the step moment passed, the step riding it. Either the step waits again with the
/// act, or it is refused there (the record came to hold its revision's page), and the act,
/// judged by its own rules as a run that ended then, waits for its own moment. Once the step is
/// refused, the act brings the page up at that moment, with no run of its own.
#[cfg(target_os = "macos")]
#[test]
fn an_ask_the_developer_tools_step_carried_waits_for_the_steps_spctl_run() {
    use crate::devtools_helper::{HandOver, Listing, Origin, test_arms};
    use aterm_containment::DevToolGrant;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    thread_local! {
        static HANDED: Cell<bool> = const { Cell::new(false) };
    }
    // `spctl` runs on the lane's worker thread, and ends once the test lets it.
    static RUNS: AtomicU32 = AtomicU32::new(0);
    static ENDS: AtomicBool = AtomicBool::new(false);
    let mut counted = test_arms::opening();
    counted.list_terminal = || {
        RUNS.fetch_add(1, Ordering::SeqCst);
        let start = Instant::now();
        while !ENDS.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    };
    for refused_at_end in [false, true] {
        set_front(true);
        HANDED.with(|handed| handed.set(false));
        RUNS.store(0, Ordering::SeqCst);
        ENDS.store(false, Ordering::SeqCst);
        let mut app = new_mac(counted, test_arms::fda_opening());
        app.access_steps.set_reach(StepsReach {
            handed: |revision, access| {
                HANDED.with(Cell::get) && (revision, access) == (1, Access::DeveloperTools)
            },
            ..app.access_steps.reach
        });
        app.consent = reread(aterm_containment::consent::FdaState::Granted);
        app.devtools_helper.note_reading(
            test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(0)),
            Instant::now(),
        );
        let mut t = Instant::now();
        app.settle_access_steps(t);
        set_front(false);
        assert_eq!(
            app.hand_devtools_to_owner(Origin::Automatic),
            HandOver::Queued
        );
        assert!(
            app.devtools_helper.listing_ahead(),
            "the run starts as the step is queued"
        );
        assert!(
            !app.open_access_from_system_state(Access::DeveloperTools, false),
            "the act waits for the step moment"
        );
        set_front(true);
        for park in 0.. {
            assert!(park < 3, "the step moment came");
            parks(&mut app, &mut t, 1);
            if app.devtools_helper.listing() == Some(Listing::HandOver(Origin::Automatic)) {
                break;
            }
        }
        set_front(false);
        HANDED.with(|handed| handed.set(refused_at_end));
        ENDS.store(true, Ordering::SeqCst);
        let opens = test_arms::opens();
        let start = Instant::now();
        while app.devtools_helper.listing().is_some() {
            assert!(start.elapsed() < Duration::from_secs(60), "hang");
            app.tick_devtools_hand_over(t);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            (
                test_arms::opens(),
                RUNS.load(Ordering::SeqCst),
                app.devtools_helper.queued()
            ),
            if refused_at_end {
                (opens, 1, Some(Origin::SystemStateAct))
            } else {
                (opens, 1, Some(Origin::Automatic))
            },
            "refused at its end {refused_at_end}: the moment passed, so nothing comes up"
        );
        HANDED.with(|handed| handed.set(true));
        set_front(true);
        parks(&mut app, &mut t, 3);
        assert_eq!(
            (
                test_arms::opens(),
                RUNS.load(Ordering::SeqCst),
                app.devtools_helper.queued()
            ),
            (opens + 1, 1, None),
            "refused at its end {refused_at_end}: the act's page, with the step's run's answer"
        );
    }
}

/// Full Disk Access granted in a process whose launch read of the Developer Tools list said
/// `unknown` (no Full Disk Access to read it with) reads the list again at once, with no focus
/// gain; its `not_granted` queues that page in the same process with the `spctl` run already
/// started, and the step moment brings the page up with no run of its own. A list read again
/// before the moment (Terminal listed) brings the page up as any listed one does, and spends the
/// run ahead with it. A read another reader made just before the grant does not hold the grant's
/// back. Negative control: with no grant observed, parks and a focus gain read nothing more and
/// queue nothing.
#[cfg(target_os = "macos")]
#[test]
fn a_full_disk_access_grant_brings_the_developer_tools_page_in_the_same_process() {
    use crate::consent_state::SlotKind;
    use crate::devtools_helper::{Origin, test_arms};
    use aterm_containment::consent::FdaState;
    use aterm_containment::{DevToolProbe, DevToolProbeLabel, DevToolState, ProbeGate};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    static FDA: AtomicBool = AtomicBool::new(false);
    static LISTED: AtomicBool = AtomicBool::new(false);
    static READS: AtomicU32 = AtomicU32::new(0);
    static RUNS: AtomicU32 = AtomicU32::new(0);
    fn tccd(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
        READS.fetch_add(1, Ordering::SeqCst);
        if FDA.load(Ordering::SeqCst) {
            DevToolProbe {
                state: DevToolState::Unlisted,
                label: DevToolProbeLabel::Observed,
                requests: Some(1),
                not_allowed: Some(1),
                answered_at_ms: Some(1),
            }
        } else {
            DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest)
        }
    }
    fn list(_gate: ProbeGate, _subject: &str) -> aterm_containment::consent::DevToolList {
        if FDA.load(Ordering::SeqCst) {
            test_arms::absent(u32::from(LISTED.load(Ordering::SeqCst)))
        } else {
            test_arms::unread()
        }
    }
    fn subject() -> Option<&'static str> {
        Some(test_arms::SUBJECT)
    }
    for relisted in [false, true] {
        set_front(true);
        FDA.store(false, Ordering::SeqCst);
        LISTED.store(false, Ordering::SeqCst);
        let mut reach = test_arms::opening();
        reach.list_terminal = || {
            RUNS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        let mut t = Instant::now();
        let mut app = new_mac(reach, test_arms::fda_opening());
        app.config.privacy = Some(switches(None));
        let (tx, rx) = std::sync::mpsc::channel();
        let consent = |fda: FdaState| {
            let mut consent = reread(fda);
            consent.set_devtools_arms_for_test(tccd, subject);
            consent.probes_for_test().devtools_list = list;
            let tx = tx.clone();
            consent.set_slot_wake(std::sync::Arc::new(move |kind: SlotKind| {
                if kind == SlotKind::Devtools {
                    let _ = tx.send(());
                }
            }));
            consent
        };
        let read = |app: &mut App, what: &str| {
            if let Err(e) = rx.recv_timeout(Duration::from_secs(60)) {
                panic!("the {what} read completes ({relisted}): {e:?}");
            }
            app.on_devtools_probe_ready();
        };
        app.consent = consent(FdaState::Denied);
        focus(&mut app, true);
        let reads = READS.load(Ordering::SeqCst);
        app.read_devtools_list(t);
        read(&mut app, "launch");
        assert_eq!(
            app.devtools_helper.reading().map(|reading| reading.grant),
            Some(aterm_containment::DevToolGrant::Unknown),
            "PRECONDITION: no Full Disk Access, an unread list"
        );
        parks(&mut app, &mut t, 3);
        app.read_devtools_list(t);
        assert_eq!(
            (READS.load(Ordering::SeqCst), app.devtools_helper.queued()),
            (reads + 1, None),
            "the control: nothing reads again while Full Disk Access is off"
        );

        FDA.store(true, Ordering::SeqCst);
        app.consent = consent(FdaState::Granted);
        // Another reader (System State's Security page) read the list just before the grant:
        // the grant's read is not held back by that read's retry floor.
        app.consent.hold_retry_floors_for_test();
        let runs = RUNS.load(Ordering::SeqCst);
        app.note_macos_access_granted(t);
        read(&mut app, "grant's");
        assert_eq!(READS.load(Ordering::SeqCst), reads + 2, "read at once");
        assert_eq!(
            app.devtools_helper.queued(),
            Some(Origin::Automatic),
            "not on the list: the page is asked for in this process"
        );
        set_front(false);
        let start = Instant::now();
        while app.devtools_helper.listing_ahead() {
            assert!(start.elapsed() < Duration::from_secs(60), "hang");
            app.tick_devtools_hand_over(t);
            std::thread::sleep(Duration::from_millis(1));
        }
        set_front(true);
        assert_eq!(
            RUNS.load(Ordering::SeqCst),
            runs + 1,
            "the list reads empty: `spctl` ran ahead of the moment"
        );
        if relisted {
            LISTED.store(true, Ordering::SeqCst);
            app.consent.invalidate_devtools();
            let gate = app.config.privacy_probe_gate();
            let observer_off = app.config.privacy_observer_off();
            // The probe's slot takes a new read once its retry floor after the grant's has passed.
            let start = Instant::now();
            while app.consent.devtools(gate, observer_off).pending() {
                assert!(start.elapsed() < Duration::from_secs(60), "hang");
                std::thread::sleep(Duration::from_millis(1));
            }
            app.on_devtools_probe_ready();
            assert!(
                app.devtools_helper
                    .reading()
                    .is_some_and(|reading| reading.list.rows == Some(1)),
                "PRECONDITION: the list read again names Terminal"
            );
        }
        let opens = test_arms::opens();
        parks(&mut app, &mut t, 1);
        assert_eq!(
            test_arms::opens(),
            opens + 1,
            "the page comes up ({relisted})"
        );
        assert_eq!(
            (RUNS.load(Ordering::SeqCst), app.devtools_helper.listing()),
            (runs + 1, None),
            "with no run at its moment ({relisted})"
        );
        assert!(
            !app.devtools_helper.take_listed_ahead(),
            "the run ahead is spent by the hand-over it served ({relisted})"
        );
    }
}

/// The same for the Full Disk Access step, which waits again at its moment while the Developer
/// Tools page is within its ten minutes: a client's act on the row rides with it, and once the
/// step is refused (here `fda-pane-opened` comes to name revision 1 for this bundle and Mac) the
/// act is judged by its own rules.
#[cfg(target_os = "macos")]
#[test]
fn an_ask_the_full_disk_access_step_carried_waits_with_it_when_it_waits_again() {
    use crate::devtools_helper::{HandOver, Moment, Origin, test_arms};
    use aterm_containment::consent::FdaState;
    use std::cell::Cell;
    thread_local! {
        static DEVTOOLS_AGO: Cell<Option<u64>> = const { Cell::new(None) };
        static SHOWN: Cell<bool> = const { Cell::new(false) };
    }
    let reach =
        test_arms::opened(|window| DEVTOOLS_AGO.with(Cell::get).filter(|ago| *ago < window));
    let fda = test_arms::fda_marking(
        |_| {
            SHOWN
                .with(Cell::get)
                .then(|| (atpkg::devtools::revision_key(1), 1))
        },
        |_| {},
    );
    let on_record = StepsReach {
        recorded: |_, access| (access == Access::LocalNetwork).then_some(AccessOutcome::Triggered),
        frontmost: front,
        ..stepping()
    };
    set_front(true);
    for act in [false, true] {
        DEVTOOLS_AGO.with(|ago| ago.set(Some(atpkg::devtools::REOPEN_AFTER_S - 1)));
        SHOWN.with(|shown| shown.set(false));
        let mut app = new_mac(reach, fda);
        app.access_steps.set_reach(on_record);
        let mut t = Instant::now();
        app.observe_initial_setup_permissions(None);
        assert_eq!(
            app.fda_hand_over(Origin::Automatic, Moment::Asked, FdaState::Denied),
            HandOver::Queued
        );
        approve(&mut app, 1);
        if act {
            assert!(!app.open_access_from_system_state(Access::FullDiskAccess, false));
        }
        let opens = test_arms::fda_counts().0;
        parks(&mut app, &mut t, 2);
        assert_eq!(
            (test_arms::fda_counts().0, app.devtools_helper.fda_queued()),
            (opens, Some(Origin::Automatic)),
            "act {act}: the step waits out the Developer Tools page's minutes"
        );
        DEVTOOLS_AGO.with(|ago| ago.set(None));
        SHOWN.with(|shown| shown.set(true));
        parks(&mut app, &mut t, 3);
        assert_eq!(
            test_arms::fda_counts().0,
            opens + u32::from(act),
            "act {act}"
        );
    }
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
/// the same revision once the switch is on, and a timed retry (a LAN named but no socket) that
/// fell due meanwhile is moved on (never left due) and looks again when it next falls due.
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
            lan_here: || super::LanHere::Lan,
            ..stepping()
        });
        focus(&mut app, true);
        approve(&mut app, 7);
        let tried = failed_triggers();
        app.tick_local_network_step(t0);
        assert_eq!(failed_triggers(), tried + 1, "PRECONDITION: no socket yet");
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

/// A real pass asks its folders together (the launch pass's looks run at once), one of them
/// held in its inert read. Revoking its admission retires the pass: no look starts after it, the
/// pass ends once the look in flight returns, and a new approval starts a new pass.
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
    let asked_together = || {
        let mut asked = [
            calls.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap(),
            calls.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap(),
        ];
        asked.sort();
        asked
    };
    app.launch_warmup_look(now, |_, _| true);
    entered.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert_eq!(
        asked_together(),
        ["Documents", "Downloads"],
        "both looks are asked at once, one still held in its read"
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
        "the revoked pass starts no look after its retire"
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
    assert_eq!(asked_together(), ["Documents", "Downloads"]);
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

/// A synchronous current reading for the older rearm/queue fixtures: both
/// the retained Lane and the newly requested consent read name the same fact.
#[cfg(target_os = "macos")]
fn current_devtools_not_granted(app: &mut App, rows: u32) {
    use crate::devtools_helper::test_arms;
    use aterm_containment::{DevToolProbe, DevToolProbeLabel, DevToolState};
    let probes = app.consent.probes_for_test();
    probes.live = false;
    probes.fda = |_| probe(aterm_containment::consent::FdaState::Granted);
    probes.subject = || Some(test_arms::SUBJECT);
    probes.devtools = |_, _| DevToolProbe {
        state: DevToolState::Unlisted,
        label: DevToolProbeLabel::Observed,
        requests: Some(1),
        not_allowed: Some(1),
        answered_at_ms: None,
    };
    probes.devtools_list = match rows {
        0 => |_, _| test_arms::absent(0),
        1 => |_, _| test_arms::absent(1),
        _ => |_, _| test_arms::absent(2),
    };
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
/// hand-over unasked, so the folders that wait on it go on.
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

#[test]
fn local_network_grant_probe_frees_the_dialog_slot_before_recording_granted() {
    use super::LocalNetworkProbe;
    use aterm_types::macos_setup::{MacosDialog, macos_dialog_up};

    thread_local! {
        static PROBE_STARTED: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
        static PROBE_OUTCOME: std::cell::Cell<Option<(u64, LocalNetworkProbe)>> = const { std::cell::Cell::new(None) };
        static RECORDED_WITH_DIALOG_FREE: std::cell::RefCell<Vec<(u64, Access, AccessOutcome, bool)>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    let _serial = DIALOG_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(!macos_dialog_up());

    let reach = StepsReach {
        start_local_network_probe: |revision| {
            PROBE_STARTED.with(|v| v.borrow_mut().push(revision));
        },
        take_local_network_probe: || PROBE_OUTCOME.with(|c| c.take()),
        record: |revision, access, outcome| {
            let dialog_free = MacosDialog::when_none_up(Duration::ZERO).is_some();
            RECORDED_WITH_DIALOG_FREE.with(|v| {
                v.borrow_mut()
                    .push((revision, access, outcome, dialog_free))
            });
            Ok(())
        },
        dialog_up: macos_dialog_up,
        holds_dialog: true,
        ..stepping()
    };

    PROBE_STARTED.with(|v| v.borrow_mut().clear());
    PROBE_OUTCOME.with(|c| c.set(None));
    RECORDED_WITH_DIALOG_FREE.with(|v| v.borrow_mut().clear());

    let now = Instant::now();
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(reach);
    approve(&mut app, 91);

    app.tick_local_network_step(now);
    assert_eq!(PROBE_STARTED.with(|v| v.borrow().clone()), [91]);
    assert!(macos_dialog_up(), "Triggered holds the alert dialog slot");

    // When the 1 s re-probe hears a LAN server (Allow), the dialog slot is freed BEFORE
    // AccessOutcome::Granted is recorded so the worker's Secrets lane can start immediately.
    PROBE_OUTCOME.with(|c| c.set(Some((91, LocalNetworkProbe::Granted))));
    app.on_local_network_probe_ready(now + Duration::from_secs(2));
    assert!(!macos_dialog_up(), "Granted releases the alert dialog slot");
    assert_eq!(
        RECORDED_WITH_DIALOG_FREE.with(|v| v.borrow().clone()),
        [
            (91, Access::LocalNetwork, AccessOutcome::Triggered, false),
            (91, Access::LocalNetwork, AccessOutcome::Granted, true),
        ]
    );
    drop(app);

    // Negative control: Refused (or a stale revision) releases the hold without recording
    // AccessOutcome::Granted, and Unanswered leaves it to the person's time with the alert.
    PROBE_STARTED.with(|v| v.borrow_mut().clear());
    RECORDED_WITH_DIALOG_FREE.with(|v| v.borrow_mut().clear());
    let mut control = App::headless_for_test();
    control.access_steps.set_reach(reach);
    approve(&mut control, 92);
    control.tick_local_network_step(now);
    assert!(macos_dialog_up());

    PROBE_OUTCOME.with(|c| c.set(Some((91, LocalNetworkProbe::Granted))));
    control.on_local_network_probe_ready(now + Duration::from_secs(1));
    assert!(macos_dialog_up(), "stale revision is ignored");

    PROBE_OUTCOME.with(|c| c.set(Some((92, LocalNetworkProbe::Refused))));
    control.on_local_network_probe_ready(now + Duration::from_secs(2));
    assert!(!macos_dialog_up(), "Refused releases the dialog hold");
    assert_eq!(
        RECORDED_WITH_DIALOG_FREE.with(|v| v.borrow().clone()),
        [(92, Access::LocalNetwork, AccessOutcome::Triggered, false)]
    );
    drop(control);

    RECORDED_WITH_DIALOG_FREE.with(|v| v.borrow_mut().clear());
    let mut unanswered = App::headless_for_test();
    unanswered.access_steps.set_reach(reach);
    approve(&mut unanswered, 93);
    unanswered.tick_local_network_step(now);
    assert!(macos_dialog_up());

    // A re-probe that heard nothing says nothing of the alert: raised while nobody was at the
    // Mac, it is still up for them, so the hold and its slot stand for the person's time.
    PROBE_OUTCOME.with(|c| c.set(Some((93, LocalNetworkProbe::Unanswered))));
    let ended = now + Duration::from_secs(30);
    unanswered.on_local_network_probe_ready(ended);
    unanswered.settle_access_steps(ended);
    assert!(
        macos_dialog_up(),
        "Unanswered leaves the hold to the person"
    );
    unanswered.user_input_recent = |_| true;
    let back = ended + Duration::from_secs(1);
    unanswered.tick_local_network_step(back);
    unanswered.settle_access_steps(back);
    assert!(
        macos_dialog_up(),
        "the person back: their time with the alert starts"
    );
    unanswered.tick_local_network_step(back + LOCAL_NETWORK_ALERT_HOLD);
    unanswered.settle_access_steps(back + LOCAL_NETWORK_ALERT_HOLD);
    assert!(
        !macos_dialog_up(),
        "the person's time with the alert releases the dialog hold"
    );
    assert_eq!(
        RECORDED_WITH_DIALOG_FREE.with(|v| v.borrow().clone()),
        [(93, Access::LocalNetwork, AccessOutcome::Triggered, false)]
    );
}

/// Local Network's step tells the LAN readers (the converge lane, whose `sync.peers` read "macOS
/// has not asked" before) each time the answer moves as the window hears it: once as its question
/// goes up, after the record says `triggered` (which the access record keeps as asked, so a look
/// from then on reads past "has not asked"), and once as the re-probe ends, whatever it heard: a
/// grant, a Don't Allow, or nothing in its 30 s (no ALab vault on this network, as on e2e145, or
/// an answer after the probe gave up). Another revision's probe tells nothing.
/// NEGATIVE CONTROL: the rule before this (a nudge only for a heard grant or refusal) fails the
/// question's case, and the probe's end that heard nothing, the one e2e145 met.
#[test]
fn local_network_steps_tell_the_lan_readers_each_time_the_answer_moves() {
    use super::LocalNetworkProbe;

    type Heard = Option<(u64, LocalNetworkProbe)>;
    thread_local! {
        static OUTCOME: std::cell::Cell<Heard> = const { std::cell::Cell::new(None) };
        static NUDGES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        static ASKED_FIRST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    let reach = StepsReach {
        take_local_network_probe: || OUTCOME.with(std::cell::Cell::take),
        lan_answer_moved: || {
            NUDGES.with(|count| count.set(count.get() + 1));
            let asked = RECORDS.with(|records| {
                records
                    .borrow()
                    .contains(&(91, Access::LocalNetwork, AccessOutcome::Triggered))
            });
            ASKED_FIRST.with(|first| first.set(asked));
        },
        ..stepping()
    };
    let now = Instant::now();
    for probe in [
        LocalNetworkProbe::Granted,
        LocalNetworkProbe::Refused,
        LocalNetworkProbe::Unanswered,
    ] {
        let mut app = App::headless_for_test();
        app.access_steps.set_reach(reach);
        approve(&mut app, 91);
        RECORDS.with(|records| records.borrow_mut().clear());
        NUDGES.with(|count| count.set(0));
        ASKED_FIRST.with(|first| first.set(false));

        app.tick_local_network_step(now);
        assert_eq!(
            NUDGES.with(std::cell::Cell::get),
            1,
            "{probe:?}: the question went up"
        );
        assert!(
            ASKED_FIRST.with(std::cell::Cell::get),
            "{probe:?}: told once the record says asked"
        );

        OUTCOME.with(|outcome| outcome.set(Some((90, probe))));
        app.on_local_network_probe_ready(now + Duration::from_secs(1));
        assert_eq!(
            NUDGES.with(std::cell::Cell::get),
            1,
            "{probe:?}: another revision's"
        );

        OUTCOME.with(|outcome| outcome.set(Some((91, probe))));
        app.on_local_network_probe_ready(now + Duration::from_secs(31));
        assert_eq!(
            NUDGES.with(std::cell::Cell::get),
            2,
            "{probe:?}: the re-probe ended"
        );
    }
}

#[test]
fn swarm_off_starts_no_local_network_probe_and_discards_a_ready_grant() {
    use super::LocalNetworkProbe;
    use aterm_types::macos_setup::{MacosDialog, macos_dialog_up};

    thread_local! {
        static STARTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        static RECORDED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let _serial = DIALOG_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(StepsReach {
        start_local_network_probe: |_| STARTED.with(|count| count.set(count.get() + 1)),
        take_local_network_probe: || Some((91, LocalNetworkProbe::Granted)),
        record: |_, _, _| {
            RECORDED.with(|count| count.set(count.get() + 1));
            Ok(())
        },
        holds_dialog: true,
        ..stepping()
    });
    approve(&mut app, 91);
    app.config.swarm = Some(crate::app_config::SwarmConfig {
        enabled: Some(false),
    });
    STARTED.with(|count| count.set(0));
    RECORDED.with(|count| count.set(0));
    app.tick_local_network_step(now);
    assert_eq!(STARTED.with(std::cell::Cell::get), 0);
    assert_eq!(RECORDED.with(std::cell::Cell::get), 0);
    assert!(
        app.access_step_moment(now),
        "independent access steps remain eligible while swarm is Off"
    );

    // Turning Off while a grant is ready releases the old dialog hold without publishing the
    // grant that would wake the automatic Secrets lane.
    app.access_steps.local_network_done = Some(91);
    app.access_steps.alert_seen_at_for_test(now);
    app.access_steps.dialog = MacosDialog::when_none_up(Duration::ZERO);
    assert!(macos_dialog_up());
    app.on_local_network_probe_ready(now);
    assert!(!macos_dialog_up());
    assert_eq!(RECORDED.with(std::cell::Cell::get), 0);
}

/// With `[swarm] enabled = false` the window takes no Local Network step, so it tells the setup
/// worker the row is settled: an opt-out naming the switch calmly, which the Permissions stage
/// reads as not needed instead of waiting on a step that never runs. It claims no answer to
/// macOS's question. With the switch on, the row has no opt-out and the step runs.
/// NEGATIVE CONTROL: an `access_opt_outs` that publishes nothing for Local Network leaves the
/// stage waiting for good, and a System State tab reopening at each launch.
#[test]
fn swarm_off_settles_the_local_network_row_for_the_permissions_stage() {
    use super::SHARING_OFF_LAN;
    use aterm_types::macos_setup::opt_out::OptOut;
    let now = Instant::now();
    let lan = super::OPT_OUT_ROWS
        .iter()
        .position(|access| *access == Access::LocalNetwork)
        .unwrap();
    let mut app = App::headless_for_test();
    app.access_steps.set_reach(stepping());
    approve(&mut app, 92);
    assert_eq!(
        app.access_opt_outs()[lan],
        None,
        "the switch on: no opt-out"
    );
    assert!(app.local_network_pending(), "the switch on: the step runs");
    app.config.swarm = Some(crate::app_config::SwarmConfig {
        enabled: Some(false),
    });
    RECORDS.with(|r| r.borrow_mut().clear());
    for minute in 0..5 {
        app.tick_local_network_step(now + Duration::from_secs(60 * minute));
    }
    assert!(
        !records()
            .iter()
            .any(|(_, access, _)| *access == Access::LocalNetwork),
        "the switch off: no step"
    );
    assert_eq!(
        app.access_opt_outs()[lan],
        Some(OptOut::Always(SHARING_OFF_LAN)),
        "the switch off: the row's opt-out says why"
    );
}

/// The door a window's first LAN uses go through past their wait for the Local Network step reads
/// the window's keys at rest as the steps read their quiet moment: an aterm window has keyboard
/// focus, and no key went to aterm, nor did focus come back, within the steps' key rest.
#[test]
fn the_doors_key_rest_needs_focus_and_the_steps_quiet_seconds() {
    use crate::devtools_helper::QUIET_FOR;
    let now = Instant::now() + Duration::from_secs(3600);
    assert!(crate::keys_rest_at(true, None, now), "no key yet");
    assert!(crate::keys_rest_at(true, Some(now - QUIET_FOR), now));
    assert!(
        !crate::keys_rest_at(true, Some(now - QUIET_FOR + Duration::from_secs(1)), now),
        "a key within the rest"
    );
    assert!(
        !crate::keys_rest_at(false, None, now),
        "no aterm window has keyboard focus"
    );
}

/// When the window stops a Local Network alert watch that [`watch_through`] drives, as a time
/// since its first read: the hold it may end ended another way (the person's time, the
/// re-probe's answer), which stops the watch, as an alert never answered leaves it. It comes
/// past the watch's first looks ([`super::LOCAL_NETWORK_ALERT_FIRST_LOOKS`]), so a watch that
/// finds the person away reads at the away pace before it.
const WATCH_STOPPED: Duration = Duration::from_secs(60);

/// Windows of the alert's owner, each on screen.
fn on(numbers: &[u32]) -> Vec<super::AlertWindow> {
    numbers
        .iter()
        .map(|&number| super::AlertWindow {
            number,
            on_screen: true,
        })
        .collect()
}

/// Windows of the alert's owner, each listed off screen (ordered out).
fn off(numbers: &[u32]) -> Vec<super::AlertWindow> {
    numbers
        .iter()
        .map(|&number| super::AlertWindow {
            number,
            on_screen: false,
        })
        .collect()
}

/// One Local Network alert watch ([`super::watch_alert`]) driven by explicit times: its first
/// read, the baseline the trigger waits for, at `t0`, with the trigger arming it then; then, as
/// the live worker paces its reads, one every [`super::LOCAL_NETWORK_ALERT_LOOK`] after a read
/// within [`super::LOCAL_NETWORK_ALERT_FIRST_LOOKS`] of `t0` or one that found the person
/// present, and every [`super::LOCAL_NETWORK_ALERT_AWAY_LOOK`] after a later one that found them
/// away, while that look comes before `t0 + WATCH_STOPPED`, as the live worker's look ends once
/// the window stops the watch. Each read is `listed` at its time since `t0` (`None`: a window list that could not be
/// read), and the person is `present` at it. How the watch ended, and the times since `t0` it
/// read at.
fn watch_with(
    t0: Instant,
    listed: impl Fn(Duration) -> Option<Vec<super::AlertWindow>>,
    present: impl Fn(Duration) -> bool,
) -> (super::AlertWatchEnd, Vec<Duration>) {
    let at = std::cell::Cell::new(t0);
    let reads = RefCell::new(Vec::new());
    let end = super::watch_alert(
        || {
            let since = at.get().duration_since(t0);
            reads.borrow_mut().push(since);
            listed(since)
        },
        || present(at.get().duration_since(t0)),
        |here| {
            let first = at.get().duration_since(t0) < super::LOCAL_NETWORK_ALERT_FIRST_LOOKS;
            let next = at.get()
                + if here || first {
                    super::LOCAL_NETWORK_ALERT_LOOK
                } else {
                    super::LOCAL_NETWORK_ALERT_AWAY_LOOK
                };
            if next >= t0 + WATCH_STOPPED {
                return false;
            }
            at.set(next);
            true
        },
    );
    (end, reads.into_inner())
}

/// [`watch_with`] with the person at the Mac throughout.
fn watch_through(
    t0: Instant,
    listed: impl Fn(Duration) -> Option<Vec<super::AlertWindow>>,
) -> (super::AlertWatchEnd, Vec<Duration>) {
    watch_with(t0, listed, |_| true)
}

/// The last look before the window stops the watch, as a time since its first read.
fn last_look() -> Duration {
    WATCH_STOPPED - super::LOCAL_NETWORK_ALERT_LOOK
}

/// macOS's Local Network alert seen on screen after the trigger ends the watch `Answered` at the
/// first read that lists no window of the alert's owner at all, and not before: a window of the
/// owner that was up at the first read and stays holds it after the seen window is gone, since
/// it may be the alert itself, and a read that could not see the list ends nothing. NEGATIVE
/// CONTROL: a watch that ends once every seen window is gone, whatever the first read's windows
/// do (`listed.is_empty()` read as `seen.iter().all(|n| !listed.iter().any(|w| w.number == *n))`),
/// ends at 2 s, with window 7 still up.
#[test]
fn the_alert_watch_ends_at_the_read_that_lists_no_window_of_its_owner() {
    use super::AlertWatchEnd;
    let t0 = Instant::now();
    let (end, reads) = watch_through(t0, |at| match at.as_millis() {
        0..1000 => Some(on(&[7])),
        1000..1500 => Some(on(&[7, 48])),
        1500 => None,
        1750 => Some(on(&[48, 7])),
        2000..3000 => Some(on(&[7])),
        _ => Some(Vec::new()),
    });
    assert_eq!(end, AlertWatchEnd::Answered);
    assert_eq!(
        reads.last(),
        Some(&Duration::from_millis(3000)),
        "it ends at the read that lists no window of the owner"
    );
    assert_eq!(reads.len(), 13, "a read every quarter second until then");
}

/// A seen window that leaves the screen but stays in the window list (ordered out: a Space
/// switch, or a CFUserNotification taken down by its client, which stays listed off screen for
/// about 8 s, `docs/measured/unc-alert-window-list-2026-11-03.md`) ends nothing; the watch ends
/// `Answered` at the first read that lists no window of the owner, on screen or off. NEGATIVE
/// CONTROL: a watch that reads only the screen (`listed.is_empty()` read as
/// `listed.iter().all(|w| !w.on_screen)`) ends at 1 s, with window 48 still listed.
#[test]
fn a_seen_window_ordered_out_ends_the_alert_watch_only_once_it_leaves_the_list() {
    use super::AlertWatchEnd;
    let t0 = Instant::now();
    let (end, reads) = watch_through(t0, |at| match at.as_millis() {
        0..250 => Some(Vec::new()),
        250..1000 => Some(on(&[48])),
        1000..3000 => Some(off(&[48])),
        _ => Some(Vec::new()),
    });
    assert_eq!(end, AlertWatchEnd::Answered);
    assert_eq!(
        reads.last(),
        Some(&Duration::from_millis(3000)),
        "it ends once the window leaves the list"
    );
}

/// A window listed off screen at the first read (an alert taken down just before, still
/// listed) is not taken for one up before the trigger: drawn on screen after the trigger, it is
/// seen, and its going ends the watch `Answered`. NEGATIVE CONTROL: a baseline of every window
/// the first read lists (its `.filter(|window| window.on_screen)` taken out) never sees window 48
/// and ends `NeverSeen`.
#[test]
fn a_window_listed_off_screen_before_the_trigger_is_seen_once_drawn_after_it() {
    use super::AlertWatchEnd;
    let t0 = Instant::now();
    let (end, reads) = watch_through(t0, |at| match at.as_millis() {
        0 => Some(off(&[48])),
        250..1000 => Some(on(&[48])),
        _ => Some(Vec::new()),
    });
    assert_eq!(end, AlertWatchEnd::Answered);
    assert_eq!(reads.last(), Some(&Duration::from_millis(1000)));
}

/// A seen window's going ends nothing while the person is away (no touch within a minute, or
/// the screen locked or switched away for another login): an alert stands, unanswered, while
/// nobody is there, and its window going then is not known to be an answer. The watch reads
/// every quarter second for its first looks ([`super::LOCAL_NETWORK_ALERT_FIRST_LOOKS`]), then
/// every [`super::LOCAL_NETWORK_ALERT_AWAY_LOOK`] while the person is away, and ends `Answered`
/// at the first read that finds them back with no window of the owner listed. NEGATIVE
/// CONTROLS: a watch that ends whether or not the person is there (`&& here` taken out) ends at
/// 1 s; one that reads every quarter second while they are away (`look_again` given `true`
/// whatever the person) reads 181 times and ends at 45 s.
#[test]
fn an_alert_gone_while_the_person_is_away_ends_the_watch_only_once_they_are_back() {
    use super::AlertWatchEnd;
    let t0 = Instant::now();
    let (end, reads) = watch_with(
        t0,
        |at| match at.as_millis() {
            0..250 => Some(Vec::new()),
            250..1000 => Some(on(&[48])),
            _ => Some(Vec::new()),
        },
        |at| at < Duration::from_secs(1) || at >= Duration::from_secs(45),
    );
    assert_eq!(end, AlertWatchEnd::Answered);
    let ms: Vec<u128> = reads.iter().map(Duration::as_millis).collect();
    let paced: Vec<u128> = (0..=120)
        .map(|look| look * 250)
        .chain([35_000, 40_000, 45_000])
        .collect();
    assert_eq!(
        ms, paced,
        "a quarter second apart for the first 30 s, then five seconds apart while away"
    );
}

/// A seen window gone while the person is away, and the watch stopped before they come back,
/// ends it `GoneWhileAway`, never `Answered`. NEGATIVE CONTROL: an end that reads the last read
/// as `StillUp` whatever it listed (its `(false, true)` arm taken out) ends `StillUp`.
#[test]
fn an_alert_watch_stopped_while_the_person_is_away_ends_gone_while_away() {
    use super::AlertWatchEnd;
    let t0 = Instant::now();
    let (end, _) = watch_with(
        t0,
        |at| match at.as_millis() {
            0..250 => Some(Vec::new()),
            250..1000 => Some(on(&[48])),
            _ => Some(Vec::new()),
        },
        |at| at < Duration::from_secs(1),
    );
    assert_eq!(end, AlertWatchEnd::GoneWhileAway);
}

/// A Local Network alert already up at the watch's first read (an earlier LAN use raised it, and
/// macOS draws no second one) while another window of the same owner comes and is answered (a
/// tccd consent prompt): the first read's window 7 is still up, so the watch never ends
/// `Answered` and reads on until the window stops it, `StillUp`. NEGATIVE CONTROL: a watch that
/// ends once every seen window is gone (`listed.is_empty()` read as
/// `seen.iter().all(|n| !listed.iter().any(|w| w.number == *n))`) ends `Answered` as window 90
/// goes, with the alert still on screen.
#[test]
fn another_alert_of_the_owner_answered_while_the_first_reads_window_stays_ends_nothing() {
    use super::AlertWatchEnd;
    let t0 = Instant::now();
    let (end, reads) = watch_through(t0, |at| match at.as_millis() {
        0..1000 => Some(on(&[7])),
        1000..4000 => Some(on(&[7, 90])),
        _ => Some(on(&[7])),
    });
    assert_eq!(end, AlertWatchEnd::StillUp);
    assert_eq!(
        reads.last(),
        Some(&last_look()),
        "it reads until it is stopped"
    );
}

/// With no window of the alert's owner coming up after the trigger (macOS asked aterm before and
/// raises no alert), the watch reads every quarter second until the window stops it and ends
/// `NeverSeen`, so the hold ends only by the re-probe's answer or the person's time; it reads
/// nothing once stopped. NEGATIVE CONTROL: a watch that takes "no seen window is up" for the
/// answer (its `!seen.is_empty()` taken out) ends `Answered` at its first look.
#[test]
fn an_alert_watch_that_sees_no_alert_reads_until_it_is_stopped() {
    use super::AlertWatchEnd;
    let t0 = Instant::now();
    let (end, reads) = watch_through(t0, |_| Some(Vec::new()));
    assert_eq!(end, AlertWatchEnd::NeverSeen);
    assert_eq!(
        reads.len(),
        240,
        "the trigger's read and one each quarter second"
    );
    assert_eq!(reads.last(), Some(&last_look()), "none once it is stopped");
}

/// A window of the alert's owner already up at the first read (another app's alert, or one an
/// earlier ask left up) going away, with no new window seen, ends nothing: the trigger's alert may
/// still come, so the watch reads until it is stopped and ends `NeverSeen`. NEGATIVE CONTROL: a
/// watch that counts the first read's windows as seen (its `!baseline.contains(..)` taken out)
/// ends `Answered` as that window goes.
#[test]
fn a_window_up_before_the_trigger_going_away_does_not_end_the_alert_watch() {
    use super::AlertWatchEnd;
    let t0 = Instant::now();
    let (end, reads) = watch_through(t0, |at| {
        Some(if at < Duration::from_secs(1) {
            on(&[7])
        } else {
            Vec::new()
        })
    });
    assert_eq!(end, AlertWatchEnd::NeverSeen);
    assert_eq!(
        reads.last(),
        Some(&last_look()),
        "it reads until it is stopped"
    );
}

/// macOS's alert seen after the trigger and still up when the window stops the watch ends it
/// `StillUp` at its last look; one of two seen windows going while the other stays ends nothing.
/// NEGATIVE CONTROL: a watch that takes any seen window gone for the answer
/// (`listed.is_empty()` read as `seen.iter().any(|n| !listed.iter().any(|w| w.number == *n))`)
/// ends `Answered` as the second goes.
#[test]
fn an_alert_still_up_when_its_watch_is_stopped_ends_it_still_up() {
    use super::AlertWatchEnd;
    let t0 = Instant::now();
    let (end, reads) = watch_through(t0, |at| match at.as_millis() {
        0..250 => Some(Vec::new()),
        250..500 => Some(on(&[48, 49])),
        _ => Some(on(&[48])),
    });
    assert_eq!(end, AlertWatchEnd::StillUp);
    assert_eq!(
        reads.last(),
        Some(&last_look()),
        "it reads until it is stopped"
    );
}

/// A window list the watch could not read before the trigger leaves nothing to tell the alert
/// from: the watch ends `Blind` after that one read, whatever comes up later, so the hold ends
/// only by the re-probe's answer or the person's time. NEGATIVE CONTROL: a watch that takes the
/// failed read for an empty list (`unwrap_or_default` in place of its `Blind` arm) ends
/// `Answered` once a window came and went.
#[test]
fn an_alert_watch_that_cannot_read_the_list_at_the_trigger_ends_blind() {
    use super::AlertWatchEnd;
    let t0 = Instant::now();
    let (end, reads) = watch_through(t0, |at| match at.as_millis() {
        0 => None,
        250..1000 => Some(on(&[48])),
        _ => Some(Vec::new()),
    });
    assert_eq!(end, AlertWatchEnd::Blind);
    assert_eq!(reads, [Duration::ZERO], "one read, at the trigger");
}

/// The alert watch starts only on a macOS whose Local Network alert was measured as a window of
/// UserNotificationCenter, Tahoe 26 and Golden Gate 27; on any other, or an unknown version, no
/// watch starts and the hold ends only by the re-probe's answer or the person's time. NEGATIVE
/// CONTROL: with `alert_host_measured` answering `true` whatever the version, 28.0, 15.7.1 and an
/// unknown version start a watch.
#[test]
fn the_alert_watch_starts_only_where_its_alerts_host_was_measured() {
    use super::alert_host_measured;
    for measured in ["26.6.2", "26.0", "27.0", "27.0.1"] {
        assert!(alert_host_measured(Some(measured)), "{measured}");
    }
    for other in ["28.0", "15.7.1", "2", "", "27x"] {
        assert!(!alert_host_measured(Some(other)), "{other}");
    }
    assert!(!alert_host_measured(None), "an unknown version");
}

/// Every end of an alert watch makes one log record of its own kind, a watch the window stopped
/// first included (the hold's end by the person's time or the re-probe's answer, a later
/// start), with its revision and what its first read found; a watch that never read after its
/// first is told apart from one that read and saw nothing. Only an answer from the watch still
/// the window's is left and wakes the window. Each kind's aterm.log line differs from every
/// other's and names the revision. NEGATIVE CONTROLS: a worker that leaves before its record
/// once the watch is no longer the window's (an `if !live { return; }` after `settle`) logs
/// nothing for a stopped watch; a `NeverSeen` that ignores its looks logs `NeverSeen` for a watch
/// that never read again; a line for `AnsweredOnceStopped` that is `Answered`'s fails the check
/// that the kinds' lines differ.
#[test]
fn every_end_of_an_alert_watch_makes_one_record_of_its_kind() {
    use super::{
        AlertWatchEnd as End, AlertWatchEnded as Ended, AlertWatchLooks as Looks, BaselineRead,
        finish_alert_watch,
    };
    let cases = [
        (End::Answered, Looks::Read, true, Ended::Answered),
        (
            End::Answered,
            Looks::Read,
            false,
            Ended::AnsweredOnceStopped,
        ),
        (End::StillUp, Looks::Read, true, Ended::StillUp),
        (End::StillUp, Looks::Read, false, Ended::StillUp),
        (End::GoneWhileAway, Looks::Read, false, Ended::GoneWhileAway),
        (End::NeverSeen, Looks::Read, false, Ended::NeverSeen),
        (End::NeverSeen, Looks::Stopped, false, Ended::StoppedUnarmed),
        (
            End::NeverSeen,
            Looks::ArmWaitRanOut,
            true,
            Ended::ArmWaitRanOut,
        ),
        (End::Blind, Looks::Stopped, true, Ended::Blind),
        (End::Blind, Looks::Stopped, false, Ended::Blind),
    ];
    let mut lines: Vec<(Ended, String)> = Vec::new();
    for (end, looks, live, ended) in cases {
        let baseline = Some(BaselineRead {
            after: Duration::from_millis(12),
            windows: (end != End::Blind).then_some(1),
        });
        let left = std::cell::Cell::new(false);
        let woke = std::cell::Cell::new(false);
        let records = RefCell::new(Vec::new());
        finish_alert_watch(
            end,
            looks,
            super::Watched::Step(9),
            baseline,
            |answered| {
                left.set(answered && live);
                live
            },
            || woke.set(true),
            |report| records.borrow_mut().push(report),
        );
        let records = records.into_inner();
        assert_eq!(
            records,
            [super::AlertWatchReport {
                watched: super::Watched::Step(9),
                ended,
                baseline,
            }],
            "{end:?} after {looks:?} (live {live}): one record of its kind"
        );
        let answers = live && end == End::Answered;
        assert_eq!(
            left.get(),
            answers,
            "{end:?} (live {live}): the answer left"
        );
        assert_eq!(
            woke.get(),
            answers,
            "{end:?} (live {live}): the window woken"
        );
        let line = records[0].to_string();
        assert!(line.contains("revision 9"), "{line}");
        if lines.iter().all(|(kind, _)| *kind != ended) {
            lines.push((ended, line));
        }
    }
    assert_eq!(lines.len(), 8, "every kind of end is reached");
    for (i, (kind, line)) in lines.iter().enumerate() {
        for (other, other_line) in &lines[i + 1..] {
            assert_ne!(line, other_line, "{kind:?} and {other:?} log one line");
        }
    }
}

/// How long a test waits for an alert watch's worker to do what it must: a hang detector.
const WATCH_HANG: Duration = Duration::from_secs(60);

/// A pace for a scripted alert watch's worker whose looks come at once, both with the person
/// there and away, and whose wait for its arm outlasts [`WATCH_HANG`], so a worker the window
/// does not wake shows as a hang.
const AT_ONCE: super::AlertWatchPace = super::AlertWatchPace {
    look: Duration::ZERO,
    away: Duration::ZERO,
    first: Duration::ZERO,
    arm_wait: Duration::from_secs(600),
};

/// What a scripted alert watch's worker did.
#[derive(Debug, PartialEq)]
enum WatchEvent {
    /// It asked for a read of the window list; it waits for [`ScriptedWatch::answer`].
    Read,
    /// It woke the window.
    Woke,
    /// It ended, with this record.
    Logged(super::AlertWatchReport),
    /// A door's watch told the door's hold that this pass's question was answered.
    Answered(u64),
}

/// A live alert watch's worker ([`super::run_alert_watch`]) driven through a scripted window
/// list: each read waits for the test's answer, the list and whether the person is at the Mac.
struct ScriptedWatch {
    events: std::sync::mpsc::Receiver<WatchEvent>,
    lists: std::sync::mpsc::Sender<(Option<Vec<super::AlertWindow>>, bool)>,
}

impl ScriptedWatch {
    /// The worker's reach, wired to a new script, with `pace`.
    fn new(pace: super::AlertWatchPace) -> (super::AlertWatchIo, Self) {
        let (io, watch, _) = Self::wired(pace);
        (io, watch)
    }

    /// [`Self::new`], and what a door's watch tells the door's hold
    /// ([`super::start_door_alert_watch`]'s `answered`), which the script hears as
    /// [`WatchEvent::Answered`].
    fn for_door(
        pace: super::AlertWatchPace,
    ) -> (super::AlertWatchIo, Self, impl Fn(u64) + Send + 'static) {
        let (io, watch, sent) = Self::wired(pace);
        (io, watch, move |pass| {
            let _ = sent.send(WatchEvent::Answered(pass));
        })
    }

    /// [`Self::new`], and a sender of the script's events.
    fn wired(
        pace: super::AlertWatchPace,
    ) -> (
        super::AlertWatchIo,
        Self,
        std::sync::mpsc::Sender<WatchEvent>,
    ) {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (sent, events) = std::sync::mpsc::channel();
        let (lists, listed) = std::sync::mpsc::channel();
        let here = Arc::new(AtomicBool::new(true));
        let read_here = Arc::clone(&here);
        let read_sent = sent.clone();
        let wake_sent = sent.clone();
        let door_sent = sent.clone();
        let io = super::AlertWatchIo {
            read: Box::new(move || {
                let _ = read_sent.send(WatchEvent::Read);
                let (list, present) = listed.recv_timeout(WATCH_HANG).ok()?;
                read_here.store(present, Ordering::SeqCst);
                list
            }),
            present: Box::new(move || here.load(Ordering::SeqCst)),
            wake: Box::new(move || {
                let _ = wake_sent.send(WatchEvent::Woke);
            }),
            log: Box::new(move |report| {
                let _ = sent.send(WatchEvent::Logged(report));
            }),
            pace,
        };
        (io, Self { events, lists }, door_sent)
    }

    /// The worker's next act, within the hang detector.
    fn next(&self) -> WatchEvent {
        self.events
            .recv_timeout(WATCH_HANG)
            .expect("hang detector: the alert watch's worker did nothing within a minute")
    }

    /// Answer the read the worker asked for: `list`, with the person `present`.
    fn answer(&self, list: Option<Vec<super::AlertWindow>>, present: bool) {
        self.lists
            .send((list, present))
            .expect("the worker is still reading");
    }

    /// The worker's end, which must be its next act: its record.
    fn ended(&self) -> super::AlertWatchReport {
        match self.next() {
            WatchEvent::Logged(report) => report,
            other => panic!("the worker went on: {other:?}"),
        }
    }
}

/// Wait until a worker on `slot` has begun its `nth` wait on the window's moves: once that count
/// is read, a stop or an arm, which takes the slot's lock, finds the worker asleep, so only its
/// wake gets it going again. A hang detector, not a latency budget.
fn await_worker_asleep(slot: &super::AlertWatchSlot, nth: usize) {
    let since = Instant::now();
    while slot.waits() < nth {
        assert!(
            since.elapsed() < WATCH_HANG,
            "hang detector: the alert watch's worker never began wait {nth}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A live alert watch's first read, which its trigger waits for, marks the baseline in only once
/// it has come back, and wakes the window; a stop then ends its worker, which logs that it never
/// read again. NEGATIVE CONTROL: a worker that never marks the baseline in (`mark_baseline_in`'s
/// body taken out) leaves `baseline_in` false after its wake.
#[test]
fn a_live_alert_watchs_first_read_lets_its_trigger_go_and_wakes_the_window() {
    static SLOT: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, watch) = ScriptedWatch::new(AT_ONCE);
    assert!(super::start_alert_watch(
        &SLOT,
        super::Watched::Step(4),
        Some("27.0.1"),
        io
    ));
    assert_eq!(
        watch.next(),
        WatchEvent::Read,
        "its first read goes at once"
    );
    assert!(!SLOT.baseline_in(), "not in before that read comes back");
    watch.answer(Some(Vec::new()), true);
    assert_eq!(watch.next(), WatchEvent::Woke);
    assert!(SLOT.baseline_in(), "in once it came back");
    SLOT.cancel();
    let report = watch.ended();
    assert_eq!(
        (report.watched, report.ended),
        (
            super::Watched::Step(4),
            super::AlertWatchEnded::StoppedUnarmed
        )
    );
    assert!(!SLOT.baseline_in(), "a stop takes the baseline back");
}

/// Arming a live alert watch wakes its worker at once: it reads in the arm's turn, not when its
/// wait for the arm runs out. NEGATIVE CONTROL: an arm that does not wake the worker (its
/// `notify_all` taken out) leaves it asleep for its 600 s wait, past the hang detector.
#[test]
fn arming_a_live_alert_watch_wakes_its_worker_at_once() {
    static SLOT: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, watch) = ScriptedWatch::new(AT_ONCE);
    assert!(super::start_alert_watch(
        &SLOT,
        super::Watched::Step(5),
        Some("26.6.2"),
        io
    ));
    assert_eq!(watch.next(), WatchEvent::Read);
    watch.answer(Some(Vec::new()), true);
    assert_eq!(watch.next(), WatchEvent::Woke);
    await_worker_asleep(&SLOT, 1);
    SLOT.arm();
    assert_eq!(watch.next(), WatchEvent::Read, "the arm wakes it to read");
    SLOT.cancel();
    watch.answer(Some(Vec::new()), true);
    assert_eq!(watch.ended().ended, super::AlertWatchEnded::NeverSeen);
}

/// Stopping a live alert watch wakes its worker at once, whether it waits for its arm or for its
/// next look, and drops an answer it left that the window has not drained. NEGATIVE CONTROLS: a
/// stop that does not wake the worker (`cancel`'s `notify_all` taken out) leaves it asleep past
/// the hang detector; one that keeps the answer (`state.answer = None` taken out) lets the
/// window drain an answer for a hold that already ended.
#[test]
fn stopping_a_live_alert_watch_wakes_its_worker_and_drops_its_answer() {
    let slow = super::AlertWatchPace {
        look: Duration::from_secs(600),
        away: Duration::from_secs(600),
        first: Duration::ZERO,
        arm_wait: Duration::from_secs(600),
    };
    static FOR_ARM: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, watch) = ScriptedWatch::new(slow);
    assert!(super::start_alert_watch(
        &FOR_ARM,
        super::Watched::Step(6),
        Some("27.0"),
        io
    ));
    assert_eq!(watch.next(), WatchEvent::Read);
    watch.answer(Some(Vec::new()), true);
    assert_eq!(watch.next(), WatchEvent::Woke);
    await_worker_asleep(&FOR_ARM, 1);
    FOR_ARM.cancel();
    assert_eq!(
        watch.ended().ended,
        super::AlertWatchEnded::StoppedUnarmed,
        "a stop wakes a worker waiting for its arm"
    );

    static FOR_LOOK: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, watch) = ScriptedWatch::new(slow);
    assert!(super::start_alert_watch(
        &FOR_LOOK,
        super::Watched::Step(7),
        Some("27.0"),
        io
    ));
    assert_eq!(watch.next(), WatchEvent::Read);
    watch.answer(Some(Vec::new()), true);
    assert_eq!(watch.next(), WatchEvent::Woke);
    await_worker_asleep(&FOR_LOOK, 1);
    FOR_LOOK.arm();
    await_worker_asleep(&FOR_LOOK, 2);
    FOR_LOOK.cancel();
    assert_eq!(
        watch.ended().watched,
        super::Watched::Step(7),
        "a stop wakes a worker waiting out its look"
    );

    static ANSWERED: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, watch) = ScriptedWatch::new(AT_ONCE);
    assert!(super::start_alert_watch(
        &ANSWERED,
        super::Watched::Step(8),
        Some("27.0"),
        io
    ));
    assert_eq!(watch.next(), WatchEvent::Read);
    watch.answer(Some(Vec::new()), true);
    assert_eq!(watch.next(), WatchEvent::Woke);
    ANSWERED.arm();
    assert_eq!(watch.next(), WatchEvent::Read);
    watch.answer(Some(on(&[48])), true);
    assert_eq!(watch.next(), WatchEvent::Read);
    watch.answer(Some(Vec::new()), true);
    assert_eq!(
        watch.next(),
        WatchEvent::Woke,
        "its answer wakes the window"
    );
    assert_eq!(watch.ended().ended, super::AlertWatchEnded::Answered);
    assert_eq!(
        ANSWERED.state().answer,
        Some(super::Watched::Step(8)),
        "the answer is left"
    );
    ANSWERED.cancel();
    assert_eq!(ANSWERED.take_answer(), None, "a stop drops it");
}

/// A later start of the live alert watch begins from nothing: the last watch's first read,
/// arm and answer are gone before the new watch's first read is in. NEGATIVE CONTROL: a start
/// that keeps them (`*state = AlertWatchState { id, last_id: id, ..*state }`) leaves the last
/// watch's answer for the window to drain beside a hold whose own alert was never seen.
#[test]
fn a_later_start_of_the_live_alert_watch_resets_what_the_last_one_left() {
    static SLOT: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, first) = ScriptedWatch::new(AT_ONCE);
    assert!(super::start_alert_watch(
        &SLOT,
        super::Watched::Step(1),
        Some("27.0"),
        io
    ));
    assert_eq!(first.next(), WatchEvent::Read);
    first.answer(Some(Vec::new()), true);
    assert_eq!(first.next(), WatchEvent::Woke);
    SLOT.arm();
    assert_eq!(first.next(), WatchEvent::Read);
    first.answer(Some(on(&[48])), true);
    assert_eq!(first.next(), WatchEvent::Read);
    first.answer(Some(Vec::new()), true);
    assert_eq!(first.next(), WatchEvent::Woke);
    assert_eq!(first.ended().ended, super::AlertWatchEnded::Answered);

    let (io, second) = ScriptedWatch::new(AT_ONCE);
    assert!(super::start_alert_watch(
        &SLOT,
        super::Watched::Step(2),
        Some("27.0"),
        io
    ));
    assert_eq!(second.next(), WatchEvent::Read);
    assert_eq!(
        SLOT.state(),
        super::AlertWatchState {
            id: 2,
            last_id: 2,
            watched: Some(super::Watched::Step(2)),
            baseline_in: false,
            blind: false,
            armed: None,
            answer: None,
        },
        "nothing the first watch left stands for the second"
    );
    assert_eq!(SLOT.take_answer(), None);
    second.answer(Some(Vec::new()), true);
    assert_eq!(second.next(), WatchEvent::Woke);
    SLOT.cancel();
    assert_eq!(second.ended().ended, super::AlertWatchEnded::StoppedUnarmed);
}

/// A later start wakes the worker of the watch it replaces, asleep in its wait for the arm,
/// which then stops at once and logs that it never read again. NEGATIVE CONTROL: a start that
/// does not wake it (`begin`'s `notify_all` taken out) leaves it asleep for its 600 s wait,
/// past the hang detector.
#[test]
fn a_later_start_wakes_the_worker_of_the_watch_it_replaces() {
    static SLOT: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let slow = super::AlertWatchPace {
        look: Duration::from_secs(600),
        away: Duration::from_secs(600),
        first: Duration::ZERO,
        arm_wait: Duration::from_secs(600),
    };
    let (io, first) = ScriptedWatch::new(slow);
    assert!(super::start_alert_watch(
        &SLOT,
        super::Watched::Step(1),
        Some("27.0"),
        io
    ));
    assert_eq!(first.next(), WatchEvent::Read);
    first.answer(Some(Vec::new()), true);
    assert_eq!(first.next(), WatchEvent::Woke);
    await_worker_asleep(&SLOT, 1);
    let (io, second) = ScriptedWatch::new(slow);
    assert!(super::start_alert_watch(
        &SLOT,
        super::Watched::Step(2),
        Some("27.0"),
        io
    ));
    let report = first.ended();
    assert_eq!(
        (report.watched, report.ended),
        (
            super::Watched::Step(1),
            super::AlertWatchEnded::StoppedUnarmed
        ),
        "the replaced worker stops at the start"
    );
    assert_eq!(second.next(), WatchEvent::Read);
    second.answer(Some(Vec::new()), true);
    assert_eq!(second.next(), WatchEvent::Woke);
    SLOT.cancel();
    assert_eq!(second.ended().ended, super::AlertWatchEnded::StoppedUnarmed);
}

/// An answer a replaced watch's worker sees once a later start took the slot is never left: the
/// window drains nothing for the new watch, and the old watch logs its answer as one a stop
/// overtook. NEGATIVE CONTROL: a settle that leaves the answer whatever the live watch is
/// (`let live = state.id == id;` read as `let live = true;`) leaves revision 1's answer for
/// revision 2's hold.
#[test]
fn an_answer_a_replaced_live_alert_watch_saw_is_never_left_for_the_next() {
    static SLOT: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, first) = ScriptedWatch::new(AT_ONCE);
    assert!(super::start_alert_watch(
        &SLOT,
        super::Watched::Step(1),
        Some("27.0"),
        io
    ));
    assert_eq!(first.next(), WatchEvent::Read);
    first.answer(Some(Vec::new()), true);
    assert_eq!(first.next(), WatchEvent::Woke);
    SLOT.arm();
    assert_eq!(first.next(), WatchEvent::Read);
    first.answer(Some(on(&[48])), true);
    assert_eq!(first.next(), WatchEvent::Read, "the first worker reads on");

    let (io, second) = ScriptedWatch::new(AT_ONCE);
    assert!(super::start_alert_watch(
        &SLOT,
        super::Watched::Step(2),
        Some("27.0"),
        io
    ));
    assert_eq!(second.next(), WatchEvent::Read);
    first.answer(Some(Vec::new()), true);
    assert_eq!(
        first.ended().ended,
        super::AlertWatchEnded::AnsweredOnceStopped,
        "its answer came once a later start took the slot: no wake, nothing left"
    );
    assert_eq!(SLOT.take_answer(), None);
    second.answer(Some(Vec::new()), true);
    assert_eq!(second.next(), WatchEvent::Woke);
    SLOT.cancel();
    assert_eq!(second.ended().ended, super::AlertWatchEnded::StoppedUnarmed);
}

/// No live alert watch starts on a macOS whose alert's host was not measured, or whose version is
/// unknown: no worker reads, and the slot is untouched. NEGATIVE CONTROL: a start without its
/// `alert_host_measured` gate starts a worker, whose first read comes.
#[test]
fn no_live_alert_watch_starts_where_its_alerts_host_was_not_measured() {
    static SLOT: super::AlertWatchSlot = super::AlertWatchSlot::new();
    for version in [Some("28.0"), Some("15.7.1"), None] {
        let (io, watch) = ScriptedWatch::new(AT_ONCE);
        assert!(
            !super::start_alert_watch(&SLOT, super::Watched::Step(3), version, io),
            "{version:?}"
        );
        assert!(
            matches!(
                watch.events.recv_timeout(WATCH_HANG),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
            ),
            "{version:?}: no worker, its reach dropped unread"
        );
        assert_eq!(
            SLOT.state().last_id,
            0,
            "{version:?}: the slot is untouched"
        );
    }
}

/// Past its first looks (none here), the live worker hands the person's presence to its pace and
/// its end: after a read that found the person away it looks again after the away pace, after
/// one that found them there after the present pace, and a window gone while they are away ends
/// nothing until a read finds them back. Each pace here is either at once or past the hang
/// detector, so a worker that picks the wrong one hangs. NEGATIVE CONTROLS: a worker that looks
/// at the present pace whatever the person (`if here || …` read as `if true`) hangs after its
/// away first read; one at the away pace whatever the person (`if false`) hangs in the second
/// watch; a watch that ends whether or not the person is there (`&& here` taken out of
/// `watch_alert`) ends at the away read.
#[test]
fn a_live_alert_watch_paces_and_ends_by_the_persons_presence() {
    static AWAY: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, watch) = ScriptedWatch::new(super::AlertWatchPace {
        look: Duration::from_secs(600),
        away: Duration::ZERO,
        first: Duration::ZERO,
        arm_wait: Duration::from_secs(600),
    });
    assert!(super::start_alert_watch(
        &AWAY,
        super::Watched::Step(1),
        Some("27.0"),
        io
    ));
    assert_eq!(watch.next(), WatchEvent::Read);
    watch.answer(Some(Vec::new()), false);
    assert_eq!(watch.next(), WatchEvent::Woke);
    AWAY.arm();
    assert_eq!(
        watch.next(),
        WatchEvent::Read,
        "the away pace after an away read"
    );
    watch.answer(Some(on(&[48])), false);
    assert_eq!(watch.next(), WatchEvent::Read);
    watch.answer(Some(Vec::new()), false);
    assert_eq!(
        watch.next(),
        WatchEvent::Read,
        "no window left, with the person away: it reads on"
    );
    watch.answer(Some(Vec::new()), true);
    assert_eq!(
        watch.next(),
        WatchEvent::Woke,
        "the person back: it answers"
    );
    assert_eq!(watch.ended().ended, super::AlertWatchEnded::Answered);
    assert_eq!(AWAY.take_answer(), Some(super::Watched::Step(1)));
    AWAY.cancel();

    static HERE: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, watch) = ScriptedWatch::new(super::AlertWatchPace {
        look: Duration::ZERO,
        away: Duration::from_secs(600),
        first: Duration::ZERO,
        arm_wait: Duration::from_secs(600),
    });
    assert!(super::start_alert_watch(
        &HERE,
        super::Watched::Step(2),
        Some("27.0"),
        io
    ));
    assert_eq!(watch.next(), WatchEvent::Read);
    watch.answer(Some(Vec::new()), true);
    assert_eq!(watch.next(), WatchEvent::Woke);
    HERE.arm();
    assert_eq!(
        watch.next(),
        WatchEvent::Read,
        "the present pace after a present read"
    );
    watch.answer(Some(on(&[48])), true);
    assert_eq!(watch.next(), WatchEvent::Read);
    watch.answer(Some(Vec::new()), true);
    assert_eq!(watch.next(), WatchEvent::Woke);
    assert_eq!(watch.ended().ended, super::AlertWatchEnded::Answered);
    HERE.cancel();
}

/// For its first looks after the arm the live worker reads at the present pace whatever the
/// person, so an alert drawn after the trigger is seen though the person, admitted to the step's
/// moment by a touch minutes before, reads as away; the alert's going still ends nothing until a
/// read finds them there. The away pace is past the hang detector and the first looks outlast
/// it, so a worker that paces its first looks by the person hangs. NEGATIVE CONTROL: a worker
/// whose pace leaves out its first looks (`|| armed.elapsed() < pace.first` taken out of
/// `run_alert_watch`) waits out the away pace after its away first read and hangs.
#[test]
fn a_live_alert_watch_reads_at_the_present_pace_for_its_first_looks_whatever_the_person() {
    static SLOT: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, watch) = ScriptedWatch::new(super::AlertWatchPace {
        look: Duration::ZERO,
        away: Duration::from_secs(600),
        first: Duration::from_secs(600),
        arm_wait: Duration::from_secs(600),
    });
    assert!(super::start_alert_watch(
        &SLOT,
        super::Watched::Step(3),
        Some("27.0.1"),
        io
    ));
    assert_eq!(watch.next(), WatchEvent::Read);
    watch.answer(Some(Vec::new()), false);
    assert_eq!(watch.next(), WatchEvent::Woke);
    SLOT.arm();
    assert_eq!(
        watch.next(),
        WatchEvent::Read,
        "the present pace after the arm, the person away"
    );
    watch.answer(Some(on(&[48])), false);
    assert_eq!(
        watch.next(),
        WatchEvent::Read,
        "the present pace after a read that saw the alert, the person away"
    );
    watch.answer(Some(Vec::new()), false);
    assert_eq!(
        watch.next(),
        WatchEvent::Read,
        "no window left, with the person away: it reads on"
    );
    watch.answer(Some(Vec::new()), true);
    assert_eq!(
        watch.next(),
        WatchEvent::Woke,
        "the person there: it answers"
    );
    assert_eq!(watch.ended().ended, super::AlertWatchEnded::Answered);
    assert_eq!(
        SLOT.take_answer().and_then(super::Watched::revision),
        Some(3),
        "the step's drain reads its revision"
    );
    SLOT.cancel();
}

/// The door's alert watch ([`super::start_door_alert_watch`]): its first read, which the door's
/// use waits for before it asks, is in when the start returns, and the watch, armed then, reads
/// on with no wake of the window for that read. Once it saw a new window of the alert's owner
/// come and then, with the person at the Mac, no window of that owner left, it tells the door's
/// hold of its own pass, which ends it, and only then wakes the window, so the window's turn
/// finds the dialog slot free; nothing is left for the step's drain, and its record is the
/// door's. NEGATIVE CONTROLS: a start that does not arm the watch (`slot.arm_of(watched)` taken
/// out of `start_door_alert_watch`) leaves its worker waiting for an arm past the hang detector;
/// a wake that does not tell the hold (`answered(pass)` taken out) wakes the window with the
/// hold still standing.
#[test]
fn the_doors_alert_watch_ends_its_hold_once_the_question_is_answered() {
    use super::{AlertWatchEnded, Watched};
    static SLOT: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, watch, answered) = ScriptedWatch::for_door(AT_ONCE);
    // The first read's list is there before the use's thread waits for it.
    watch.answer(Some(on(&[17])), true);
    assert!(super::start_door_alert_watch(
        &SLOT,
        3,
        Some("27.0.1"),
        io,
        answered,
        WATCH_HANG
    ));
    assert_eq!(watch.next(), WatchEvent::Read, "its first read");
    assert!(SLOT.baseline_in(), "in when the start returned");
    assert!(
        SLOT.state().armed.is_some(),
        "and armed, so the use asks into a watch that reads on"
    );
    assert_eq!(
        watch.next(),
        WatchEvent::Read,
        "it reads on, with no wake for its first read"
    );
    watch.answer(Some(on(&[17, 48])), true);
    assert_eq!(watch.next(), WatchEvent::Read);
    watch.answer(Some(Vec::new()), true);
    assert_eq!(
        watch.next(),
        WatchEvent::Answered(3),
        "the door's hold of its pass is told"
    );
    assert_eq!(watch.next(), WatchEvent::Woke, "then the window wakes");
    let report = watch.ended();
    assert_eq!(
        (report.watched, report.ended),
        (Watched::Door(3), AlertWatchEnded::Answered)
    );
    assert!(report.to_string().contains("door pass 3"), "{report}");
    assert_eq!(
        SLOT.take_answer(),
        None,
        "nothing left for the window's drain"
    );
    SLOT.cancel_of(Watched::Door(3));
}

/// A door's watch whose first read is not back within its wait is stopped and gives `false`, so
/// its use asks unwatched and the door's hold runs its bound; its worker, once that read comes
/// back, finds itself stopped, reads nothing more and records that it was stopped before it read
/// again, as the door's. One whose first read could not read the window list gives `false` too:
/// its worker ended at that read, recording it blind, so nothing reads on and the watch is not
/// left armed. On a macOS whose alert host was not measured no worker starts at all.
/// NEGATIVE CONTROLS: a start that keeps a late watch (`slot.cancel_of(watched)` taken out of
/// `start_door_alert_watch`) leaves its worker waiting for an arm past the hang detector; one
/// that gives `true` whatever came back reads the late watch as watching; a worker that marks
/// every first read as read (`listed.is_some()` replaced by `true` in `run_alert_watch`) reads
/// the blind watch as watching.
#[test]
fn a_doors_alert_watch_whose_first_read_is_late_lets_its_use_ask_unwatched() {
    use super::{AlertWatchEnded, Watched};
    static SLOT: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, watch, answered) = ScriptedWatch::for_door(AT_ONCE);
    assert!(
        !super::start_door_alert_watch(&SLOT, 4, Some("27.0"), io, answered, Duration::ZERO),
        "no first read back: unwatched"
    );
    assert_eq!(watch.next(), WatchEvent::Read, "its first read went");
    assert!(!SLOT.baseline_in());
    assert_eq!(SLOT.state().armed, None, "never armed");
    watch.answer(Some(Vec::new()), true);
    let report = watch.ended();
    assert_eq!(
        (report.watched, report.ended),
        (Watched::Door(4), AlertWatchEnded::StoppedUnarmed)
    );

    static BLIND: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, watch, answered) = ScriptedWatch::for_door(AT_ONCE);
    // The first read's answer is there before the use's thread waits for it: no list.
    watch.answer(None, true);
    assert!(
        !super::start_door_alert_watch(&BLIND, 6, Some("27.0"), io, answered, WATCH_HANG),
        "the window list unreadable at the first read: unwatched"
    );
    assert_eq!(watch.next(), WatchEvent::Read, "its first read went");
    let report = watch.ended();
    assert_eq!(
        (report.watched, report.ended),
        (Watched::Door(6), AlertWatchEnded::Blind)
    );
    assert_eq!(
        (BLIND.state().id, BLIND.state().armed),
        (0, None),
        "stopped, never armed"
    );

    static UNMEASURED: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, watch, answered) = ScriptedWatch::for_door(AT_ONCE);
    assert!(!super::start_door_alert_watch(
        &UNMEASURED,
        5,
        Some("28.0"),
        io,
        answered,
        WATCH_HANG
    ));
    assert!(
        matches!(
            watch.events.recv_timeout(WATCH_HANG),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ),
        "no worker, its reach dropped unread"
    );
    assert_eq!(UNMEASURED.state().last_id, 0, "the slot is untouched");
}

/// The door's hold arms and stops only its own pass's watch: an arm or a stop for another pass
/// leaves the watch as it was, so a hold's late stop never stops a later pass's watch.
/// NEGATIVE CONTROLS: a `cancel_of` that stops whichever watch is live (its `watched` check taken
/// out of `cancel_where`) stops the watch at the other pass's stop; an `arm_of` that arms
/// whichever (its check taken out of `arm_where`) arms it for another pass.
#[test]
fn the_doors_hold_arms_and_stops_only_its_own_passs_watch() {
    use super::{AlertWatchEnded, Watched};
    static SLOT: super::AlertWatchSlot = super::AlertWatchSlot::new();
    let (io, watch) = ScriptedWatch::new(AT_ONCE);
    assert!(super::start_alert_watch(
        &SLOT,
        Watched::Door(6),
        Some("27.0"),
        io
    ));
    assert_eq!(watch.next(), WatchEvent::Read);
    watch.answer(Some(Vec::new()), true);
    assert_eq!(watch.next(), WatchEvent::Woke);
    SLOT.arm_of(Watched::Door(7));
    assert_eq!(SLOT.state().armed, None, "another pass's arm arms nothing");
    SLOT.arm_of(Watched::Door(6));
    assert!(SLOT.state().armed.is_some());
    assert_eq!(watch.next(), WatchEvent::Read);
    SLOT.cancel_of(Watched::Door(7));
    watch.answer(Some(Vec::new()), true);
    assert_eq!(
        watch.next(),
        WatchEvent::Read,
        "another pass's stop: it reads on"
    );
    SLOT.cancel_of(Watched::Door(6));
    watch.answer(Some(Vec::new()), true);
    assert_eq!(watch.ended().ended, AlertWatchEnded::NeverSeen);
}
