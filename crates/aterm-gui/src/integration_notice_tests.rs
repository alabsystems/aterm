// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{AGENT_MOVE_GRACE, Heal, IntegrationNotices, heal_for};
use crate::{App, WindowId};
use std::time::{Duration, Instant};

/// A headless App with one stub tab whose engine is degraded as an adoption
/// that carried no nonce leaves it (`spawn::authorize_adopted_shell_nonce`:
/// the requirement standing, no key authorized), running `program`.
fn degraded_tab(program: &str) -> (App, u64) {
    let mut app = App::headless_for_test();
    let sid = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(sid));
    let pooled = app.pool.get(sid).expect("pooled");
    crate::term_lock(&pooled.term).set_require_shell_integration_nonce(true);
    front(&app, sid, program);
    (app, sid)
}

fn front(app: &App, sid: u64, program: &str) {
    let pooled = app.pool.get(sid).expect("pooled");
    let mut tl = pooled.ctx.timeline.lock().unwrap();
    tl.note_foreground_group(4242);
    tl.set_program(4242, Some(program.to_string()));
}

/// The rows on the glass under the lost-integration key, as `(title, detail)`.
fn rows(app: &App) -> Vec<(String, Vec<String>)> {
    app.messages
        .live_rows()
        .filter(|l| {
            l.msg
                .key
                .as_deref()
                .is_some_and(|k| k.starts_with("session.integration"))
        })
        .map(|l| (l.msg.title.clone(), l.msg.detail.clone()))
        .collect()
}

/// A shell spawned with the re-key channel heals at its next prompt by
/// itself: a record, never a row.
#[test]
fn a_channel_tab_is_a_record_and_a_closed_tab_takes_its_row() {
    let (mut app, sid) = degraded_tab("claude");
    app.store.write().unwrap().mark_rekey_channel(sid);
    let _ = app.observe_session_statuses(Instant::now());
    assert!(rows(&app).is_empty(), "a record, never a row");
    let record = app
        .messages
        .log()
        .records()
        .find(|r| {
            r.key
                .as_deref()
                .is_some_and(|k| k.starts_with("session.integration."))
        })
        .expect("the record");
    assert!(record.detail[0].starts_with("in tab 2:"), "{record:?}");

    let (mut app, sid) = degraded_tab("vim");
    let _ = app.observe_session_statuses(Instant::now());
    assert_eq!(rows(&app).len(), 1);
    app.retire_session_status(sid);
    assert!(rows(&app).is_empty(), "the closed tab's row went with it");
}

/// A row the person put down stays down, a newly degraded tab included, and the heal that
/// follows still closes its record as fixed.
#[test]
fn a_dismissed_row_stays_down_until_news_and_its_heal_still_reads_fixed() {
    let (mut app, a) = degraded_tab("vim");
    let mut t = Instant::now();
    let _ = app.observe_session_statuses(t);
    let first = app
        .messages
        .live_by_key(crate::message_reporters::KEY_INTEGRATION_LOST)
        .expect("the row")
        .id;
    assert!(app.messages.dismiss(first, t));
    t += Duration::from_millis(300);
    let _ = app.observe_session_statuses(t);
    assert!(rows(&app).is_empty(), "put down: stays down");

    let b = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(b));
    crate::term_lock(&app.pool.get(b).expect("pooled").term)
        .set_require_shell_integration_nonce(true);
    front(&app, b, "zsh");
    t += Duration::from_millis(300);
    let _ = app.observe_session_statuses(t);
    assert!(
        rows(&app).is_empty(),
        "put down: a further tab is the same loss: {:?}",
        rows(&app)
    );

    crate::term_lock(&app.pool.get(a).expect("pooled").term)
        .authorize_shell_integration([0x3C; 32]);
    t += Duration::from_millis(300);
    let _ = app.observe_session_statuses(t);
    assert!(rows(&app).is_empty(), "a heal is no news: {:?}", rows(&app));

    crate::term_lock(&app.pool.get(b).expect("pooled").term)
        .authorize_shell_integration([0x3C; 32]);
    t += Duration::from_millis(300);
    let _ = app.observe_session_statuses(t);
    assert!(rows(&app).is_empty());
    let record = app.messages.log().get(first).expect("the record");
    assert_eq!(
        record.title,
        crate::message_reporters::INTEGRATION_BACK,
        "{record:?}"
    );
}

#[test]
fn a_channel_heals_at_the_prompt_a_moved_agent_by_its_move_anything_else_in_a_new_tab() {
    use aterm_phase::Program;
    assert_eq!(
        heal_for(true, Some(Program::Claude), true, false),
        Heal::NextPrompt
    );
    assert_eq!(
        heal_for(true, Some(Program::Claude), false, false),
        Heal::NextPrompt
    );
    assert_eq!(
        heal_for(false, Some(Program::Claude), true, false),
        Heal::AgentMoved {
            agent: "Claude Code"
        }
    );
    assert_eq!(
        heal_for(false, Some(Program::Codex), true, true),
        Heal::AgentMoved { agent: "Codex" }
    );
    assert_eq!(
        heal_for(false, Some(Program::Codex), true, false),
        Heal::NewTab
    );
    assert_eq!(
        heal_for(false, Some(Program::Claude), false, false),
        Heal::NewTab
    );
    assert_eq!(
        heal_for(false, Some(Program::Codex), false, true),
        Heal::NewTab
    );
    assert_eq!(
        heal_for(false, Some(Program::Generic), true, false),
        Heal::NewTab
    );
    assert_eq!(heal_for(false, None, true, false), Heal::NewTab);
    assert_eq!(
        heal_for(false, Some(Program::Agy), true, false),
        Heal::NewTab
    );
    assert_eq!(
        heal_for(false, Some(Program::Muse), true, false),
        Heal::NewTab
    );
}

#[test]
fn only_a_changed_reading_moves_and_the_moves_own_flip_is_held() {
    let t = Instant::now();
    let mut n = IntegrationNotices::default();
    assert!(!n.observe(1, None, t), "first read healthy: nothing to say");
    assert!(!n.observe(1, None, t));
    assert!(n.observe(2, Some(Heal::NewTab), t), "first read degraded");
    assert!(
        !n.observe(2, Some(Heal::NewTab), t),
        "the same reading again"
    );
    let agent = Some(Heal::AgentMoved {
        agent: "Claude Code",
    });
    assert!(n.observe(2, agent, t), "its program was only now resolved");
    assert!(!n.observe(2, Some(Heal::NewTab), t));
    assert!(!n.observe(2, Some(Heal::NewTab), t + AGENT_MOVE_GRACE / 2));
    assert!(n.observe(2, Some(Heal::NewTab), t + AGENT_MOVE_GRACE));
    assert!(n.observe(2, None, t + AGENT_MOVE_GRACE), "healed");
    assert!(n.healed);
    assert!(!n.observe(2, None, t + AGENT_MOVE_GRACE));
    assert!(
        n.observe(1, Some(Heal::NextPrompt), t),
        "a later loss moves too"
    );
    assert_eq!(n.records, vec![1]);
}
