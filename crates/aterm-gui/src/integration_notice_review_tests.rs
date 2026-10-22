// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! App-level tests of `integration_notice` through the shipping path: the status sweep
//! (`App::observe_session_statuses`), a session's close (`App::retire_session_status`) and the
//! handoff carry (`App::carried_messages` / `App::seed_carried_messages`), reading only the
//! message center.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_messages::{LogRecord, LogState, Outcome, Retired};

use crate::harness_host::HostHandle;
use crate::{App, WindowId};

const KEY: &str = "session.integration";

/// A degraded stub tab in `app` (the adoption's shape: the nonce required, no
/// key authorized), with `program` in front. Returns its local id.
fn add_degraded(app: &mut App, program: &str) -> u64 {
    let id = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(id));
    let pooled = app.pool.get(id).expect("pooled");
    crate::term_lock(&pooled.term).set_require_shell_integration_nonce(true);
    front(app, id, program);
    id
}

fn front(app: &App, id: u64, program: &str) {
    let pooled = app.pool.get(id).expect("pooled");
    let mut tl = pooled.ctx.timeline.lock().unwrap();
    tl.note_foreground_group(4242);
    tl.set_program(4242, Some(program.to_string()));
}

fn heal(app: &App, id: u64) {
    let pooled = app.pool.get(id).expect("pooled");
    crate::term_lock(&pooled.term).authorize_shell_integration([0x3C; 32]);
}

fn sid(app: &App, id: u64) -> String {
    app.store
        .read()
        .unwrap()
        .by_local(id)
        .expect("registered")
        .sid
        .as_str()
        .to_string()
}

/// The live rows under the key or beneath it.
fn live(app: &App) -> Vec<(aterm_messages::MessageId, String, Vec<String>, u32)> {
    app.messages
        .live_rows()
        .filter(|l| l.msg.key.as_deref().is_some_and(|k| k.starts_with(KEY)))
        .map(|l| (l.id, l.msg.title.clone(), l.msg.detail.clone(), l.repeats))
        .collect()
}

/// Every record under the key, retired or not.
fn records(app: &App) -> Vec<LogRecord> {
    app.messages
        .log()
        .records()
        .filter(|r| r.key.as_deref().is_some_and(|k| k.starts_with(KEY)))
        .cloned()
        .collect()
}

fn resolved_ok(r: &LogRecord) -> bool {
    r.state == LogState::Retired(Retired::Resolved(Outcome::Ok))
}

/// Every word the band ever showed under the key: the live rows and the
/// records.
fn every_word(app: &App) -> String {
    let mut all = String::new();
    for (_, title, detail, _) in live(app) {
        all.push_str(&title);
        all.push_str(&detail.join("\n"));
    }
    for r in records(app) {
        all.push_str(&r.title);
        all.push_str(&r.detail.join("\n"));
    }
    all
}

/// A harness host over no real session under the default `[harness]` (on,
/// upgrade and relaunch on), or under `cfg`.
fn harness(cfg: Option<aterm_agent::supervise::SupervisorConfig>) -> HostHandle {
    let host = HostHandle::for_restored_test(Arc::new(|_, _, _, _| "refused:inert".to_string()));
    if let Some(cfg) = cfg {
        host.set_config(cfg);
    }
    host
}

fn sweep(app: &mut App, t: &mut Instant, by: Duration) {
    *t += by;
    let _ = app.observe_session_statuses(*t);
}

const STEP: Duration = Duration::from_millis(300);

/// The row names the tab by its place; the session id rides only the technical line.
#[test]
fn the_row_names_the_tab_by_its_place_never_its_session_id() {
    let mut app = App::headless_for_test();
    let id = add_degraded(&mut app, "vim");
    let mut t = Instant::now();
    sweep(&mut app, &mut t, STEP);
    let rows = live(&app);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (_, title, detail, _) = &rows[0];
    let sid = sid(&app, id);
    assert!(!title.contains(&sid), "{title:?}");
    let person = &detail[..2.min(detail.len())];
    assert!(
        person.iter().all(|l| !l.contains(&sid)),
        "the sid in the person's words: {detail:?}"
    );
    let place = app.upgrade_place(&sid);
    assert!(place.starts_with("in tab "), "{place:?}");
    assert!(detail[0].contains(&place), "named by place: {detail:?}");
}

/// Two degraded tabs are one row that counts them; a heal restates it in place, and the last
/// heal ends it.
#[test]
fn two_degraded_tabs_are_one_row_that_counts_them() {
    let mut app = App::headless_for_test();
    let a = add_degraded(&mut app, "vim");
    let b = add_degraded(&mut app, "zsh");
    let mut t = Instant::now();
    sweep(&mut app, &mut t, STEP);
    let rows = live(&app);
    assert_eq!(rows.len(), 1, "one row for the condition: {rows:?}");
    let (id, title, detail, _) = &rows[0];
    assert!(title.starts_with("2 tabs"), "{title:?}");
    for tab in [a, b] {
        let place = app.upgrade_place(&sid(&app, tab));
        let bare = place.strip_prefix("in ").unwrap_or(&place);
        assert!(detail[0].contains(bare), "{bare:?} in {detail:?}");
    }

    heal(&app, a);
    sweep(&mut app, &mut t, STEP);
    let rows = live(&app);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, *id, "restated in place, never a new row");
    assert!(!rows[0].1.starts_with("2 tabs"), "one tab now: {rows:?}");
    assert!(
        records(&app).iter().all(|r| r.state == LogState::Posted),
        "one tab left: nothing retired yet"
    );

    heal(&app, b);
    sweep(&mut app, &mut t, STEP);
    assert!(live(&app).is_empty());
}

/// A changed remedy restates the row (never a `✓` for a tab still degraded); only the heal
/// resolves it fixed. Negative control for the grace: past [`super::AGENT_MOVE_GRACE`] the
/// flip is said.
#[test]
fn a_remedy_change_is_restated_and_only_the_heal_reads_fixed() {
    let mut app = App::headless_for_test();
    let host = harness(None);
    app.harness = Some(host.clone());
    let id = add_degraded(&mut app, "claude");
    let mut t = Instant::now();
    sweep(&mut app, &mut t, STEP);
    let rows = live(&app);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].2[1].contains(" anew"), "{rows:?}");
    let row = rows[0].0;

    front(&app, id, "zsh");
    sweep(&mut app, &mut t, STEP);
    sweep(&mut app, &mut t, Duration::from_secs(61));
    let rows = live(&app);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, row, "the same row, restated");
    assert!(rows[0].2[1].starts_with("open a new tab"), "{rows:?}");
    assert!(
        !records(&app).iter().any(resolved_ok),
        "a changed problem is not a fixed one: {:?}",
        records(&app)
    );

    heal(&app, id);
    sweep(&mut app, &mut t, STEP);
    assert!(live(&app).is_empty());
    let fixed: Vec<LogRecord> = records(&app).into_iter().filter(resolved_ok).collect();
    assert_eq!(fixed.len(), 1, "{fixed:?}");
    assert_eq!(
        fixed[0].title,
        crate::message_reporters::INTEGRATION_BACK,
        "{fixed:?}"
    );
    host.shutdown_and_join();
}

/// The carried row's successor: the same App's sessions (a seamless update
/// keeps them), a fresh center seeded with what the parent carried, and a
/// fresh reading of every tab.
fn hand_over(app: &mut App) {
    // A standing row is born late (the Row Law): the parent has shown it by the
    // time it parks.
    app.settle_messages(Instant::now() + aterm_messages::PROGRESS_GRACE);
    let carried = app.carried_messages();
    let carry = crate::session_store::WindowCarry {
        rows: 24,
        cols: 80,
        outer_x: None,
        outer_y: None,
        status_bar_rows: app.message_band_rows,
        messages: carried.messages,
        next_message_id: carried.next_message_id,
        update_verified_unix_ms: None,
        font_px_milli: None,
        font_reset_px_milli: None,
        font_size_milli: None,
        font_reset_size_milli: None,
        update_health_said: carried.update_health_said,
        messages_captured_unix_ms: carried.captured_unix_ms,
        inner_px: None,
        presence_row: false,
    };
    let mut fresh = App::headless_for_test();
    std::mem::swap(&mut app.messages, &mut fresh.messages);
    app.integration_notices = super::IntegrationNotices::default();
    app.seed_carried_messages(&carry, None);
}

/// A carried row whose tab healed between the parent's last sweep and the park ends fixed.
#[test]
fn a_carried_row_whose_tab_healed_meanwhile_ends_fixed() {
    let mut app = App::headless_for_test();
    let id = add_degraded(&mut app, "vim");
    let mut t = Instant::now();
    sweep(&mut app, &mut t, STEP);
    assert_eq!(live(&app).len(), 1);
    hand_over(&mut app);
    assert_eq!(live(&app).len(), 1, "carried");
    heal(&app, id);
    sweep(&mut app, &mut t, STEP);
    assert!(live(&app).is_empty(), "healed: {:?}", live(&app));
    assert!(records(&app).iter().any(resolved_ok), "{:?}", records(&app));
}

/// A carried row whose tab is still degraded is adopted: restated, never posted again.
#[test]
fn a_carried_row_whose_tab_is_still_degraded_is_adopted_once() {
    let mut app = App::headless_for_test();
    let _ = add_degraded(&mut app, "vim");
    let mut t = Instant::now();
    sweep(&mut app, &mut t, STEP);
    let before = live(&app);
    hand_over(&mut app);
    sweep(&mut app, &mut t, STEP);
    sweep(&mut app, &mut t, STEP);
    let after = live(&app);
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(after[0].0, before[0].0, "adopted, never posted again");
    assert_eq!(after[0].3, 1, "no repeat bumped: {after:?}");
}

/// A carried row whose tab closed during the handoff is withdrawn, never a `✓`.
#[test]
fn a_carried_row_whose_tab_is_gone_is_withdrawn() {
    let mut parent = App::headless_for_test();
    let _ = add_degraded(&mut parent, "vim");
    let mut t = Instant::now();
    sweep(&mut parent, &mut t, STEP);
    let mut successor = App::headless_for_test();
    let other = successor.next_session_id;
    successor.push_stub_tab(WindowId(0), crate::stub_session(other));
    std::mem::swap(&mut successor.messages, &mut parent.messages);
    successor.integration_notices = super::IntegrationNotices::default();
    hand_over(&mut successor);
    assert_eq!(live(&successor).len(), 1, "carried");
    sweep(&mut successor, &mut t, STEP);
    assert!(live(&successor).is_empty(), "{:?}", live(&successor));
    assert!(
        !records(&successor).iter().any(resolved_ok),
        "a closed tab is no fix"
    );
}

/// The harness ending its agent to move it is not a remedy change: the row never says
/// `a new tab`, and the heal that follows is its one `✓`.
#[test]
fn the_harness_moving_the_agent_is_not_a_remedy_change() {
    let mut app = App::headless_for_test();
    let host = harness(None);
    app.harness = Some(host.clone());
    let id = add_degraded(&mut app, "claude");
    let mut t = Instant::now();
    sweep(&mut app, &mut t, STEP);
    assert!(live(&app)[0].2[1].contains(" anew"), "{:?}", live(&app));

    // The harness ends Claude Code: the shell is in front…
    front(&app, id, "zsh");
    sweep(&mut app, &mut t, STEP);
    sweep(&mut app, &mut t, Duration::from_secs(2));
    // …until its relaunch line re-keys the shell.
    heal(&app, id);
    front(&app, id, "claude");
    sweep(&mut app, &mut t, STEP);

    assert!(live(&app).is_empty(), "{:?}", live(&app));
    let words = every_word(&app);
    assert!(
        !words.contains("open a new tab") && !words.contains("new tab only"),
        "a false remedy was said: {words}"
    );
    let retired: Vec<LogRecord> = records(&app)
        .into_iter()
        .filter(|r| r.state != LogState::Posted)
        .collect();
    assert_eq!(
        retired.len(),
        1,
        "one entry, no intermediate ✓: {retired:?}"
    );
    assert!(resolved_ok(&retired[0]), "{retired:?}");
    host.shutdown_and_join();
}

/// The harness's move is the remedy only when the harness will type its line: on, with
/// `[harness] upgrade` or `relaunch`. Otherwise a new tab is.
#[test]
fn the_move_is_offered_only_when_the_harness_will_type_it() {
    let remedy = |app: &App| live(app)[0].2[1].clone();

    let mut app = App::headless_for_test();
    let _ = add_degraded(&mut app, "claude");
    let mut t = Instant::now();
    sweep(&mut app, &mut t, STEP);
    assert!(
        !remedy(&app).contains(" anew"),
        "no harness: {}",
        remedy(&app)
    );

    let mut app = App::headless_for_test();
    let host = harness(Some(aterm_agent::supervise::SupervisorConfig {
        upgrade: false,
        relaunch: false,
        ..Default::default()
    }));
    app.harness = Some(host.clone());
    let _ = add_degraded(&mut app, "claude");
    let mut t = Instant::now();
    sweep(&mut app, &mut t, STEP);
    assert!(
        !remedy(&app).contains(" anew"),
        "a harness that moves nothing: {}",
        remedy(&app)
    );
    // Relaunch on alone types the line too.
    host.set_config(aterm_agent::supervise::SupervisorConfig {
        upgrade: false,
        relaunch: true,
        ..Default::default()
    });
    sweep(&mut app, &mut t, STEP);
    assert!(remedy(&app).contains(" anew"), "{}", remedy(&app));
    // …and with no upgrade to come, the remedy promises no new build.
    assert!(!remedy(&app).contains("new build"), "{}", remedy(&app));
    // A disabled Claude is not moved even while other agents are enabled.
    host.set_config(aterm_agent::supervise::SupervisorConfig {
        off: vec![aterm_phase::Program::Claude],
        ..Default::default()
    });
    sweep(&mut app, &mut t, STEP);
    sweep(&mut app, &mut t, Duration::from_secs(61));
    let disabled = remedy(&app);
    // The same Claude remains movable when only Codex is switched off.
    host.set_config(aterm_agent::supervise::SupervisorConfig {
        off: vec![aterm_phase::Program::Codex],
        ..Default::default()
    });
    sweep(&mut app, &mut t, Duration::from_secs(61));
    let enabled = remedy(&app);
    host.shutdown_and_join();
    assert!(disabled.starts_with("open a new tab"), "{disabled}");
    assert!(enabled.contains(" anew"), "{enabled}");
}

/// A CLAUDE CODE STARTED AS `node` HEALS WITH THE HARNESS'S MOVE (the npm
/// ): the sweep asked the published program's NAME, so a
/// `node` the host supervises by its screen (`harness_host::agent_of`) was
/// offered a new tab though the live upgrade moves it in its own tab.
/// NEGATIVE CONTROL: a `node` whose screen named no agent is offered a new tab.
#[test]
fn a_node_hosted_agent_the_harness_moves_heals_with_its_move() {
    for (reader, moved) in [(true, true), (false, false)] {
        let mut app = App::headless_for_test();
        let host = harness(None);
        app.harness = Some(host.clone());
        let id = add_degraded(&mut app, "node");
        app.pool
            .get(id)
            .expect("pooled")
            .ctx
            .timeline
            .lock()
            .unwrap()
            .set_agent_reader_for_test(reader);
        let mut t = Instant::now();
        sweep(&mut app, &mut t, STEP);
        let said = live(&app)[0].2[1].clone();
        host.shutdown_and_join();
        assert_eq!(
            said.contains(" anew"),
            moved,
            "screen read: {reader}: {said}"
        );
    }
}

/// A successor says nothing before its Commit (its harness host is suspended, so a word then
/// would offer `a new tab` for an agent about to be moved); after it, the row offers the move.
#[test]
fn the_successor_says_nothing_before_its_commit() {
    let mut app = App::headless_for_test();
    let host = harness(None);
    host.suspend();
    app.harness = Some(host.clone());
    app.incoming_handoff_pending = true;
    let _ = add_degraded(&mut app, "claude");
    let mut t = Instant::now();
    sweep(&mut app, &mut t, STEP);
    sweep(&mut app, &mut t, STEP);
    assert!(
        live(&app).is_empty() && records(&app).is_empty(),
        "said before Commit: {}",
        every_word(&app)
    );

    // The Commit: the host resumes and the handoff is no longer pending.
    app.incoming_handoff_pending = false;
    host.resume();
    sweep(&mut app, &mut t, STEP);
    let rows = live(&app);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].2[1].contains(" anew"), "{rows:?}");
    let words = every_word(&app);
    assert!(
        !words.contains("open a new tab"),
        "a false remedy was said: {words}"
    );
    host.shutdown_and_join();
}
