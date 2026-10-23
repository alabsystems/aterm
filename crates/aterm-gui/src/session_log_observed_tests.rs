// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The remaining marks (plan §3.5).

use std::time::{Duration, Instant};

use super::super::tests::{blocks, finished, lines, lines_of, opened, running, stamp, with_claude};
use super::*;
use crate::{App, WindowId};

fn watching(app: &App, session: u64) -> crate::session_watch::Subscription {
    let sub = crate::session_watch::Subscription::single(
        &app.pool.get(session).expect("a live session").ctx.watchers,
    );
    let _ = sub.wait(Duration::ZERO);
    sub
}

fn having<'a>(got: &'a [String], needle: &str) -> Vec<&'a String> {
    got.iter().filter(|l| l.contains(needle)).collect()
}

#[test]
fn the_sweep_marks_what_the_engine_says_once() {
    let mut app = App::headless_for_test();
    let front = app.front_terminal(WindowId(0)).expect("a front terminal");
    let (session, term) = (front.session, std::sync::Arc::clone(&front.term));
    let timeline = std::sync::Arc::clone(&app.pool.get(session).expect("pooled").ctx.timeline);
    crate::term_lock(&term).process(b"\x1b]9;SENTINEL-EARLY\x07");
    let t0 = Instant::now();
    app.observe_session_statuses(t0);
    let base = lines_of(&timeline);
    assert!(having(&base, " notify ").is_empty(), "a baseline: {base:?}");
    assert_eq!(
        having(&base, " mark - integration state=").len(),
        1,
        "{base:?}"
    );
    let sub = watching(&app, session);
    let _ = timeline.lock().unwrap().slog_take_wake();
    crate::term_lock(&term).process(b"\x1b]9;SENTINEL-BODY\x07\x07\x1b]9;4;1;42\x07");
    app.observe_session_statuses(t0 + Duration::from_secs(1));
    assert!(
        sub.wait(aterm_spec::wait::MUST_HAPPEN),
        "the sweep woke the watchers"
    );
    let got = lines_of(&timeline);
    assert_eq!(having(&got, " mark - notify n=1 ").len(), 1, "{got:?}");
    assert_eq!(having(&got, " mark - bell n=1 ").len(), 1, "{got:?}");
    let progress = having(&got, " progress ");
    assert_eq!(progress.len(), 1, "{got:?}");
    assert!(
        progress[0].contains(" state=normal ")
            && progress[0].contains(" pct=42 ")
            && progress[0].contains(" src=engine conf=exact "),
        "{progress:?}"
    );
    app.observe_session_statuses(t0 + Duration::from_secs(2));
    let again = lines_of(&timeline);
    for kind in [" notify ", " bell ", " progress ", " integration "] {
        assert_eq!(
            having(&again, kind).len(),
            having(&got, kind).len(),
            "{kind}: {again:?}"
        );
    }
    let g = timeline.lock().unwrap();
    for rec in g.slog().log().window() {
        let line = aterm_slog::encode(rec, aterm_slog::Tier::Content).expect("fits");
        assert!(!line.contains("SENTINEL"), "{line}");
    }
}

#[test]
fn a_status_transition_is_one_typed_mark() {
    let mut app = App::headless_for_test();
    let front = app.front_terminal(WindowId(0)).expect("a front terminal");
    let (session, term) = (front.session, std::sync::Arc::clone(&front.term));
    let timeline = std::sync::Arc::clone(&app.pool.get(session).expect("pooled").ctx.timeline);
    crate::term_lock(&term).process(
        b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\false\r\n\x1b]133;C\x1b\\\
          \x1b]133;D;1\x1b\\\x1b]133;A\x1b\\\x1b]133;B\x1b\\",
    );
    let t0 = Instant::now();
    for i in 0..4 {
        app.observe_session_statuses(t0 + Duration::from_secs(i));
    }
    let got = lines_of(&timeline);
    let marks = having(&got, " status phase=");
    let rows: Vec<String> = timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|e| e.kind == crate::session_status::STATUS_CHANGE)
        .map(|e| e.payload.to_string())
        .collect();
    assert_eq!(
        marks.len(),
        rows.len(),
        "one mark a transition: {got:?} {rows:?}"
    );
    let failed: Vec<&&String> = marks.iter().filter(|l| l.contains(" out=fail ")).collect();
    assert_eq!(failed.len(), 1, "{got:?}");
    assert!(
        failed[0].contains(" code=1 ") && failed[0].contains(" src=status "),
        "{failed:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.contains("outcome=failure exit_code=1")),
        "{rows:?}"
    );
    app.observe_session_statuses(t0 + Duration::from_secs(9));
    assert_eq!(
        having(&lines_of(&timeline), " status phase=").len(),
        marks.len()
    );
}

fn claude_screen(ctx: Option<u8>, goal: bool) -> Vec<String> {
    let rule = "\u{2500}".repeat(120);
    let right = |t: &str| format!("{}{t}", " ".repeat(120 - t.chars().count()));
    let mut rows = vec![String::new(); 24];
    rows[0] = "\u{23fa} Working on it.".into();
    rows[17] = "\u{2736} Deliberating\u{2026} (3s \u{00b7} thinking)".into();
    if goal {
        rows[18] = right("\u{25ce} /goal active (3h)");
    }
    if let Some(n) = ctx {
        rows[19] = right(&format!("Context left until auto-compact: {n}%"));
    }
    rows[20] = rule.clone();
    rows[21] = "\u{276f} ".into();
    rows[22] = rule;
    rows[23] = "  \u{23f5}\u{23f5} bypass permissions on \u{00b7} esc to interrupt".into();
    rows
}

fn paint(app: &App, sid: u64, rows: &[String]) {
    let pooled = app.pool.get(sid).expect("pooled");
    let mut t = crate::term_lock(&pooled.term);
    if t.cols() != 120 {
        t.resize(24, 120);
    }
    let mut bytes = String::from("\x1b[H\x1b[2J");
    for (i, row) in rows.iter().enumerate() {
        bytes.push_str(&format!("\x1b[{};1H{row}", i + 1));
    }
    t.process(bytes.as_bytes());
}

#[test]
fn claude_codes_context_and_goal_are_marked_when_they_move() {
    let mut app = App::headless_for_test();
    let sid = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(sid));
    let timeline = std::sync::Arc::clone(&app.pool.get(sid).expect("pooled").ctx.timeline);
    let t0 = Instant::now();
    let step = Duration::from_millis(300);
    let mut t = t0;
    for ctx in [Some(12), Some(15), Some(15), Some(34)] {
        paint(&app, sid, &claude_screen(ctx, true));
        let _ = app.observe_session_statuses(t);
        t += step;
    }
    let got = lines_of(&timeline);
    assert_eq!(
        timeline.lock().unwrap().agent().word,
        "busy",
        "PRECONDITION: Claude Code read by its frame: {got:?}"
    );
    let ctx: Vec<&String> = having(&got, " ctx pct=");
    assert_eq!(ctx.len(), 2, "12, then 34 (15 is the same tenth): {got:?}");
    assert!(ctx[0].contains(" ctx pct=12 ") && ctx[1].contains(" ctx pct=34 "));
    assert!(ctx[0].contains(" src=screen conf=strong "), "{ctx:?}");
    let goal = having(&got, " goal state=on ");
    assert_eq!(goal.len(), 1, "{got:?}");
    let run = ctx[0].split(' ').nth(2).expect("a node");
    assert!(run.starts_with('r'), "on the agent run: {ctx:?}");
}

#[test]
fn a_stopped_turn_closes_interrupted() {
    for stopped in [true, false] {
        let (tl, hand) = opened();
        with_claude(&tl);
        let mut g = tl.lock().unwrap();
        hand.set(1_000);
        g.publish_agent(
            "busy",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(5),
        );
        hand.set(2_000);
        if stopped {
            g.slog_mut().agent_interrupted();
        }
        g.publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(6),
        );
        hand.set(4_000);
        let _ = g.slog_tick();
        let got = lines(g.slog().log());
        let out = if stopped { "interrupted" } else { "done" };
        assert_eq!(
            having(&got, &format!(" close t1 turn out={out} ")).len(),
            1,
            "{got:?}"
        );
    }
    let screen = claude_screen(Some(40), true);
    let read = aterm_phase::read(Some("claude"), &screen, None);
    let claude = crate::presence::TurnFacts::of(aterm_phase::Program::Claude, &read);
    assert_eq!((claude.ctx_pct, claude.goal), (Some(40), Some(true)));
    assert_eq!(
        crate::presence::TurnFacts::of(aterm_phase::Program::Codex, &read),
        crate::presence::TurnFacts::default()
    );
}

#[test]
fn the_launch_card_amends_the_agent_run_once() {
    let card = aterm_phase::LaunchCard {
        version: "2.1.300".into(),
        model: "Opus 5.5".into(),
        effort: Some("xhigh".into()),
    };
    let (tl, _hand) = opened();
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().observe_card(&card, now);
    }
    assert!(having(&lines_of(&tl), " amend ").is_empty());
    with_claude(&tl);
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().observe_card(&card, now);
        g.slog_mut().observe_card(&card, now);
    }
    let got = lines_of(&tl);
    let amends = having(&got, " amend r1 agent ");
    assert_eq!(amends.len(), 1, "{got:?}");
    assert!(
        amends[0].contains(" ver=2.1.300 ")
            && amends[0].contains(" effort=xhigh ")
            && amends[0].contains(" src=screen conf=strong "),
        "{amends:?}"
    );
}

#[test]
fn every_render_transition_is_one_mark_with_its_row() {
    use crate::resize_ledger::Transition;
    assert_eq!(
        Transition::Healed {
            run: 1,
            after_ms: 2
        }
        .state_word(),
        "healed"
    );
    assert_eq!(
        Transition::Risk {
            run: 1,
            displaced: 2,
            net_zero: true,
            at_ms: 3
        }
        .state_word(),
        "desync-risk"
    );
    let (tl, _hand) = opened();
    let mut g = tl.lock().unwrap();
    let now = g.slog().now();
    for t in [
        Transition::Risk {
            run: 1,
            displaced: 2,
            net_zero: true,
            at_ms: 3,
        },
        Transition::Risk {
            run: 2,
            displaced: -1,
            net_zero: false,
            at_ms: 4,
        },
        Transition::Healed {
            run: 2,
            after_ms: 5,
        },
    ] {
        g.slog_mut().observe_render(&t, now);
    }
    let got = lines(g.slog().log());
    let marks = having(&got, " mark - render state=");
    assert_eq!(marks.len(), 3, "{got:?}");
    assert!(
        marks[1].contains(" state=desync-risk run=2 displaced=-1 net=changed at=4 "),
        "{marks:?}"
    );
    assert!(
        marks[2].contains(" state=healed run=2 after_ms=5 "),
        "{marks:?}"
    );
}

#[test]
fn the_observed_values_cross_an_update_as_words() {
    let (tl, _hand) = opened();
    let mut g = tl.lock().unwrap();
    let now = g.slog().now();
    let engine = EngineRead {
        integration: aterm_core::terminal::ShellIntegrationPosture::On,
        notify_total: 0,
        bell_total: 0,
        progress: Some(aterm_types::osc::TaskbarProgress::Paused(57)),
    };
    g.slog_mut().observe_engine(&engine, now);
    let tokens = g.slog().observed.carry_tokens();
    assert_eq!(tokens, " obs_int=on obs_prog=paused.5", "{tokens}");
    let line = format!("state v=1 origin=spawn{tokens}");
    let kv = |k: &str| {
        line.split(' ')
            .filter_map(|t| t.split_once('='))
            .find(|(key, _)| *key == k)
            .map(|(_, v)| v.to_string())
    };
    let mut read = Observed::default();
    read.read_carry(kv);
    assert_eq!(read.carry_tokens(), tokens, "{line}");
    let mut old = Observed::default();
    old.read_carry(|_| None);
    assert_eq!(old, Observed::default());
    let mut odd = Observed::default();
    odd.read_carry(|k| (k == "obs_int").then(|| "sideways".to_string()));
    assert_eq!(odd, Observed::default());
    drop(g);
    let (next, _hand) = opened();
    let mut n = next.lock().unwrap();
    let now = n.slog().now();
    n.slog_mut().observed = read;
    n.slog_mut().observe_engine(&engine, now);
    let got = lines(n.slog().log());
    assert!(
        having(&got, " integration ").is_empty() && having(&got, " progress ").is_empty(),
        "{got:?}"
    );
    let (fresh, _hand) = opened();
    let mut f = fresh.lock().unwrap();
    let now = f.slog().now();
    f.slog_mut().observe_engine(&engine, now);
    let said = lines(f.slog().log());
    assert_eq!(having(&said, " integration ").len(), 1, "{said:?}");
    assert_eq!(having(&said, " progress ").len(), 1, "{said:?}");
}

/// Every record of `timeline`'s log at the content tier (the title's).
pub(super) fn content_lines(
    timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
) -> Vec<String> {
    let g = timeline.lock().unwrap();
    g.slog()
        .log()
        .window()
        .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Content).expect("fits"))
        .collect()
}

#[test]
fn the_drift_flush_marks_the_cwd_and_the_title_when_they_move() {
    let mut app = App::headless_for_test();
    let front = app.front_terminal(WindowId(0)).expect("a front terminal");
    let (session, term) = (front.session, std::sync::Arc::clone(&front.term));
    let timeline = std::sync::Arc::clone(&app.pool.get(session).expect("pooled").ctx.timeline);
    let sub = watching(&app, session);
    let _ = timeline.lock().unwrap().slog_take_wake();
    crate::term_lock(&term)
        .process(b"\x1b]7;file://localhost/aterm-p3b/cwd\x07\x1b]2;SENTINEL-TITLE build\x07");
    let t0 = Instant::now();
    assert!(app.flush_title_drift(session, t0), "uncontended");
    assert!(
        sub.wait(aterm_spec::wait::MUST_HAPPEN),
        "the flush woke the watchers"
    );
    let got = lines_of(&timeline);
    let cwd = having(&got, " mark - cwd ");
    assert_eq!(cwd.len(), 1, "{got:?}");
    assert!(
        cwd[0].contains(" cwd=/aterm-p3b/cwd ") && cwd[0].contains(" src=engine conf=exact "),
        "{cwd:?}"
    );
    let title = having(&got, " mark - title ");
    assert_eq!(title.len(), 1, "{got:?}");
    assert!(!title[0].contains("SENTINEL"), "host tier: {title:?}");
    assert!(
        having(
            &content_lines(&timeline),
            " mark - title title=SENTINEL-TITLE"
        )
        .len()
            == 1,
        "content tier: {:?}",
        content_lines(&timeline)
    );
    for (i, t) in ["\u{280b} Build", "\u{2819} Build"].iter().enumerate() {
        crate::term_lock(&term).process(format!("\x1b]2;{t}\x07").as_bytes());
        let at = t0 + Duration::from_secs(1 + i as u64);
        assert!(app.flush_title_drift(session, at));
    }
    let got = lines_of(&timeline);
    assert_eq!(having(&got, " mark - title ").len(), 2, "{got:?}");
    assert_eq!(having(&got, " mark - cwd ").len(), 1, "{got:?}");
    {
        let mut g = timeline.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().observe_cwd(None, at);
        g.slog_mut().observe_cwd(Some(""), at);
    }
    let got = lines_of(&timeline);
    let cwd = having(&got, " mark - cwd ");
    assert_eq!(cwd.len(), 2, "{got:?}");
    assert!(!cwd[1].contains(" cwd="), "{cwd:?}");
}

#[test]
fn the_cwd_and_the_title_cross_an_update_as_fingerprints() {
    let (tl, _hand) = opened();
    let tokens = {
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut()
            .observe_cwd(Some("/aterm-p3b/SENTINEL-DIR"), at);
        g.slog_mut().observe_title("SENTINEL-TITLE", at);
        g.slog().observed.carry_tokens()
    };
    assert!(
        !tokens.contains("SENTINEL")
            && tokens.contains(" cwd_fp=")
            && tokens.contains(" title_fp="),
        "{tokens}"
    );
    let line = format!("state v=1{tokens}");
    let kv = |k: &str| {
        line.split(' ')
            .filter_map(|t| t.split_once('='))
            .find(|(key, _)| *key == k)
            .map(|(_, v)| v.to_string())
    };
    let (next, _hand) = opened();
    let mut n = next.lock().unwrap();
    n.slog_mut().observed.read_carry(kv);
    let at = n.slog().now();
    n.slog_mut()
        .observe_cwd(Some("/aterm-p3b/SENTINEL-DIR"), at);
    n.slog_mut().observe_title("SENTINEL-TITLE", at);
    let got = lines(n.slog().log());
    assert!(
        having(&got, " mark - cwd ").is_empty() && having(&got, " mark - title ").is_empty(),
        "{got:?}"
    );
    n.slog_mut().observe_title("\u{280b} SENTINEL-TITLE", at);
    assert_eq!(having(&lines(n.slog().log()), " mark - title ").len(), 1);
    let (fresh, _hand) = opened();
    let mut f = fresh.lock().unwrap();
    let at = f.slog().now();
    f.slog_mut().observe_title("", at);
    assert!(having(&lines(f.slog().log()), " mark - title ").is_empty());
}

#[test]
fn a_manual_reset_is_a_modes_mark_by_whoever_asked() {
    let (tl, _hand) = opened();
    let report = crate::manual_reset::ResetReport {
        reverted: vec!["mouse", "alt"],
        bytes: 12,
    };
    crate::manual_reset::record(9, &tl, crate::manual_reset::ResetSource::Menu, &report);
    crate::manual_reset::record(9, &tl, crate::manual_reset::ResetSource::Verb, &report);
    let got = lines_of(&tl);
    let modes = having(&got, " mark - modes n=2 ");
    assert_eq!(modes.len(), 2, "{got:?}");
    assert!(modes[0].contains(" by=human "), "{modes:?}");
    assert!(!modes[1].contains(" by="), "{modes:?}");
    let h = crate::session_store::test_handle(11);
    let _ = h.ctx.timeline.lock().unwrap().slog_take_wake();
    let reply = crate::manual_reset::cmd_reset(h.local_id, &h.term, &h.ctx, "");
    assert!(reply.starts_with("OK reset"), "{reply}");
    assert_eq!(
        having(&lines_of(&h.ctx.timeline), " mark - modes n=").len(),
        1
    );
    assert!(
        h.ctx.timeline.lock().unwrap().slog_take_wake(),
        "the verb's arm takes the mark's wake"
    );
}

#[test]
fn a_write_in_doubt_is_marked_by_its_verb_and_wakes_its_watchers() {
    let h = crate::session_store::test_handle(12);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    let woke = std::cell::Cell::new(0u32);
    let wake = || {
        woke.set(woke.get() + 1);
        h.ctx.watchers.ring_watchers();
    };
    let (since, ep) = {
        let g = h.ctx.timeline.lock().unwrap();
        (g.slog().log().last_seq(), g.slog().log().ep())
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let (h2, store2) = (h.clone(), store.clone());
    std::thread::spawn(move || {
        let reply = crate::control::cmd_await(
            &h2.term,
            &store2,
            h2.local_id,
            &h2.ctx,
            &format!("log kind=in-doubt since={since} ep={ep} timeout=600000"),
        );
        let _ = tx.send(reply);
    });
    let key = |seq: u32| format!("{}:4:{seq}", h.ctx.nonce.to_hex());
    let owner = crate::control::Scope::Owner;
    let ok = crate::pty_idem::guarded_waking(&h.ctx, owner, "send", Some(&key(1)), &wake, || {
        "OK 3 bytes\n".to_string()
    });
    assert_eq!((ok.as_str(), woke.get()), ("OK 3 bytes\n", 0));
    let failed =
        crate::pty_idem::guarded_waking(&h.ctx, owner, "send", Some(&key(2)), &wake, || {
            "ERR write SENTINEL-REPLY failed\n".to_string()
        });
    assert!(failed.starts_with("ERR write"), "{failed}");
    assert_eq!(woke.get(), 1, "the guard woke once");
    let reply = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the parked await returns on the guard's wake");
    assert!(reply.contains(" mark - in-doubt verb=send "), "{reply}");
    let replay =
        crate::pty_idem::guarded_waking(&h.ctx, owner, "send", Some(&key(2)), &wake, || {
            unreachable!("an in-doubt sequence is never replayed")
        });
    assert!(replay.starts_with("ERR in-doubt"), "{replay}");
    assert_eq!(woke.get(), 1, "a replay wakes nobody");
    let _ = crate::pty_idem::guarded_waking(&h.ctx, owner, "frob", Some(&key(3)), &wake, || {
        "ERR write failed\n".to_string()
    });
    let got = content_lines(&h.ctx.timeline);
    assert_eq!(having(&got, " mark - in-doubt ").len(), 2, "{got:?}");
    assert_eq!(
        having(&got, " mark - in-doubt verb=unknown ").len(),
        1,
        "{got:?}"
    );
    assert!(got.iter().all(|l| !l.contains("SENTINEL")), "{got:?}");
}

/// A registered session and its store: what a control worker's fabric verb runs against.
pub(super) fn fabric_rig(
    local: u64,
) -> (
    crate::session_store::SessionHandle,
    crate::session_store::Store,
) {
    let h = crate::session_store::test_handle(local);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    (h, store)
}

/// An in-process `await log kind=<kind>` parked past `h`'s log as it stands.
pub(super) fn parked_log_await(
    h: &crate::session_store::SessionHandle,
    store: &crate::session_store::Store,
    kind: &str,
) -> std::sync::mpsc::Receiver<String> {
    let (since, ep) = {
        let g = h.ctx.timeline.lock().unwrap();
        (g.slog().log().last_seq(), g.slog().log().ep())
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let (h2, store2) = (h.clone(), store.clone());
    let line = format!("log kind={kind} since={since} ep={ep} timeout=600000");
    std::thread::spawn(move || {
        let reply = crate::control::cmd_await(&h2.term, &store2, h2.local_id, &h2.ctx, &line);
        let _ = tx.send(reply);
    });
    rx
}

pub(super) fn returned(rx: &std::sync::mpsc::Receiver<String>, site: &str) -> String {
    rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .unwrap_or_else(|_| panic!("{site}: the parked await never returned"))
}

#[test]
fn a_workers_task_moves_are_task_marks_and_wake_the_log() {
    use crate::control::Scope;
    use crate::task_record::cmd_task;
    let (h, store) = fabric_rig(23);
    let tasks = |h: &crate::session_store::SessionHandle| -> Vec<String> {
        lines_of(&h.ctx.timeline)
            .into_iter()
            .filter(|l| l.contains(" mark - task "))
            .collect()
    };
    let rx = parked_log_await(&h, &store, "task");
    let reply = cmd_task(&h.ctx, h.local_id, Scope::Owner, "mark working");
    assert!(reply.starts_with("OK task rev=1 state=working"), "{reply}");
    let got = returned(&rx, "task mark working");
    assert!(
        got.contains(" mark - task rev=1 state=working attention=0 src=aterm conf=exact "),
        "{got}"
    );
    let rx = parked_log_await(&h, &store, "task");
    let reply = cmd_task(
        &h.ctx,
        h.local_id,
        Scope::Owner,
        "mark exited why=exit exit=3 result_bytes=12",
    );
    assert!(reply.contains(" changed=1"), "{reply}");
    let got = returned(&rx, "task mark exited");
    assert!(
        got.contains(
            " mark - task rev=2 state=exited attention=0 why=exit code=3 src=aterm conf=exact "
        ),
        "{got}"
    );
    let rx = parked_log_await(&h, &store, "task");
    let changed = crate::session_timeline::write_attention_owned(
        &h.ctx,
        crate::harness_host::ATTENTION_OWNER,
        crate::session_timeline::MetaEdit::Set("SENTINEL-ATTENTION"),
        aterm_slog::Actor::Harness,
    )
    .expect("the supervisor's entry");
    assert!(changed);
    let got = returned(&rx, "attention");
    assert!(
        got.contains(" mark - task rev=3 state=exited attention=1 why=exit code=3 "),
        "{got}"
    );
    let reply = cmd_task(
        &h.ctx,
        h.local_id,
        Scope::Owner,
        "mark exited why=exit exit=3 result_bytes=12",
    );
    assert!(reply.contains(" changed=0"), "{reply}");
    let marks = tasks(&h);
    assert_eq!(marks.len(), 3, "{marks:?}");
    for leak in ["SENTINEL", "result", "from=", "tasks/"] {
        assert!(marks.iter().all(|l| !l.contains(leak)), "{leak}: {marks:?}");
    }
    let seeded = crate::session_store::test_handle(24);
    let record = h
        .ctx
        .timeline
        .lock()
        .unwrap()
        .task()
        .cloned()
        .expect("a task");
    seeded.ctx.timeline.lock().unwrap().seed_task(record);
    assert!(tasks(&seeded).is_empty(), "a seed is no move");
    assert!(seeded.ctx.timeline.lock().unwrap().task().is_some());
}

#[test]
fn the_fabric_verbs_mark_the_log_and_wake_its_watchers() {
    let (h, store) = fabric_rig(21);
    let sid = h.ctx.self_id.as_str().to_string();
    let wake = |s: &aterm_session::SessionId| crate::control::wake_sid_watchers(&store, s);
    let rx = parked_log_await(&h, &store, "mail");
    let line = format!("{sid} off=7 from=h-a kind=task trust=human text=SENTINEL%20BODY");
    assert_eq!(
        crate::fabric::cmd_deliver_waking(&store, &line, &wake),
        "OK 1\n"
    );
    let reply = returned(&rx, "deliver");
    assert!(
        reply.contains(" mark - mail dir=in kind=task ")
            && reply.contains(" off=7 ")
            && reply.contains(" inbox=1 ")
            && reply.contains(" by=bridge src=fabric conf=exact "),
        "{reply}"
    );
    let _ = crate::fabric::cmd_deliver_waking(&store, &line, &wake);
    let rx = parked_log_await(&h, &store, "mail");
    let post = crate::fabric::cmd_post(&h.ctx, "to=@s-peer kind=ask --wait=0 SENTINEL-POST", None);
    assert!(super::overlap_tests::queued(&post, 1), "{post}");
    let reply = returned(&rx, "post");
    assert!(reply.contains(" mark - mail dir=out kind=ask "), "{reply}");
    let rx = parked_log_await(&h, &store, "mail-landed");
    let landed = format!("{sid} landed=1 off=9");
    assert_eq!(
        crate::fabric::cmd_deliver_waking(&store, &landed, &wake),
        "OK\n"
    );
    let reply = returned(&rx, "landed");
    assert!(
        reply.contains(" mark - mail-landed inbox=1 off=9 by=bridge "),
        "{reply}"
    );
    let rx = parked_log_await(&h, &store, "mail-seen");
    let seen = crate::fabric::cmd_inbox_seen(&h.ctx, "1 handled");
    assert!(seen.starts_with("OK seen=1"), "{seen}");
    let reply = returned(&rx, "inbox seen");
    assert!(
        reply.contains(" mark - mail-seen inbox=1 off=7 floor=8 seen=1 moved=1 word=handled "),
        "{reply}"
    );
    let rx = parked_log_await(&h, &store, "mail-seen");
    let _ = crate::fabric::cmd_inbox_seen(&h.ctx, "1 handled");
    let reply = returned(&rx, "inbox seen again");
    assert!(reply.contains(" seen=1 moved=0 src=fabric "), "{reply}");
    for (state, word) in [("on reason=demo", "on"), ("off", "off")] {
        let rx = parked_log_await(&h, &store, "hold");
        let reply = crate::fabric::cmd_hold_waking(
            &store,
            &format!("{sid} {state}"),
            crate::fabric::HoldIssuer::Owner,
            &wake,
        );
        assert!(reply.starts_with("OK hold="), "{reply}");
        let got = returned(&rx, "hold");
        assert!(
            got.contains(&format!(" mark - hold state={word} ")),
            "{got}"
        );
    }
    let got = content_lines(&h.ctx.timeline);
    assert_eq!(having(&got, " mark - mail dir=in ").len(), 1, "{got:?}");
    assert_eq!(having(&got, " mark - mail-seen ").len(), 2, "{got:?}");
    assert_eq!(having(&got, " moved=0 ").len(), 1, "{got:?}");
    assert!(got.iter().all(|l| !l.contains("SENTINEL")), "{got:?}");
    let totals = h
        .ctx
        .timeline
        .lock()
        .unwrap()
        .slog()
        .log()
        .state()
        .totals
        .clone();
    assert_eq!((totals.mail_in, totals.mail_out), (1, 1), "{totals:?}");
}

/// T13'S IN-PROCESS NEGATIVE CONTROL for 5B-7's world test, which asserts
/// presence only (the delivery path's fault gives no final positive
/// signal): its check — a delivered mail has its inbox row and its `mail
/// dir=in` mark at the row's offset — holds over the real fabric hook
/// ([`crate::fabric::FabricSession::record_fact`] on a `SessionCtx`) and
/// fails over a hook that drops the `MailIn` fact, which writes no mark and
/// so no row (the row is the mark since the ring was deleted, 6G-5).
#[test]
fn a_delivery_whose_hook_drops_the_fact_writes_no_row_and_no_mail_mark() {
    use std::sync::{Arc, RwLock};

    use aterm_session::SessionId;

    use crate::fabric::{FabricFact, FabricSession, Registry, SessionFabric};

    /// A session whose fabric hook drops the `MailIn` fact.
    struct DropsMailIn(Arc<crate::SessionCtx>);
    impl FabricSession for DropsMailIn {
        fn self_id(&self) -> &SessionId {
            self.0.self_id()
        }
        fn fabric(&self) -> &SessionFabric {
            FabricSession::fabric(&*self.0)
        }
        fn record_fact(&self, kind: &'static str, fact: FabricFact<'_>) -> bool {
            !matches!(fact, FabricFact::MailIn { .. })
                && FabricSession::record_fact(&*self.0, kind, fact)
        }
        fn fabric_changed(&self) {}
        fn holder_token(&self) -> Option<String> {
            None
        }
    }
    struct One(Arc<dyn FabricSession>);
    impl Registry for One {
        fn sessions(&self) -> Vec<Arc<dyn FabricSession>> {
            vec![Arc::clone(&self.0)]
        }
        fn session(&self, sid: &SessionId) -> Option<Arc<dyn FabricSession>> {
            (sid == self.0.self_id()).then(|| Arc::clone(&self.0))
        }
        fn bridge_lost_advised(&self, _: usize) {}
        fn torn_state_set_aside(&self, _: u64, _: bool) {}
        fn torn_state_held(&self, _: u64) {}
    }
    let check = |ctx: &crate::SessionCtx, off: u64| -> Result<(), String> {
        let g = ctx.timeline.lock().unwrap();
        let at = format!(" off={off}");
        if !g
            .since(None)
            .any(|e| e.kind == "inbox" && e.payload.ends_with(&at))
        {
            return Err(format!("no inbox row at off={off}"));
        }
        let marks = lines(g.slog().log());
        if marks
            .iter()
            .any(|l| l.contains(" mark - mail dir=in ") && l.contains(&format!("{at} ")))
        {
            Ok(())
        } else {
            Err(format!(
                "the inbox row at off={off} has no mail dir=in mark: {marks:?}"
            ))
        }
    };
    let deliver = |store: &dyn Fn(&str) -> String, sid: &str| {
        let line = format!("{sid} off=7 from=h-a kind=task trust=human text=SENTINEL");
        assert_eq!(store(&line), "OK 1\n", "the delivery");
    };
    let (h, store) = fabric_rig(24);
    deliver(
        &|l| crate::fabric::cmd_deliver_waking(&store, l, &|_| {}),
        h.ctx.self_id.as_str(),
    );
    check(&h.ctx, 7).expect("the real hook marks the delivery");
    let dropped = crate::session_store::test_handle(25);
    let reg = Arc::new(RwLock::new(One(Arc::new(DropsMailIn(Arc::clone(
        &dropped.ctx,
    ))))));
    deliver(
        &|l| crate::fabric::cmd_deliver_waking(&reg, l, &|_| {}),
        dropped.ctx.self_id.as_str(),
    );
    let e = check(&dropped.ctx, 7).expect_err("a dropped fact is caught");
    assert!(e.contains("no inbox row at off=7"), "{e}");
}

fn escalated_needs(ctx: &crate::SessionCtx) -> Vec<String> {
    let g = ctx.timeline.lock().unwrap();
    g.slog()
        .log()
        .state()
        .needs
        .iter()
        .flatten()
        .filter(|n| n.word == "escalated")
        .map(|n| {
            format!(
                "by={} src={} conf={}",
                n.by.word(),
                n.src.word(),
                n.conf.word()
            )
        })
        .collect()
}

fn meta_marks(ctx: &crate::SessionCtx) -> Vec<String> {
    lines_of(&ctx.timeline)
        .into_iter()
        .filter(|l| l.contains(" mark - meta "))
        .collect()
}

#[test]
fn the_meta_writers_mark_the_log_by_their_writer() {
    let (h, store) = fabric_rig(31);
    let rx = parked_log_await(&h, &store, "meta");
    let reply =
        crate::control::meta_line_for_test(&store, &h.term, h.local_id, &h.ctx, "set role builder");
    assert_eq!(reply, "OK\n");
    crate::control::wake_sid_watchers(&store, &h.ctx.self_id);
    let got = returned(&rx, "meta set");
    assert!(
        got.contains(" mark - meta field=role value=builder by=ctl src=meta conf=exact "),
        "{got}"
    );
    let marks = meta_marks(&h.ctx).len();
    let _ =
        crate::control::meta_line_for_test(&store, &h.term, h.local_id, &h.ctx, "set role builder");
    assert_eq!(meta_marks(&h.ctx).len(), marks, "the same value is no move");
    let _ = crate::control::meta_line_for_test(&store, &h.term, h.local_id, &h.ctx, "unset role");
    let last = meta_marks(&h.ctx).pop().expect("the unset's mark");
    assert!(
        last.contains(" mark - meta field=role by=ctl ") && !last.contains("value="),
        "{last}"
    );
    assert!(crate::session_timeline::write_server_attention(
        &h.ctx,
        crate::input_stall::SERVER_ATTENTION_OWNER,
        Some("stalled".to_string()),
    ));
    let last = meta_marks(&h.ctx).pop().expect("the stall line's mark");
    let attention = h
        .ctx
        .meta
        .lock()
        .unwrap()
        .get("attention")
        .map(str::to_owned);
    assert!(
        last.contains(&format!(
            " field=attention fp={:016x} by=aterm ",
            name_fp(attention.as_deref())
        )) && !last.contains("value="),
        "an attention text is said by its fingerprint: {last}"
    );
    let mut app = App::headless_for_test();
    let session = app
        .front_terminal(WindowId(0))
        .expect("a front terminal")
        .session;
    assert_eq!(
        app.write_session_meta_field(session, crate::session_timeline::MetaField::Role, "lead"),
        Ok(true)
    );
    let ctx = std::sync::Arc::clone(&app.pool.get(session).expect("pooled").ctx);
    let last = meta_marks(&ctx).pop().expect("the editor's mark");
    assert!(last.contains(" field=role value=lead by=human "), "{last}");
}

#[test]
fn the_supervisors_attention_is_one_escalated_need_by_aterm() {
    use crate::session_timeline::{MetaEdit, write_attention_owned};
    let owner = aterm_agent::supervise::run::ATTENTION_OWNER;
    let (h, store) = fabric_rig(32);
    let reply = crate::control::meta_line_for_test(
        &store,
        &h.term,
        h.local_id,
        &h.ctx,
        "set attention owner=upgrade restart",
    );
    assert_eq!(reply, "OK\n");
    assert!(
        escalated_needs(&h.ctx).is_empty(),
        "an upgrade's line is no escalation"
    );
    assert_eq!(
        write_attention_owned(
            &h.ctx,
            owner,
            MetaEdit::Set("stuck"),
            aterm_slog::Actor::Harness
        ),
        Ok(true)
    );
    assert_eq!(escalated_needs(&h.ctx), ["by=aterm src=meta conf=strong"]);
    let raised = lines_of(&h.ctx.timeline);
    assert_eq!(
        having(
            &raised,
            " mark - need word=escalated by=aterm src=meta conf=strong "
        )
        .len(),
        1,
        "{raised:?}"
    );
    let stuck = h
        .ctx
        .meta
        .lock()
        .unwrap()
        .get("attention")
        .map(str::to_owned);
    assert_eq!(
        having(
            &raised,
            &format!(
                " field=attention fp={:016x} by=harness ",
                name_fp(stuck.as_deref())
            )
        )
        .len(),
        1,
        "{raised:?}"
    );
    let reply = crate::control::meta_line_for_test(
        &store,
        &h.term,
        h.local_id,
        &h.ctx,
        &format!("set attention owner={owner} still"),
    );
    assert_eq!(reply, "OK\n");
    assert_eq!(escalated_needs(&h.ctx).len(), 1);
    assert_eq!(
        having(&lines_of(&h.ctx.timeline), " need word=escalated ").len(),
        1,
        "no second need mark"
    );
    assert!(crate::session_timeline::write_server_attention(
        &h.ctx,
        crate::input_stall::SERVER_ATTENTION_OWNER,
        Some("stalled".to_string()),
    ));
    assert!(escalated_needs(&h.ctx).is_empty(), "aterm's clear lifts it");
    let got = lines_of(&h.ctx.timeline);
    assert_eq!(
        having(
            &got,
            " mark - clear word=escalated by=aterm src=meta conf=strong "
        )
        .len(),
        1,
        "{got:?}"
    );
}

#[test]
fn the_supervisors_attention_text_never_leaves_ram() {
    use crate::session_timeline::{MetaEdit, write_attention_owned};
    let owner = aterm_agent::supervise::run::ATTENTION_OWNER;
    let (h, store) = fabric_rig(34);
    let text = "claude bash: grep -rn SENTINEL-ATTENTION ./notes (needs a person)";
    assert_eq!(
        write_attention_owned(
            &h.ctx,
            owner,
            MetaEdit::Set(text),
            aterm_slog::Actor::Harness
        ),
        Ok(true)
    );
    let stored = h
        .ctx
        .meta
        .lock()
        .unwrap()
        .get("attention")
        .map(str::to_owned);
    assert!(
        stored
            .as_deref()
            .is_some_and(|a| a.contains("SENTINEL-ATTENTION")),
        "NEGATIVE CONTROL: the store holds the text: {stored:?}"
    );
    let reply =
        crate::control::meta_line_for_test(&store, &h.term, h.local_id, &h.ctx, "set role builder");
    assert_eq!(reply, "OK\n");
    let host = lines_of(&h.ctx.timeline);
    let marks = having(&host, " mark - meta field=attention ");
    assert_eq!(marks.len(), 1, "{host:?}");
    assert!(
        marks[0].contains(&format!(" fp={:016x} ", name_fp(stored.as_deref())))
            && !marks[0].contains("value="),
        "{marks:?}"
    );
    assert_eq!(
        having(&host, " field=role value=builder ").len(),
        1,
        "NEGATIVE CONTROL: {host:?}"
    );
    let content = content_lines(&h.ctx.timeline);
    let held = having(&content, " mark - meta field=attention ");
    assert!(
        held.len() == 1
            && held[0].contains(" attn_text=")
            && held[0].contains("SENTINEL-ATTENTION"),
        "the text at the content tier, in RAM: {held:?}"
    );
    let (journal, mirror, brief, carry) = {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        let log = g.slog().log();
        let sid = aterm_slog::ShortId::new("s1").expect("an id");
        let take = aterm_slog::journal::take(log, 0, usize::MAX, true);
        let journal: Vec<String> = aterm_slog::journal::take_frames(sid, &take, true, now.wall_ms)
            .iter()
            .filter_map(aterm_slog::journal::encode_frame)
            .collect();
        assert!(
            journal.iter().any(|l| l.contains("field=attention")),
            "the journal holds the mark: {journal:?}"
        );
        let mirror: Vec<String> = log
            .window()
            .filter_map(|r| {
                aterm_slog::encode_mirror(r, aterm_slog::Tier::Content, log.ep(), None, &|_| None)
                    .ok()
            })
            .collect();
        let brief = super::super::render_brief(
            log,
            "s-1",
            aterm_slog::Node::SESSION,
            None,
            aterm_slog::DEFAULT_BUDGET,
            now.wall_ms,
        );
        let src = g.slog_mut().carry_source();
        let carry = src
            .encode(aterm_slog::CarryBudget::SESSION)
            .expect("carries")
            .lines
            .join("\n");
        g.slog_mut().lift_read_fence();
        (journal.join("\n"), mirror.join("\n"), brief, carry)
    };
    let leaks: Vec<String> = [
        ("host", host.join("\n")),
        ("journal", journal),
        ("mirror", mirror),
        ("brief", brief),
        ("carry", carry),
    ]
    .into_iter()
    .filter(|(_, got)| got.contains("SENTINEL"))
    .map(|(what, got)| format!("the attention text in the {what}: {got}"))
    .collect();
    assert!(leaks.is_empty(), "{}", leaks.join("\n"));
}

#[test]
fn a_successor_restoring_the_same_meta_marks_it_carried() {
    use crate::session_timeline::MetaChange;
    let say = |tl: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
               field: &'static str,
               value: Option<&str>| {
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        let change = MetaChange::written_by(field, value, aterm_slog::Actor::Aterm);
        g.slog_mut().observe_meta(change, false, at);
    };
    let (prev, _hand) = opened();
    say(&prev, "role", Some("SENTINEL-ROLE"));
    say(&prev, "attention", Some("SENTINEL-ATTENTION"));
    say(&prev, "icon", None);
    let tokens = prev.lock().unwrap().slog().observed.carry_tokens();
    assert!(
        tokens.contains(" meta_fp=") && !tokens.contains("SENTINEL"),
        "fingerprints only: {tokens}"
    );
    let mut read = Observed::default();
    read.read_carry(|k| {
        tokens
            .split(' ')
            .filter_map(|t| t.split_once('='))
            .find(|(key, _)| *key == k)
            .map(|(_, v)| v.to_string())
    });
    let (next, _hand2) = opened();
    next.lock().unwrap().slog_mut().observed = read;
    say(&next, "role", Some("SENTINEL-ROLE"));
    say(&next, "attention", Some("SENTINEL-ATTENTION"));
    say(&next, "icon", None);
    let metas = |tl: &std::sync::Mutex<crate::session_timeline::SessionTimeline>| {
        lines_of(tl)
            .into_iter()
            .filter(|l| l.contains(" mark - meta "))
            .collect::<Vec<_>>()
    };
    let restored = metas(&next);
    assert_eq!(restored.len(), 3, "a mark a row: {restored:?}");
    assert!(
        restored.iter().all(|l| l.contains(" carried=1 ")),
        "no news: {restored:?}"
    );
    say(&next, "role", Some("lead"));
    say(&next, "description", Some("new"));
    let got = metas(&next);
    let moved = &got[restored.len()..];
    assert_eq!(moved.len(), 2, "{got:?}");
    assert!(moved[0].contains(" field=role value=lead "), "{moved:?}");
    assert!(
        moved.iter().all(|l| !l.contains(" carried=")),
        "a move is news: {moved:?}"
    );
}

#[test]
fn the_autopilot_timer_is_in_the_meta_fingerprint() {
    use crate::session_timeline::MetaChange;
    let say = |tl: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
               land_at: Option<i64>| {
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        let change = MetaChange::written_by("autopilot", Some("cruise"), aterm_slog::Actor::Ctl)
            .with_land_at(land_at);
        g.slog_mut().observe_meta(change, false, at);
    };
    let metas = |tl: &std::sync::Mutex<crate::session_timeline::SessionTimeline>| {
        lines_of(tl)
            .into_iter()
            .filter(|l| l.contains(" mark - meta field=autopilot "))
            .collect::<Vec<_>>()
    };
    let (prev, _hand) = opened();
    say(&prev, Some(1_900_000_000_500));
    say(&prev, Some(1_900_000_060_000));
    say(&prev, Some(1_900_000_060_000));
    let got = metas(&prev);
    assert_eq!(got.len(), 3, "a mark a row: {got:?}");
    assert!(
        got[1].contains(" land_at=1900000060 ") && !got[1].contains(" carried="),
        "a timer-only move is a move: {got:?}"
    );
    assert!(got[2].contains(" carried=1 "), "the same pair: {got:?}");
    let tokens = prev.lock().unwrap().slog().observed.carry_tokens();
    let mut read = Observed::default();
    read.read_carry(|k| {
        tokens
            .split(' ')
            .filter_map(|t| t.split_once('='))
            .find(|(key, _)| *key == k)
            .map(|(_, v)| v.to_string())
    });
    let (next, _hand2) = opened();
    next.lock().unwrap().slog_mut().observed = read;
    say(&next, Some(1_900_000_060_000));
    say(&next, Some(1_900_000_120_000));
    let got = metas(&next);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(
        got[0].contains(" carried=1 "),
        "the carried pair is no news: {got:?}"
    );
    assert!(
        !got[1].contains(" carried="),
        "another timer is a move: {got:?}"
    );
}

#[test]
fn a_proven_claim_the_sweep_clears_leaves_the_need_consistent() {
    use crate::session_timeline::{MetaEdit, claim_supervisor_as, release_proven_if_gone};
    let owner = aterm_agent::supervise::run::ATTENTION_OWNER;
    let (h, _store) = fabric_rig(33);
    let _ = crate::session_timeline::write_attention_owned(
        &h.ctx,
        owner,
        MetaEdit::Set("stuck"),
        aterm_slog::Actor::Harness,
    );
    let proven = crate::self_claim::stand_in::running();
    let pid = proven.pid;
    let now = crate::metrics::now_us();
    assert!(
        claim_supervisor_as(
            &h.ctx,
            "agent@1",
            Some(1),
            None,
            Some(proven),
            now,
            aterm_slog::Actor::Ctl
        )
        .is_ok()
    );
    let mine = i32::try_from(pid).expect("a pid");
    assert!(
        release_proven_if_gone(&h.ctx, mine + 1, now),
        "another group leads"
    );
    let last = meta_marks(&h.ctx).pop().expect("the release's mark");
    assert!(
        last.contains(" mark - meta field=supervisor by=aterm ") && !last.contains("value="),
        "{last}"
    );
    assert_eq!(escalated_needs(&h.ctx), ["by=aterm src=meta conf=strong"]);
    let got = lines_of(&h.ctx.timeline);
    assert_eq!(having(&got, " word=escalated ").len(), 1, "{got:?}");
    let tokens = h
        .ctx
        .timeline
        .lock()
        .unwrap()
        .slog()
        .observed
        .carry_tokens();
    assert!(tokens.contains(" obs_esc=1"), "{tokens}");
    let mut read = Observed::default();
    read.read_carry(|k| (k == "obs_esc").then(|| "1".to_string()));
    let (next, _hand) = opened();
    let mut n = next.lock().unwrap();
    n.slog_mut().observed = read;
    let at = n.slog().now();
    let change = crate::session_timeline::MetaChange::written_by(
        "attention",
        Some("stuck"),
        aterm_slog::Actor::Aterm,
    );
    n.slog_mut().observe_meta(change, true, at);
    let got = lines(n.slog().log());
    assert!(
        having(&got, " word=escalated ").is_empty(),
        "restated: {got:?}"
    );
}

#[test]
fn a_told_story_is_one_mark_by_its_teller_and_no_text() {
    let mut app = App::headless_for_test();
    let session = app
        .front_terminal(WindowId(0))
        .expect("a front terminal")
        .session;
    let ctx = std::sync::Arc::clone(&app.pool.get(session).expect("pooled").ctx);
    let _ = ctx.timeline.lock().unwrap().slog_take_wake();
    let sub = watching(&app, session);
    let told = app.tell_story_by(
        session,
        aterm_messages::presence::StoryVerb::Chose,
        aterm_slog::Actor::Harness,
    );
    assert!(told.is_ok(), "{told:?}");
    assert!(
        sub.wait(aterm_spec::wait::MUST_HAPPEN),
        "the story woke the watchers"
    );
    let got = content_lines(&ctx.timeline);
    assert_eq!(
        having(
            &got,
            " mark - story word=chose by=harness src=ctl conf=exact "
        )
        .len(),
        1,
        "{got:?}"
    );
    assert!(got.iter().all(|l| !l.contains("SENTINEL")), "{got:?}");
}

#[test]
#[should_panic(expected = "record_meta_change's")]
fn a_meta_change_has_one_writer() {
    let mut tl = crate::session_timeline::SessionTimeline::default();
    tl.record(crate::session_timeline::META_CHANGE);
}

fn failures_and_harness_seen(
    tl: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
) -> (usize, usize) {
    let g = tl.lock().unwrap();
    let failures = g
        .slog()
        .log()
        .state()
        .needs
        .iter()
        .flatten()
        .filter(|n| n.word == "failure")
        .count();
    let seen = lines(g.slog().log())
        .iter()
        .filter(|l| l.contains(" mark - seen by=harness src=aterm conf=strong "))
        .count();
    (failures, seen)
}

fn relaunch_at(
    tl: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
    hand: &super::super::tests::Hand,
    ms: u64,
) {
    hand.set(ms);
    let mut g = tl.lock().unwrap();
    let now = g.slog().now();
    g.slog_mut().observe_relaunch_act("relaunch", true, now);
}

fn claude_in_front(
    tl: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
    hand: &super::super::tests::Hand,
    ms: u64,
    (blk, start, pgid): (u64, u64, i32),
) {
    hand.set(ms);
    let mut g = tl.lock().unwrap();
    assert!(g.note_foreground_group(pgid));
    assert!(g.set_program(pgid, Some("claude".into())));
    let _ = g.sweep_slog(&running(blk, start, "claude"), false, 100, pgid);
    assert!(
        lines(g.slog().log())
            .iter()
            .any(|l| l.contains(" agent par=") && l.contains(" prog=claude agent=claude ")),
        "the agent's run opened: {:?}",
        lines(g.slog().log())
    );
}

#[test]
fn a_relaunch_lifts_the_failure_it_caused() {
    let (tl, hand) = opened();
    blocks(&tl, &hand, 150, &running(0, 100, "claude"));
    claude_in_front(&tl, &hand, 160, (0, 100, 300));
    blocks(&tl, &hand, 400, &finished(0, Some(143), 100, 300, "claude"));
    assert_eq!(failures_and_harness_seen(&tl), (1, 0), "PRECONDITION");
    blocks(&tl, &hand, 600, &running(1, 500, "claude"));
    claude_in_front(&tl, &hand, 650, (1, 500, 301));
    relaunch_at(&tl, &hand, 700);
    assert_eq!(
        failures_and_harness_seen(&tl),
        (0, 1),
        "{:?}",
        lines_of(&tl)
    );
    let got = lines_of(&tl);
    let tail: Vec<&str> = got
        .iter()
        .rev()
        .take(2)
        .rev()
        .map(|l| l.split_once(' ').map_or("", |(_, b)| b))
        .collect();
    assert_eq!(
        tail,
        [
            format!(
                "mark - act word=relaunch rule=relaunch by=harness src=aterm conf=strong t={}",
                super::super::tests::WALL0 + 700
            ),
            format!(
                "mark - seen by=harness src=aterm conf=strong t={}",
                super::super::tests::WALL0 + 700
            ),
        ],
        "the act, then the seen: {got:?}"
    );
    let brief = crate::session_log::render_brief(
        tl.lock().unwrap().slog().log(),
        "s-x",
        Node::SESSION,
        None,
        4000,
        super::super::tests::WALL0 + 800,
    );
    assert!(!brief.contains("need=failure"), "{brief}");
}

#[test]
fn a_relaunch_with_no_newer_run_open_lifts_nothing() {
    let (tl, hand) = opened();
    blocks(&tl, &hand, 150, &running(0, 100, "claude"));
    claude_in_front(&tl, &hand, 160, (0, 100, 300));
    relaunch_at(&tl, &hand, 200);
    assert_eq!(failures_and_harness_seen(&tl), (0, 0), "nothing to lift");
    blocks(&tl, &hand, 400, &finished(0, Some(143), 100, 300, "claude"));
    assert_eq!(
        failures_and_harness_seen(&tl),
        (1, 0),
        "the 143 close that follows raises the need"
    );
    relaunch_at(&tl, &hand, 500);
    assert_eq!(
        failures_and_harness_seen(&tl),
        (1, 0),
        "no newer run open: {:?}",
        lines_of(&tl)
    );
}

#[test]
fn a_relaunch_beside_a_second_failure_lifts_neither() {
    use aterm_slog::{Close, Open, Out, RunOut};
    let (tl, hand) = opened();
    blocks(&tl, &hand, 150, &running(0, 100, "claude"));
    claude_in_front(&tl, &hand, 160, (0, 100, 300));
    blocks(&tl, &hand, 400, &finished(0, Some(143), 100, 300, "claude"));
    {
        let mut g = tl.lock().unwrap();
        let log = g.slog_mut().log_mut_for_tests();
        let prog = log
            .open(
                Open::new(Kind::Prog, Src::Status, Conf::Strong),
                super::super::tests::at(410),
            )
            .expect("a program run");
        log.close(
            prog,
            Close::new(Out::Run(RunOut::Fail), Src::Status, Conf::Exact),
            super::super::tests::at(420),
        )
        .expect("it fails");
    }
    assert_eq!(failures_and_harness_seen(&tl), (2, 0), "PRECONDITION");
    blocks(&tl, &hand, 600, &running(1, 500, "claude"));
    relaunch_at(&tl, &hand, 700);
    assert_eq!(
        failures_and_harness_seen(&tl),
        (2, 0),
        "both failures stand: {:?}",
        lines_of(&tl)
    );
}

#[test]
fn a_restart_lifts_no_failure_of_a_command_that_ran_no_agent() {
    let (tl, hand) = opened();
    blocks(&tl, &hand, 150, &running(0, 100, "make"));
    blocks(&tl, &hand, 400, &finished(0, Some(2), 100, 300, "make"));
    assert_eq!(failures_and_harness_seen(&tl), (1, 0), "PRECONDITION");
    blocks(&tl, &hand, 600, &running(1, 500, "claude"));
    claude_in_front(&tl, &hand, 650, (1, 500, 300));
    relaunch_at(&tl, &hand, 700);
    assert_eq!(
        failures_and_harness_seen(&tl),
        (1, 0),
        "make's failure stands: {:?}",
        lines_of(&tl)
    );
    assert!(
        lines_of(&tl)
            .iter()
            .any(|l| l.contains(" mark - act word=relaunch rule=relaunch by=harness ")),
        "the act is still written: {:?}",
        lines_of(&tl)
    );
}
