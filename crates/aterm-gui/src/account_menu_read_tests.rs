// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::footer::FooterFacts;

use crate::fact_menu::SessionWeek;

#[cfg(unix)]
fn saved_next_for_projection(store: &accounts::Store, _nonce: u64) -> accounts::NextRequest {
    store.request_next(100).unwrap()
}

// An unsupported host can still read historical selections. Seed that exact wire through
// the real decoder and transaction, without claiming fresh manual admission is supported.
#[cfg(not(unix))]
fn saved_next_for_projection(store: &accounts::Store, nonce: u64) -> accounts::NextRequest {
    let mut wire: aterm_json::Value =
        aterm_json::from_str(&store.load(100).unwrap().0.to_json()).unwrap();
    wire.as_object_mut().unwrap().insert(
        "pending_pin".into(),
        aterm_json::json!({"kind": "next", "nonce": format!("{nonce:032x}")}),
    );
    let selection = accounts::State::from_json(&aterm_json::to_string(&wire).unwrap(), 100)
        .unwrap()
        .pending_pin
        .unwrap();
    let request = selection.next().unwrap().clone();
    store
        .transact(100, &mut None, |state| {
            state.pending_pin = Some(selection.clone());
            Ok(Vec::new())
        })
        .unwrap();
    request
}

#[test]
fn queue_observation_requires_the_current_request_namespace_and_row() {
    use aterm_agent::harness::accounts_sync::{
        LoginActivity, LoginActivityBatch, LoginIdentity, QueuedSignin,
    };
    let root = aterm_tempfile::tempdir().unwrap();
    let store = accounts::Store::at(root.path());
    let request = store.request_pin("work", 100).unwrap();
    let state = store.load(100).unwrap().0;
    let row = accounts::Row {
        label: "work".into(),
        ..Default::default()
    };
    let roster = accounts::Roster {
        rows: vec![row.clone()],
    };
    let queued = QueuedSignin::for_test(
        request.into(),
        accounts::SigninPosition {
            ahead: 0,
            blocker: accounts::SigninBlocker::UnscheduledSignin,
        },
    );
    let entry = LoginActivity {
        label: "work".into(),
        route: None,
        active: false,
        observed_at: 100,
        next_at: None,
        diagnostic: None,
        identity: Some(LoginIdentity::for_test(&row)),
        queued: Some(queued),
    };
    let batch = LoginActivityBatch::for_test(
        [Some((&store, vec![entry.clone()])), None],
        accounts::Cli::Claude,
    );
    let waiting =
        queue_status_from(&store, accounts::Cli::Claude, &roster, &state, &batch).unwrap();
    assert!(waiting.contains("0 ahead"));
    let mut changed = roster.clone();
    changed.rows[0].email = Some("different@example.test".into());
    assert!(queue_status_from(&store, accounts::Cli::Claude, &changed, &state, &batch).is_none());
    assert!(queue_status_from(&store, accounts::Cli::Codex, &roster, &state, &batch).is_none());
    let mut acquired = entry;
    acquired.queued = None;
    acquired.active = true;
    let acquired = LoginActivityBatch::for_test(
        [Some((&store, vec![acquired])), None],
        accounts::Cli::Claude,
    );
    assert!(
        queue_status_from(&store, accounts::Cli::Claude, &roster, &state, &acquired).is_none(),
        "an acquired worker has no waiting place"
    );
    let unobserved =
        LoginActivityBatch::for_test([Some((&store, Vec::new())), None], accounts::Cli::Claude);
    assert!(
        queue_status_from(&store, accounts::Cli::Claude, &roster, &state, &unobserved).is_none()
    );
    let _newer = store.request_pin("work", 100).unwrap();
    let replaced = store.load(100).unwrap().0;
    assert!(queue_status_from(&store, accounts::Cli::Claude, &roster, &replaced, &batch).is_none());
}

#[test]
fn next_queue_observation_never_survives_a_newer_selection() {
    use aterm_agent::harness::accounts_sync::{LoginActivityBatch, QueuedSignin};
    let root = aterm_tempfile::tempdir().unwrap();
    let store = accounts::Store::at(root.path());
    let request = saved_next_for_projection(&store, 1);
    let state = store.load(100).unwrap().0;
    let queued = QueuedSignin::for_test(
        accounts::PendingSelection::Next(request),
        accounts::SigninPosition {
            ahead: 2,
            blocker: accounts::SigninBlocker::ManualAhead,
        },
    );
    let batch =
        LoginActivityBatch::for_test([Some((&store, Vec::new())), None], accounts::Cli::Claude)
            .with_queued_next_for_test(&store, accounts::Cli::Claude, queued);
    let roster = accounts::Roster { rows: Vec::new() };
    let waiting =
        queue_status_from(&store, accounts::Cli::Claude, &roster, &state, &batch).unwrap();
    assert!(waiting.contains("2 ahead"));
    let other = accounts::Store::at(&root.path().join("other"));
    assert!(queue_status_from(&other, accounts::Cli::Claude, &roster, &state, &batch).is_none());
    let _newer = saved_next_for_projection(&store, 2);
    assert!(
        queue_status_from(
            &store,
            accounts::Cli::Claude,
            &roster,
            &store.load(100).unwrap().0,
            &batch
        )
        .is_none()
    );
    let _account = store.request_pin("work", 100).unwrap();
    assert!(
        queue_status_from(
            &store,
            accounts::Cli::Claude,
            &roster,
            &store.load(100).unwrap().0,
            &batch
        )
        .is_none()
    );
}

/// A worker's answer of an empty pool, pinned or not, with the queue place it observed.
fn read(unpinned: bool, queue: Option<&str>) -> AccountRead {
    let mut read = AccountRead::for_test(Ok(Vec::new()));
    if let Ok(pool) = read.pool.as_mut() {
        pool.pinned = (!unpinned).then(String::new);
        pool.queue = queue.map(str::to_owned);
    }
    read
}

fn rig() -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    app.account_menu_read.hold_for_test = true;
    let wid = *app.windows.keys().next().unwrap();
    let session = app.focused_session_id(wid).unwrap();
    publish(&app, session, 1, "first", aterm_phase::Program::Claude);
    (app, wid, session)
}

fn publish(app: &App, session: u64, birth: u64, conversation: &str, program: aterm_phase::Program) {
    let entry = app.pool.get(session).unwrap();
    let mut timeline = entry.ctx.timeline.lock().unwrap();
    timeline.note_foreground_group(4242);
    timeline.set_agent_reader_program_for_test(Some(program));
    timeline.set_claude_footer(
        4242,
        Some(birth),
        Some(FooterFacts {
            owner: Some(FactsOwner {
                floor: Some(birth),
                session_id: conversation.into(),
            }),
            account: Some("fixture-account".into()),
            ..Default::default()
        }),
    );
}

fn open(app: &mut App, wid: WindowId, session: u64) -> u64 {
    app.open_fact_menu(wid, session, Fact::Account, (0, 0), true);
    app.account_menu_read.in_flight.unwrap()
}

#[test]
fn opening_and_repainting_coalesce_until_a_controlled_completion() {
    let (mut app, wid, session) = rig();
    let generation = open(&mut app, wid, session);
    assert!(app.account_menu_rows_cached(session).is_none());
    for _ in 0..8 {
        app.refresh_fact_menu(session);
    }
    assert_eq!(app.account_menu_read.in_flight, Some(generation));
    assert!(app.account_menu_read.pending.is_none());
    app.on_account_menu_read(Completion {
        generation,
        rows: AccountRead::for_test(Err("fixture refusal".into())),
    });
    assert!(matches!(
        app.account_menu_rows_cached(session),
        Some(AccountRead { pool: Err(_), .. })
    ));
    assert!(app.account_menu_read.in_flight.is_none());
    assert!(app.account_menu_read_deadline().is_some());
    app.close_fact_menu();
    assert!(app.account_menu_read_deadline().is_none());
}

#[test]
fn a_saved_pick_or_next_discards_ready_and_inflight_queue_notes() {
    for next in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let store = accounts::Store::at(root.path());
        let _old = store.request_pin("work", 100).unwrap();
        let before = store.load(100).unwrap().0.pending_pin;
        let (mut app, wid, session) = rig();
        let first = open(&mut app, wid, session);
        app.on_account_menu_read(Completion {
            generation: first,
            rows: read(true, Some("old queue observation")),
        });
        assert!(matches!(
            app.account_menu_rows_cached(session),
            Some(AccountRead {
                pool: Ok(PoolRows { queue: Some(_), .. }),
                ..
            })
        ));
        let due = app.account_menu_read_deadline().unwrap();
        let input = app.account_menu_input(session).unwrap();
        app.account_menu_read.request(wid, input, due);
        app.dispatch_account_menu_read();
        let old_read = app.account_menu_read.in_flight.unwrap();
        if next {
            let _ = saved_next_for_projection(&store, 3);
        } else {
            let _ = store.request_pin("work", 100).unwrap();
        }
        assert_ne!(store.load(100).unwrap().0.pending_pin, before);
        app.account_menu_selection_changed();
        assert!(app.account_menu_rows_cached(session).is_none());
        app.refresh_fact_menu(session);
        let replacement = app.account_menu_read.pending.as_ref().unwrap().0;
        for _ in 0..4 {
            app.refresh_fact_menu(session);
        }
        assert_eq!(
            app.account_menu_read.pending.as_ref().unwrap().0,
            replacement
        );
        assert_eq!(app.account_menu_read.in_flight, Some(old_read));
        app.on_account_menu_read(Completion {
            generation: old_read,
            rows: read(true, Some("superseded observation")),
        });
        assert!(app.account_menu_rows_cached(session).is_none());
        assert_eq!(app.account_menu_read.in_flight, Some(replacement));
        app.on_account_menu_read(Completion {
            generation: replacement,
            rows: read(true, None),
        });
        assert!(matches!(
            app.account_menu_rows_cached(session),
            Some(AccountRead {
                pool: Ok(PoolRows { queue: None, .. }),
                ..
            })
        ));
    }
}

/// A read that cannot start is answered on the window's thread, so it reads nothing: no pool
/// file, no claims table, no series, and it carries no card. CONTROL: a worker's own read
/// books its read of the pool file and the claims table.
#[test]
fn an_immediate_start_failure_is_visible_after_opening_the_menu() {
    let scratch = scratch();
    let _ = crate::work_counts::take_account_machines_reads();
    let _ = read_account_at(&scratch.key, &Mutex::default(), NOON);
    assert_eq!(
        crate::work_counts::take_account_machines_reads(),
        1,
        "CONTROL: the count sees a read"
    );
    let _ = crate::work_counts::take_account_history_folds();
    let (mut app, wid, session) = rig();
    app.account_menu_read.hold_for_test = false;
    app.open_fact_menu(wid, session, Fact::Account, (0, 0), true);
    assert!(matches!(
        app.account_menu_rows_cached(session),
        Some(AccountRead {
            pool: Err(_),
            card: None
        })
    ));
    assert_eq!(crate::work_counts::take_account_machines_reads(), 0);
    assert_eq!(crate::work_counts::take_account_history_folds(), 0);
    assert!(app.account_menu_read.in_flight.is_none());
    assert!(app.account_menu_read_deadline().is_some());
    assert!(app.autopilot_menu.is_some());
}

#[test]
fn a_closed_window_cannot_receive_a_completed_account_read() {
    let (mut app, wid, session) = rig();
    let generation = open(&mut app, wid, session);
    app.windows.remove(&wid);
    app.on_account_menu_read(Completion {
        generation,
        rows: read(true, None),
    });
    assert!(app.account_menu_read.active.is_none());
    assert!(app.account_menu_read.in_flight.is_none());
    assert!(app.account_menu_read_deadline().is_none());
}

#[test]
fn closing_reopening_and_retargeting_reject_old_completions() {
    let (mut app, wid, session) = rig();
    let old = open(&mut app, wid, session);
    app.close_fact_menu();
    open(&mut app, wid, session);
    let new = app.account_menu_read.active.as_ref().unwrap().generation;
    assert_ne!(old, new);
    app.on_account_menu_read(Completion {
        generation: old,
        rows: read(true, None),
    });
    assert!(app.account_menu_rows_cached(session).is_none());
    assert_eq!(app.account_menu_read.in_flight, Some(new));
    app.open_fact_menu(wid, session, Fact::Model, (0, 0), true);
    app.on_account_menu_read(Completion {
        generation: new,
        rows: read(true, None),
    });
    assert_eq!(
        app.autopilot_menu.as_ref().unwrap().owner,
        MenuOwner::Fact(Fact::Model)
    );
    assert!(app.account_menu_read.active.is_none());
}

#[test]
fn changed_process_conversation_and_provider_invalidate_pending_rows() {
    let (mut app, wid, session) = rig();
    for (birth, conversation, program) in [
        (2, "first", aterm_phase::Program::Claude),
        (2, "second", aterm_phase::Program::Claude),
        (2, "second", aterm_phase::Program::Codex),
    ] {
        let old = open(&mut app, wid, session);
        publish(&app, session, birth, conversation, program);
        app.on_account_menu_read(Completion {
            generation: old,
            rows: read(true, None),
        });
        assert!(app.account_menu_rows_cached(session).is_none());
        let current = app.account_menu_read.in_flight.unwrap();
        assert_ne!(current, old);
        app.on_account_menu_read(Completion {
            generation: current,
            rows: read(false, None),
        });
        assert!(app.autopilot_menu.as_ref().unwrap().highlight.is_none());
        assert!(matches!(
            app.account_menu_rows_cached(session),
            Some(AccountRead {
                pool: Ok(PoolRows {
                    pinned: Some(_),
                    ..
                }),
                ..
            })
        ));
    }
}

#[test]
fn unchanged_owner_gets_periodic_refresh_without_discarding_usable_rows() {
    let (mut app, wid, session) = rig();
    let generation = open(&mut app, wid, session);
    app.on_account_menu_read(Completion {
        generation,
        rows: read(true, None),
    });
    let at = app
        .account_menu_read
        .active
        .as_ref()
        .unwrap()
        .read_at
        .unwrap();
    let input = app.account_menu_input(session).unwrap();
    app.account_menu_read
        .request(wid, input.clone(), at + Duration::from_secs(1));
    assert!(app.account_menu_read.pending.is_none());
    app.account_menu_read
        .request(wid, input, at + Duration::from_secs(2));
    assert!(app.account_menu_read.pending.is_some());
    assert!(app.account_menu_rows_cached(session).is_some());
}

#[test]
fn account_identity_change_hides_old_choices_before_the_new_read_completes() {
    let (mut app, wid, session) = rig();
    let old = open(&mut app, wid, session);
    app.on_account_menu_read(Completion {
        generation: old,
        rows: read(true, None),
    });
    {
        let entry = app.pool.get(session).unwrap();
        let mut timeline = entry.ctx.timeline.lock().unwrap();
        let mut facts = timeline.claude_footer().unwrap().clone();
        facts.account = Some("different-account".into());
        timeline.set_claude_footer(4242, Some(1), Some(facts));
    }
    assert!(app.account_menu_rows_cached(session).is_none());
    app.refresh_fact_menu(session);
    assert!(app.account_menu_read.in_flight.is_some());
    assert!(
        app.account_menu_read
            .active
            .as_ref()
            .unwrap()
            .rows
            .is_none()
    );
    assert!(app.account_menu_read_deadline().is_none());
}

/// A NEW WEEK READING KEEPS THE CARD. The session's own week figure moves with every reply; it
/// is part of what a read is fenced by, so a new read is asked, but the last read of the same
/// tab on the same account stays shown, and a highlight on one of its items stays, until the
/// new one lands. Negative control: another account hides the old read at once
/// (`account_identity_change_hides_old_choices_before_the_new_read_completes`).
#[test]
fn a_new_week_reading_keeps_the_last_read_until_the_next_lands() {
    let (mut app, wid, session) = rig();
    let week = |app: &App, used_x10: u32, at: i64| {
        let entry = app.pool.get(session).unwrap();
        let mut timeline = entry.ctx.timeline.lock().unwrap();
        let mut facts = timeline.claude_footer().unwrap().clone();
        facts.subscription = Some(aterm_agent::harness::profile::SubscriptionDisplay {
            email: Some("work@example.com".into()),
            weekly: Some("week".into()),
            weekly_figure: Some(aterm_agent::harness::profile::WeeklyFigure {
                used_x10,
                resets_at: at + 86_400,
                at,
            }),
            ..Default::default()
        });
        timeline.set_claude_footer(4242, Some(1), Some(facts));
    };
    let now = unix_now().unwrap();
    week(&app, 300, now - 30);
    let first = open(&mut app, wid, session);
    app.on_account_menu_read(Completion {
        generation: first,
        rows: read(true, Some("the first read")),
    });
    let before = app.account_menu_input(session).unwrap().key;
    let menu = app.autopilot_menu.as_mut().unwrap();
    let chosen = crate::autopilot_menu::step(&menu.rows, None, false);
    assert!(chosen.is_some());
    menu.highlight = chosen;
    let shown = menu.rows.clone();
    week(&app, 310, now - 5);
    let after = app.account_menu_input(session).unwrap().key;
    assert_ne!(before, after, "the reading is part of the fence");
    assert!(before.same_but_week(&after));
    app.refresh_fact_menu(session);
    let second = app.account_menu_read.in_flight.unwrap();
    assert_ne!(second, first, "a new read is asked");
    assert!(matches!(
        app.account_menu_rows_cached(session),
        Some(AccountRead {
            pool: Ok(PoolRows { queue: Some(_), .. }),
            ..
        })
    ));
    let menu = app.autopilot_menu.as_ref().unwrap();
    assert_eq!(menu.highlight, chosen, "the highlight holds its item");
    assert_eq!(menu.rows.len(), shown.len(), "no row left the card");
    app.on_account_menu_read(Completion {
        generation: second,
        rows: read(true, None),
    });
    assert!(matches!(
        app.account_menu_rows_cached(session),
        Some(AccountRead {
            pool: Ok(PoolRows { queue: None, .. }),
            ..
        })
    ));
}

#[test]
fn reader_panic_completes_the_pending_slot_and_occlusion_disarms_polling() {
    let (mut app, wid, session) = rig();
    let generation = open(&mut app, wid, session);
    app.on_account_menu_read(complete_read(generation, || {
        panic!("synthetic reader failure")
    }));
    assert!(matches!(
        app.account_menu_rows_cached(session),
        Some(AccountRead { pool: Err(_), .. })
    ));
    assert!(app.account_menu_read.in_flight.is_none());
    assert!(app.account_menu_read_deadline().is_some());
    app.windows.get_mut(&wid).unwrap().occluded = true;
    assert!(app.account_menu_read_deadline().is_none());
    app.windows.get_mut(&wid).unwrap().occluded = false;
    assert!(app.account_menu_read_deadline().is_some());
}

#[test]
fn first_result_selects_for_keyboard_open_only_and_later_results_preserve_choice() {
    for keyboard in [false, true] {
        let (mut app, wid, session) = rig();
        app.open_fact_menu(wid, session, Fact::Account, (0, 0), keyboard);
        let generation = app.account_menu_read.in_flight.unwrap();
        assert!(app.autopilot_menu.as_ref().unwrap().highlight.is_none());
        app.on_account_menu_read(Completion {
            generation,
            rows: read(true, None),
        });
        let menu = app.autopilot_menu.as_mut().unwrap();
        let first = crate::autopilot_menu::step(&menu.rows, None, true);
        assert!(first.is_some());
        assert_eq!(menu.highlight, keyboard.then_some(first).flatten());
        assert!(!app.account_menu_read.deferred_keyboard_selection);
        let last = crate::autopilot_menu::step(&menu.rows, None, false);
        menu.highlight = last;
        let at = app
            .account_menu_read
            .active
            .as_ref()
            .unwrap()
            .read_at
            .unwrap();
        let input = app.account_menu_input(session).unwrap();
        app.account_menu_read
            .request(wid, input, at + Duration::from_secs(2));
        app.dispatch_account_menu_read();
        let generation = app.account_menu_read.in_flight.unwrap();
        app.on_account_menu_read(Completion {
            generation,
            rows: read(true, None),
        });
        assert_eq!(app.autopilot_menu.as_ref().unwrap().highlight, last);
    }
}

#[test]
fn pointer_or_navigation_control_cancels_deferred_keyboard_selection() {
    for pointer in [false, true] {
        let (mut app, wid, session) = rig();
        let generation = open(&mut app, wid, session);
        if pointer {
            app.autopilot_menu_motion(wid, 10.0, 20.0);
        } else {
            app.autopilot_menu_nav(wid, crate::autopilot_menu::MenuKey::Down);
        }
        app.on_account_menu_read(Completion {
            generation,
            rows: read(true, None),
        });
        // The read does not take the selection: it stays where the person left it (nowhere
        // under a pointer, on the item their key reached).
        let menu = app.autopilot_menu.as_ref().unwrap();
        assert_ne!(
            menu.highlight,
            crate::autopilot_menu::step(&menu.rows, None, true)
        );
        assert_eq!(menu.highlight.is_none(), pointer);
        assert!(!app.account_menu_read.deferred_keyboard_selection);
    }
}

#[test]
fn inert_key_and_stationary_pointer_do_not_take_keyboard_control() {
    let (mut app, wid, session) = rig();
    let generation = open(&mut app, wid, session);
    app.autopilot_menu_nav(wid, crate::autopilot_menu::MenuKey::Inert);
    app.autopilot_menu.as_mut().unwrap().rest =
        crate::autopilot_menu::PointerRest::at_open(Some((10.0, 20.0)));
    app.autopilot_menu_motion(wid, 10.0, 20.0);
    app.on_account_menu_read(Completion {
        generation,
        rows: read(true, None),
    });
    assert!(app.autopilot_menu.as_ref().unwrap().highlight.is_some());
}

/// THE TABS ON THE ACCOUNT ARE COUNTED BY WHAT NAMES IT. Another pooled CLI's tabs all run on
/// its pool's active account, so each tab of that CLI counts; Claude Code tabs count by the
/// address their footers name, else by the roster label; and a Claude Code tab whose footer
/// names neither claims no count. Negative control: a tab of another program, and one on
/// another address, are not counted.
#[test]
fn the_tabs_on_an_account_are_counted_by_what_names_it() {
    let (mut app, wid, first) = rig();
    let mut tab = || {
        let id = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(id));
        id
    };
    let (second, third) = (tab(), tab());
    let set = |app: &App,
               session: u64,
               program: aterm_phase::Program,
               account: Option<&str>,
               email: Option<&str>| {
        let entry = app.pool.get(session).unwrap();
        let mut timeline = entry.ctx.timeline.lock().unwrap();
        timeline.note_foreground_group(4242);
        timeline.set_agent_reader_program_for_test(Some(program));
        timeline.set_claude_footer(
            4242,
            Some(1),
            Some(FooterFacts {
                account: account.map(str::to_owned),
                subscription: email.map(|email| {
                    aterm_agent::harness::profile::SubscriptionDisplay {
                        email: Some(email.to_owned()),
                        ..Default::default()
                    }
                }),
                ..Default::default()
            }),
        );
    };
    let tabs = |app: &App| app.fact_menu_facts(first, Fact::Account).tabs_here;
    use aterm_phase::Program::{Claude, Codex};
    // Three Codex tabs, no footer naming a login: all three run on the pool's active account.
    for session in [first, second, third] {
        set(&app, session, Codex, None, None);
    }
    assert_eq!(tabs(&app), Some(3));
    set(&app, third, Claude, None, None);
    assert_eq!(tabs(&app), Some(2), "CONTROL: another program's tab");
    // Claude Code by address.
    for session in [first, second, third] {
        set(&app, session, Claude, None, Some("work@example.com"));
    }
    assert_eq!(tabs(&app), Some(3));
    set(&app, third, Claude, None, Some("other@example.com"));
    assert_eq!(tabs(&app), Some(2), "CONTROL: another address");
    // Claude Code with no address yet, by roster label.
    set(&app, first, Claude, Some("work"), None);
    set(&app, second, Claude, Some("work"), None);
    set(&app, third, Claude, Some("spare"), None);
    assert_eq!(tabs(&app), Some(2));
    // Nothing names this tab's account: no count is claimed.
    set(&app, first, Claude, None, None);
    assert_eq!(tabs(&app), None);
}

/// A scratch Claude Code pool of one row `work`, and the key of a session on it.
struct Scratch {
    _dir: aterm_tempfile::TempDir,
    key: Key,
}

fn scratch() -> Scratch {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-card-read-")
        .tempdir()
        .unwrap();
    let root = dir.path();
    let (home, config, state) = (root.join("home"), root.join("config"), root.join("state"));
    for d in [&home, &config, &state] {
        std::fs::create_dir_all(d).unwrap();
    }
    let aterm_toml = config.join("aterm.toml");
    crate::test_scratch::write_config(&aterm_toml, "[fleet]\nmachine = \"m16\"\n");
    std::fs::write(
        config.join("accounts.toml"),
        "[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\nemail = \"work@example.com\"\n",
    )
    .unwrap();
    // The store's machine id names this Mac's series files: a store nothing has written yet
    // mints one per load, so the fixture keeps one as the first writer would.
    accounts::Store::at(&state).machine_id("").unwrap();
    let key = Key {
        session: 1,
        timeline: 0,
        foreground: None,
        pgid: 0,
        birth: None,
        owner: None,
        cli: accounts::Cli::Claude,
        account: Some("work".into()),
        email: Some("work@example.com".into()),
        profile_key: None,
        week: SessionWeek::default(),
        config: Some(aterm_toml),
        state: Some(state),
        home: Some(home),
    };
    Scratch { _dir: dir, key }
}

const NOON: i64 = 1_794_398_400;

/// THE HISTORY IS FOLDED AGAIN ONLY WHEN A SERIES FILE OR THE LOCAL DAY MOVES: the card's
/// 2-second re-reads hand back the fold they hold. CONTROL: a file written, and the next local
/// day, each fold once more.
#[test]
fn the_history_is_refolded_only_when_a_stamp_or_the_day_moves() {
    let scratch = scratch();
    let memo = Mutex::default();
    let _ = crate::work_counts::take_account_history_folds();
    let first = read_account_at(&scratch.key, &memo, NOON);
    assert!(first.pool.is_ok(), "{:?}", first.pool);
    let card = |read: &AccountRead| read.card.clone().expect("a read that was made has a card");
    assert_eq!(card(&first).who.label(), Some("work"), "its roster row");
    assert_eq!(crate::work_counts::take_account_history_folds(), 1);
    for step in 1..=5 {
        let again = read_account_at(&scratch.key, &memo, NOON + 2 * step);
        assert!(
            Arc::ptr_eq(&card(&again).history, &card(&first).history),
            "the kept fold is the card's history"
        );
    }
    assert_eq!(
        crate::work_counts::take_account_history_folds(),
        0,
        "nothing moved, nothing folded"
    );

    let state_dir = scratch.key.state.as_deref().unwrap();
    let machine = crate::fact_menu::PoolRead::load(
        accounts::Cli::Claude,
        (
            scratch.key.config.as_deref().unwrap(),
            state_dir,
            scratch.key.home.as_deref(),
        ),
        NOON,
    )
    .unwrap()
    .state
    .machine;
    let series =
        aterm_agent::harness::account_series::path_of(state_dir, "work", &machine).unwrap();
    std::fs::create_dir_all(series.parent().unwrap()).unwrap();
    std::fs::write(&series, "\n").unwrap();
    let _ = read_account_at(&scratch.key, &memo, NOON + 20);
    assert_eq!(
        crate::work_counts::take_account_history_folds(),
        1,
        "a series file moved"
    );
    let _ = read_account_at(&scratch.key, &memo, NOON + 22);
    assert_eq!(crate::work_counts::take_account_history_folds(), 0);
    let _ = read_account_at(&scratch.key, &memo, NOON + 2 * 86_400);
    assert_eq!(
        crate::work_counts::take_account_history_folds(),
        1,
        "the local day moved"
    );
}

/// THE POOL FILE AND THE CLAIMS TABLE ARE READ AT MOST ONCE A MINUTE across a card's re-reads,
/// and again once the minute is over or the clock steps back.
#[test]
fn the_claims_read_is_reused_within_a_minute() {
    let scratch = scratch();
    let memo = Mutex::default();
    let _ = crate::work_counts::take_account_machines_reads();
    for at in [0, 2, 30, MACHINES_EVERY_S - 1] {
        let _ = read_account_at(&scratch.key, &memo, NOON + at);
    }
    assert_eq!(crate::work_counts::take_account_machines_reads(), 1);
    let _ = read_account_at(&scratch.key, &memo, NOON + MACHINES_EVERY_S);
    assert_eq!(
        crate::work_counts::take_account_machines_reads(),
        1,
        "a minute on"
    );
    let _ = read_account_at(&scratch.key, &memo, NOON);
    assert_eq!(
        crate::work_counts::take_account_machines_reads(),
        1,
        "a clock that stepped back"
    );
}

/// A MAC WITH NO VAULT HAS NO CLAIMS TABLE TO READ. A table read that fails there is the
/// table's absence (the card says this Mac does not read other Macs), never a read that failed
/// just now. CONTROL: the same failure on a Mac whose vault is configured is a failed read.
#[test]
fn a_failed_claims_read_with_no_vault_is_no_table() {
    use aterm_agent::harness::claims::{self, Row, Table, Tabled};
    struct Unread;
    impl Table for Unread {
        fn read(&self, _: &std::path::Path) -> Result<Tabled, String> {
            Err("this Mac has no Commons".into())
        }
        fn write(&self, _: &std::path::Path, _: &Row) -> Result<(), String> {
            panic!("a card's read writes no claim");
        }
    }
    let claims_at = |scratch: &Scratch| {
        let memo = Mutex::default();
        claims::with_table(Arc::new(Unread), || {
            read_account_at(&scratch.key, &memo, NOON)
        })
        .card
        .expect("a card")
        .machine
        .claims
    };
    let bare = scratch();
    assert_eq!(claims_at(&bare), account_card::Claims::Absent);
    let member = scratch();
    let vault = member
        .key
        .home
        .as_deref()
        .unwrap()
        .join(".config/aterm/secrets");
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("source.toml"),
        "url = \"http://127.0.0.1:9/v1/lab\"\nname = \"lab\"\n",
    )
    .unwrap();
    assert_eq!(claims_at(&member), account_card::Claims::Unreadable);
}

/// A FAILED POOL READ KEEPS THE CARD, and the card never claims a history it could not look
/// for: the roster cannot be read, so the rows are refused with the reason, and the card names
/// the account by the session's own address with no series read made.
#[test]
fn a_failed_pool_read_keeps_the_card() {
    let scratch = scratch();
    let config = scratch.key.config.as_deref().unwrap();
    std::fs::write(config.with_file_name("accounts.toml"), "[[account]\n").unwrap();
    let memo = Mutex::default();
    let _ = crate::work_counts::take_account_history_folds();
    let read = read_account_at(&scratch.key, &memo, NOON);
    assert!(read.pool.is_err(), "{:?}", read.pool);
    let card = read
        .card
        .expect("the card is built whatever the pool read did");
    assert_eq!(
        card.who,
        account_card::Who::Outside {
            email: "work@example.com".into()
        },
        "no roster names it, the session's address does"
    );
    assert_eq!(card.read_at, NOON);
    assert_eq!(
        crate::work_counts::take_account_history_folds(),
        0,
        "no store, so no machine id to find a series by"
    );
}

/// A FAILED POOL READ STILL KNOWS WHICH MAC THIS IS. The account's home is this Mac in the pool
/// file; with the roster unreadable the card still names that home as this Mac, from the
/// store's own state, and where that cannot be read either, from what the last read that
/// worked left. Negative control: with neither, the home is a Mac the card cannot call its own.
#[test]
fn a_failed_pool_read_still_knows_this_mac() {
    let scratch = scratch();
    let (config, state_dir, home) = (
        scratch.key.config.as_deref().unwrap(),
        scratch.key.state.as_deref().unwrap(),
        scratch.key.home.as_deref().unwrap(),
    );
    std::fs::create_dir_all(home.join(".secrets")).unwrap();
    std::fs::write(
        home.join(".secrets").join("ai-pool.toml"),
        "version = 1\n[[account]]\nprovider = \"claude\"\nemail = \"work@example.com\"\n\
         warm = [\"m16\"]\n",
    )
    .unwrap();
    let store = accounts::Store::at_for(state_dir, accounts::Cli::Claude, home);
    let (mut state, _) = store.load(NOON).unwrap();
    state.swarm_label = Some("m16".into());
    store.save(&state).unwrap();
    let ours = |read: &AccountRead| match &read.card.as_ref().expect("a card").machine.home {
        account_card::Home::Known { mac, .. } => mac.ours,
        other => panic!("the pool file names its home: {other:?}"),
    };
    let memo = Mutex::default();
    let good = read_account_at(&scratch.key, &memo, NOON);
    assert!(good.pool.is_ok(), "{:?}", good.pool);
    assert!(ours(&good));
    let roster = config.with_file_name("accounts.toml");
    std::fs::write(&roster, "[[account]\n").unwrap();
    // From the store's own state, with nothing kept.
    let failed = read_account_at(&scratch.key, &Mutex::default(), NOON);
    assert!(failed.pool.is_err());
    assert!(ours(&failed), "the store still says which Mac this is");
    // From the last read that worked, with the state unreadable too.
    std::fs::write(store.state_file(), b"not an account state").unwrap();
    let failed = read_account_at(&scratch.key, &memo, NOON + 2);
    assert!(failed.pool.is_err());
    assert!(ours(&failed), "the memo still says which Mac this is");
    let unknown = read_account_at(&scratch.key, &Mutex::default(), NOON);
    assert!(!ours(&unknown), "CONTROL: nothing names this Mac");
}
