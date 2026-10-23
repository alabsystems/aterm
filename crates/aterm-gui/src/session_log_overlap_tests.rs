// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Overlap tests between the session log, timeline ring, roster exits, and presence story.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_messages::presence::StoryVerb;
use aterm_slog::{Actor, Log, Op, Rec, ShortId, Tier};

use super::super::tests::{hand_over, stamp};
use super::*;
use crate::session_store::{
    CloseAttribution, ExitActor, ExitReason, RosterChange, SessionHandle, SessionState, Store,
    new_store, test_handle,
};
use crate::session_timeline::SessionTimeline;
use crate::{App, WindowId};

/// The ring kinds the log equals row for record, the whole payload (T19's
const EXACT: [&str; 24] = [
    "agent-change",
    crate::session_status::STATUS_CHANGE,
    "inbox",
    "task-change",
    "state-change",
    crate::harness_policy_verb::HARNESS_POLICY_EVENT,
    "hold-advisory",
    "fetch",
    "topic",
    "inbox-seen",
    "post",
    "post-landed",
    "hold",
    "spawned",
    "closing",
    "render",
    "modes-restored",
    "in-doubt",
    "handoff",
    crate::session_timeline::HARNESS_ACT,
    crate::session_timeline::TITLE_CHANGE,
    crate::session_timeline::CWD_CHANGE,
    crate::session_timeline::META_CHANGE,
    "human",
];

/// The exact pairs 5B-6's scripted session writes
const SCRIPTED: [&str; 7] = [
    "agent-change",
    crate::session_status::STATUS_CHANGE,
    "inbox",
    "task-change",
    crate::session_timeline::HARNESS_ACT,
    crate::session_timeline::META_CHANGE,
    "human",
];

/// Every timeline kind of plan §0.1.
const SECTION_0_1: [&str; 25] = [
    "spawned",
    "closing",
    "state-change",
    "title-change",
    "cwd-change",
    "meta-change",
    "agent-change",
    "task-change",
    crate::session_status::STATUS_CHANGE,
    "human",
    "render",
    "modes-restored",
    "in-doubt",
    "handoff",
    "harness-act",
    "program-state",
    crate::harness_policy_verb::HARNESS_POLICY_EVENT,
    "inbox",
    "inbox-seen",
    "post",
    "post-landed",
    "hold",
    "fetch",
    "topic",
    "hold-advisory",
];

/// One record as the ring rows it pairs with: the view's grammar
fn project(r: &Rec) -> Vec<(String, String)> {
    crate::session_timeline_view::rows_of(r)
        .into_iter()
        .map(|(k, p)| {
            (
                k.to_string(),
                crate::session_timeline::clamp_payload(p).to_string(),
            )
        })
        .collect()
}

/// THE VIEW's rows ([`crate::session_timeline_view::timeline_rows`], 6G-2)
pub(crate) fn overlap_rows(log: &Log) -> Vec<(String, String)> {
    crate::session_timeline_view::timeline_rows(log)
        .into_iter()
        .map(|e| (e.kind.to_string(), e.payload.to_string()))
        .collect()
}

/// The view's grammar over every record of `log`, `drop` taken out of each
fn overlap_rows_without(log: &Log, drop: Option<Key>) -> Vec<(String, String)> {
    projected(log.window(), drop)
}

/// `recs` projected ([`project`]), `drop` taken out of every record first.
fn projected<'a>(
    recs: impl IntoIterator<Item = &'a Rec>,
    drop: Option<Key>,
) -> Vec<(String, String)> {
    recs.into_iter()
        .flat_map(|r| {
            let mut r = r.clone();
            if let Some(k) = drop {
                r.f.remove(k);
            }
            project(&r)
        })
        .collect()
}

/// `rows` of `kinds` alone.
fn only(rows: Vec<(String, String)>, kinds: &[&str]) -> Vec<(String, String)> {
    rows.into_iter()
        .filter(|(k, _)| kinds.contains(&k.as_str()))
        .collect()
}

/// The ring's rows of `kinds`, oldest first, as `(kind, payload)`: the
fn ring_rows(tl: &SessionTimeline, kinds: &[&str]) -> Vec<(String, String)> {
    tl.since(None)
        .filter(|e| kinds.contains(&e.kind))
        .map(|e| (e.kind.to_string(), e.payload.to_string()))
        .collect()
}

/// Rows as the ring prints them, `(kind, payload)`, oldest first.
#[cfg(unix)]
pub(crate) type Rows = Vec<(String, String)>;

/// The log's view of the exact pairs and the ring's rows of them, oldest
#[cfg(unix)]
pub(crate) fn exact_view_and_ring(tl: &SessionTimeline) -> (Rows, Rows) {
    (
        only(overlap_rows(tl.slog().log()), &EXACT),
        ring_rows(tl, &EXACT),
    )
}

/// THE EXACT PAIRS AGREE (T19): over one scripted session, through each
#[test]
fn the_ring_and_the_log_agree_on_the_exact_pairs() {
    let mut app = App::headless_for_test();
    let local = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(local));
    let pooled = app.pool.get(local).expect("pooled");
    let (ctx, term) = (Arc::clone(&pooled.ctx), Arc::clone(&pooled.term));
    crate::term_lock(&term).process(
        b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\false\r\n\x1b]133;C\x1b\\\
          \x1b]133;D;1\x1b\\\x1b]133;A\x1b\\\x1b]133;B\x1b\\",
    );
    let t0 = Instant::now();
    for i in 0..4 {
        app.observe_session_statuses(t0 + Duration::from_secs(i));
    }
    {
        let mut g = ctx.timeline.lock().unwrap();
        let claude = Some(aterm_phase::Program::Claude);
        g.publish_agent("busy", None, None, claude, stamp(1));
        g.publish_agent("prompt", Some("bash".into()), None, claude, stamp(2));
        g.publish_agent("question", None, None, claude, stamp(3));
        g.publish_agent("idle", None, None, claude, stamp(4));
    }
    let _ = crate::app_input::note_person(&ctx, crate::human_input::Gesture::Other);
    let _ = crate::app_input::note_person(&ctx, crate::human_input::Gesture::PlainEnter);
    let sid = ctx.self_id.as_str().to_string();
    let wake = |_: &aterm_session::SessionId| {};
    for line in [
        format!("{sid} off=7 from=h-andrew kind=task trust=human text=SENTINEL%20BODY"),
        format!("{sid} off=7 from=h-andrew kind=task trust=human text=SENTINEL%20BODY"),
        format!("{sid} off=8 from=s-peer@n-lab kind=ask trust=agent text=SENTINEL"),
    ] {
        let reply = crate::fabric::cmd_deliver_waking(&app.store, &line, &wake);
        assert!(reply.starts_with("OK "), "{reply}");
    }
    for args in [
        "working",
        "done why=marker",
        "done why=marker",
        "exited why=exit exit=2",
    ] {
        let reply = crate::task_record::cmd_task(
            &ctx,
            local,
            crate::control::Scope::Owner,
            &format!("mark {args}"),
        );
        assert!(reply.starts_with("OK task "), "{reply}");
    }
    let raised = crate::session_timeline::write_attention_owned(
        &ctx,
        crate::harness_host::ATTENTION_OWNER,
        crate::session_timeline::MetaEdit::Set("look here"),
        Actor::Harness,
    );
    assert_eq!(raised, Ok(true));
    {
        let mut g = ctx.timeline.lock().unwrap();
        for (act, by) in [
            ("approve-all@v1", Actor::Ctl),
            ("finished@v1", Actor::Ctl),
            ("usage-resume@v1", Actor::Harness),
        ] {
            let _ = g.note_harness_act(act, "codex-alab@1", by);
        }
    }
    crate::term_lock(&term).process(b"true\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
    for i in 4..8 {
        app.observe_session_statuses(t0 + Duration::from_secs(i));
    }
    let g = ctx.timeline.lock().unwrap();
    let ring = ring_rows(&g, &EXACT);
    for kind in SCRIPTED {
        assert!(
            ring.iter().any(|(k, _)| k == kind),
            "the script wrote no {kind} row: {ring:?}"
        );
    }
    let log = only(overlap_rows(g.slog().log()), &EXACT);
    assert_eq!(log, ring, "the log's exact pairs against the ring's rows");
    let acts = |words: &[&str]| {
        g.slog()
            .log()
            .window()
            .filter(|r| r.k == Kind::Act && r.src == aterm_slog::Src::Mark)
            .filter(|r| words.contains(&r.f.get_word(Key::Word).unwrap_or("-")))
            .count()
    };
    assert_eq!(acts(&["approve", "continue"]), 2, "two worded acts");
    assert_eq!(acts(&["unknown"]), 1, "finished@v1 is a record too");
    for key in [
        Key::Fp,
        Key::Rev,
        Key::HarnessAct,
        Key::Holder,
        Key::AttnText,
    ] {
        assert_ne!(
            only(overlap_rows_without(g.slog().log(), Some(key)), &EXACT),
            ring,
            "a projection with {key:?} dropped agrees"
        );
    }
}

/// A person's burst, a delivered mail, then the same person typing on
fn drive_human_at(local: u64) -> (SessionHandle, Seen) {
    let (h, store) = rig(local);
    let hand = super::super::tests::hand_clock(&h.ctx.timeline);
    let seen = during(&h.ctx.timeline, || {
        let ctx = &h.ctx;
        let _ = crate::app_input::note_person(ctx, crate::human_input::Gesture::Other);
        let _ = crate::app_input::note_person(ctx, crate::human_input::Gesture::PlainEnter);
        let line = format!(
            "{} off=9 from=h-andrew kind=task trust=human text=SENTINEL",
            ctx.self_id.as_str()
        );
        let reply = crate::fabric::cmd_deliver(&store, &line);
        assert!(reply.starts_with("OK "), "{reply}");
        let _ = crate::app_input::note_person(ctx, crate::human_input::Gesture::Other);
        let _ = crate::app_input::note_person(ctx, crate::human_input::Gesture::PlainEnter);
    });
    drop(hand);
    (h, seen)
}

fn drive_human() -> Seen {
    drive_human_at(62).1
}

/// THE `human` ROWS ARE THE BURST MARKS (T39): a
#[test]
fn the_human_rows_are_the_burst_marks_and_the_latchs_own_says_why() {
    let (h, _seen) = drive_human_at(61);
    let g = h.ctx.timeline.lock().unwrap();
    let human = ["human"];
    let ring = ring_rows(&g, &human);
    assert_eq!(ring.len(), 1, "the ring keeps the burst rule: {ring:?}");
    let log = g.slog().log();
    let marks: Vec<String> = log
        .window()
        .filter(|r| r.op == Op::Mark && matches!(r.k, Kind::Human | Kind::Mail))
        .map(|r| match r.f.get_word(Key::Why) {
            Some(why) => format!("{} why={why}", r.k.word()),
            None => r.k.word().to_string(),
        })
        .collect();
    assert_eq!(
        marks,
        ["human", "mail", "human why=mail"],
        "the person's first input after the mail is the latch's own mark"
    );
    assert_eq!(
        only(overlap_rows(log), &human),
        ring,
        "the view's human rows are the ring's"
    );
    let fold = |drop_why: bool| {
        let mut fold = aterm_slog::Fold::default();
        for r in log.window() {
            let mut r = r.clone();
            if drop_why && r.k == Kind::Human {
                r.f.remove(Key::Why);
            }
            fold.apply(&r);
        }
        fold
    };
    assert_eq!(fold(false), fold(true), "the fold reads no why=");
    assert!(
        !fold(false).in_since_human(),
        "the latch's mark answered the mail"
    );
    assert_ne!(
        only(overlap_rows_without(log, Some(Key::Why)), &human),
        ring,
        "NEGATIVE CONTROL: without why= the view shows a row the ring never had"
    );
}

/// How a timeline kind and the log differ (T19's named differences; the lossy rows and the rows on another trigger are
#[derive(Clone, Copy, Debug)]
enum Diff {
    /// A record (`<op> <kind>`) for every row the log keeps, which it keeps
    Retention(&'static str, &'static str),
}

/// A `program` record line as the `program-state` row it keeps: `state=<w>
fn retained_row(line: &str) -> String {
    let state = match value(line, "state") {
        Some("none") | None => "-",
        Some(w) => w,
    };
    format!("state={state} kind={}", value(line, "ask").unwrap_or("-"))
}

/// One named difference.
struct Known {
    /// The ring's kind.
    row: &'static str,
    diff: Diff,
    /// Why (a ruling or the design's rule).
    why: &'static str,
    /// The step that closes it, if one does.
    until: Option<&'static str>,
    /// The row's own writer, driven; `None` for one no unit test reaches.
    drive: Option<fn() -> Seen>,
}

/// Every §0.1 kind that is no exact pair, and how it differs.
const KNOWN_DIFFERENCES: &[Known] = &[Known {
    row: "program-state",
    diff: Diff::Retention(
        "mark program",
        "64 a run (6F-3); a flap past the bucket coalesced to its latest state",
    ),
    why: "U39: the ring keeps 256 rows a session (PROGRAM_STATE_ROWS_MAX), the log 64 \
              program marks a run (T6's share), and a move past the run's bucket is coalesced \
              into the next mark (U17, U44)",
    until: None,
    drive: Some(drive_program),
}];

/// What one driven call wrote: the ring's rows, and the log's records at
#[derive(Clone, Debug, Default)]
struct Seen {
    rows: Vec<(String, String)>,
    recs: Vec<String>,
    log: Vec<Rec>,
}

impl Seen {
    /// `self`, then what `more` wrote.
    fn extend(&mut self, more: Seen) {
        self.rows.extend(more.rows);
        self.recs.extend(more.recs);
        self.log.extend(more.log);
    }
}

/// What `f` wrote on `tl`.
fn during(tl: &Mutex<SessionTimeline>, f: impl FnOnce()) -> Seen {
    let (id, seq) = {
        let g = tl.lock().unwrap();
        (g.high_id(), g.slog().log().last_seq())
    };
    f();
    seen_since(tl, id, seq)
}

/// What `tl` holds past ring id `id` and log seq `seq`.
fn seen_since(tl: &Mutex<SessionTimeline>, id: Option<u64>, seq: u64) -> Seen {
    let g = tl.lock().unwrap();
    Seen {
        rows: g
            .since(id)
            .map(|e| (e.kind.to_string(), e.payload.to_string()))
            .collect(),
        recs: g
            .slog()
            .log()
            .window()
            .filter(|r| r.seq > seq)
            .map(|r| aterm_slog::encode(r, Tier::Content).expect("fits"))
            .collect(),
        log: g
            .slog()
            .log()
            .window()
            .filter(|r| r.seq > seq)
            .cloned()
            .collect(),
    }
}

/// A registered session and its store.
fn rig(local: u64) -> (SessionHandle, Store) {
    let h = test_handle(local);
    let store = new_store();
    store.write().unwrap().register(h.clone());
    (h, store)
}

/// A record line's `<op> <kind>`.
fn op_kind(line: &str) -> String {
    let t: Vec<&str> = line.split(' ').collect();
    format!(
        "{} {}",
        t.get(1).copied().unwrap_or(""),
        t.get(3).copied().unwrap_or("")
    )
}

/// The value of `key=` in a space-separated line.
fn value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(' ')
        .find_map(|t| t.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
}

/// Whether the named difference `k` still holds over `seen`.
fn holds(k: &Known, seen: &Seen) -> Result<(), String> {
    let rows: Vec<&String> = seen
        .rows
        .iter()
        .filter(|(kind, _)| kind == k.row)
        .map(|(_, p)| p)
        .collect();
    if rows.is_empty() {
        return Err(format!(
            "{}: the driver wrote no row: {:?}",
            k.row, seen.rows
        ));
    }
    let of =
        |sel: &str| -> Vec<&String> { seen.recs.iter().filter(|l| op_kind(l) == sel).collect() };
    match k.diff {
        Diff::Retention(sel, rule) => {
            let recs = of(sel);
            let (Some(newest_rec), Some(newest_row)) = (recs.last(), rows.last()) else {
                return Err(format!("{}: no {sel} record: {:?}", k.row, seen.recs));
            };
            if recs.len() > rows.len() {
                return Err(format!(
                    "{}: {} records for {} rows ({rule})",
                    k.row,
                    recs.len(),
                    rows.len()
                ));
            }
            if retained_row(newest_rec) != **newest_row {
                return Err(format!(
                    "{}: the newest record {newest_rec} is not the newest row {newest_row} \
                     ({rule})",
                    k.row
                ));
            }
            Ok(())
        }
    }
}

fn drive_spawned() -> Seen {
    let h = test_handle(41);
    let store = new_store();
    during(&h.ctx.timeline, || {
        store.write().unwrap().register(h.clone())
    })
}

fn drive_closing() -> Seen {
    let (h, store) = rig(42);
    during(&h.ctx.timeline, || {
        let _ = store.write().unwrap().deregister_local_as(
            42,
            ExitReason::CtlClose,
            ExitActor::Sid("s-driver".into()),
        );
    })
}

/// The registry's every state move: back to spawning (a test handle is
fn drive_state() -> Seen {
    let (h, store) = rig(43);
    during(&h.ctx.timeline, || {
        store.write().unwrap().set_state(43, SessionState::Spawning);
        assert!(store.write().unwrap().mark_alive(43));
        store.write().unwrap().set_state(43, SessionState::Exited);
        let _ =
            store
                .write()
                .unwrap()
                .deregister_local_as(43, ExitReason::Unknown, ExitActor::Unknown);
    })
}

/// What the drift flush (`App::observe_title_drift`, the production path
fn drifted(reports: &[&[u8]]) -> Seen {
    let mut app = App::headless_for_test();
    let session = app
        .front_terminal(WindowId(0))
        .expect("a front terminal")
        .session;
    let pooled = app.pool.get(session).expect("pooled");
    let (ctx, term) = (Arc::clone(&pooled.ctx), Arc::clone(&pooled.term));
    let t0 = Instant::now();
    app.observe_title_drift(session, t0);
    let step = crate::DRIFT_REFRESH_MIN_INTERVAL + Duration::from_millis(1);
    during(&ctx.timeline, || {
        let mut at = t0;
        for report in reports {
            crate::term_lock(&term).process(report);
            at += step;
            app.observe_title_drift(session, at);
        }
    })
}

/// A title, a spinner frame over it, another frame (no move: no row, no
fn drive_title() -> Seen {
    drifted(&[
        b"\x1b]2;vim build\x07",
        "\x1b]2;\u{280b} vim build\x07".as_bytes(),
        "\x1b]2;\u{2819} vim build\x07".as_bytes(),
        b"\x1b]2;make test\x07",
        b"\x1b]2;\x07",
    ])
}

/// A TITLE AT ITS PRODUCER'S BOUNDS (6F-8): longer than the mark's
fn drive_title_at_its_bounds() -> Seen {
    let (h, _store) = rig(71);
    let long = format!("vim {}end", "t\u{e9} ".repeat(80));
    during(&h.ctx.timeline, || {
        let mut g = h.ctx.timeline.lock().unwrap();
        assert!(g.record_title(&long));
        assert!(g.record_title("vim\u{200b} hidden"));
    })
}

/// Two directories, the same one again (no move), through the drift flush.
fn drive_cwd() -> Seen {
    drifted(&[
        b"\x1b]7;file://localhost/tmp/a%20b\x07",
        b"\x1b]7;file://localhost/tmp/c\x07",
        b"\x1b]7;file://localhost/tmp/c\x07",
    ])
}

/// A seamless update's successor reading the directory its predecessor
fn drive_cwd_successor() -> Seen {
    let (h, store) = rig(45);
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .record_cwd_change(Some("/tmp/a b"));
    let (next, _next_store, _hand) = hand_over(&h, &store.read().unwrap(), 1_000, true);
    during(&next.ctx.timeline, || {
        let mut g = next.ctx.timeline.lock().unwrap();
        g.record_cwd_change(Some("/tmp/a b"));
        g.record_cwd_change(Some("/tmp/c"));
        g.record_cwd_change(None);
    })
}

/// A DIRECTORY AT ITS PRODUCER'S BOUNDS (6F-8): longer than the mark's
fn drive_cwd_at_its_bounds() -> Seen {
    let (h, _store) = rig(72);
    let long = format!("/tmp/{}end", "d \u{e9}/".repeat(80));
    during(&h.ctx.timeline, || {
        let mut g = h.ctx.timeline.lock().unwrap();
        g.record_cwd_change(Some(&long));
        g.record_cwd_change(Some("/tmp/hidden\u{200b}dir"));
    })
}

/// `lines` through the `meta` verb on `h`, each answered `OK`.
fn meta_lines(h: &SessionHandle, store: &Store, lines: &[&str]) {
    for line in lines {
        let reply = crate::control::meta_line_for_test(store, &h.term, h.local_id, &h.ctx, line);
        assert_eq!(reply, "OK\n", "{line}");
    }
}

/// The attention through the `meta` verb (set, another text, unset: its
fn drive_meta() -> Seen {
    let (h, store) = rig(46);
    during(&h.ctx.timeline, || {
        meta_lines(
            &h,
            &store,
            &[
                "set attention look here",
                "set attention look again",
                "unset attention",
                "set role builder",
            ],
        );
    })
}

/// A META VALUE AT ITS PRODUCER'S BOUNDS (6F-8): the longest description
fn drive_meta_at_its_bounds() -> Seen {
    let (h, store) = rig(73);
    let unit = "d\u{e9} ";
    let fill = |max: usize| format!("{}end", unit.repeat((max - 3) / unit.len()));
    let description = fill(crate::session_timeline::META_DESCRIPTION_MAX);
    let attention = fill(crate::session_timeline::META_ATTENTION_MAX);
    during(&h.ctx.timeline, || {
        meta_lines(
            &h,
            &store,
            &[
                &format!("set description {description}"),
                &format!("set attention {attention}"),
            ],
        );
    })
}

/// The Autopilot pair through its writer (`apply_autopilot`, the `meta`
fn drive_autopilot() -> Seen {
    let (h, _store) = rig(74);
    during(&h.ctx.timeline, || {
        for (word, land_at) in [
            ("cruise", Some(1_900_000_000_500)),
            ("cruise", Some(1_900_000_060_000)),
            ("cruise", None),
            ("off", None),
        ] {
            let w = crate::session_timeline::apply_autopilot(
                &h.ctx,
                Some(word.to_string()),
                land_at,
                Actor::Ctl,
                None,
            );
            assert!(w.moved, "{word} {land_at:?}");
        }
    })
}

/// A seamless update's successor restoring what its predecessor held (the
fn drive_meta_successor() -> Seen {
    use crate::session_timeline::{MetaEdit, MetaField, write_attention_owned};
    let (h, store) = rig(75);
    meta_lines(&h, &store, &["set description build", "set role builder"]);
    assert_eq!(
        write_attention_owned(
            &h.ctx,
            crate::harness_host::ATTENTION_OWNER,
            MetaEdit::Set("look here"),
            Actor::Harness,
        ),
        Ok(true)
    );
    let (carried, rows) = {
        let meta = h.ctx.meta.lock().unwrap();
        (meta.sanitized(), meta.handoff_attention_owners())
    };
    let (next, _next_store, _hand) = hand_over(&h, &store.read().unwrap(), 1_000, true);
    during(&next.ctx.timeline, || {
        assert!(crate::session_timeline::restore_carried_meta(
            &next.ctx, &carried
        ));
        crate::session_timeline::seed_carried_claims(
            &next.ctx.meta,
            &next.ctx.timeline,
            None,
            &rows,
            crate::metrics::now_us(),
        );
        assert!(crate::session_timeline::apply_meta_value(
            &next.ctx,
            MetaField::Role,
            Some("lead".to_string()),
            Actor::Ctl,
        ));
    })
}

/// Every kind of render transition: a run's (into the risk, signed, and
fn drive_render() -> Seen {
    use crate::resize_epoch::{EpochBy, ProgramClass};
    use crate::resize_ledger::Transition as T;
    let (h, _store) = rig(48);
    during(&h.ctx.timeline, || {
        let _ = h.ctx.timeline.lock().unwrap().record_render(&[
            T::Risk {
                run: 1,
                displaced: 2,
                net_zero: true,
                at_ms: 3,
            },
            T::Healed {
                run: 1,
                after_ms: 40,
            },
            T::Risk {
                run: 2,
                displaced: -3,
                net_zero: false,
                at_ms: 50,
            },
            T::EpochRisk {
                epoch: 4,
                shift: -2,
                pushed: 1,
                cut: 6,
                drew: true,
                class: ProgramClass::Learned,
                at_ms: 70,
            },
            T::EpochHealed {
                epoch: 4,
                after_ms: 80,
                by: EpochBy::Undo,
            },
            T::EpochHealed {
                epoch: 5,
                after_ms: 90,
                by: EpochBy::None,
            },
        ]);
    })
}

/// A person's reset and a verb's that reverted nothing, then a handback
fn drive_modes() -> Seen {
    use crate::manual_reset::{ResetReport, ResetSource};
    let (h, _store) = rig(49);
    during(&h.ctx.timeline, || {
        for (source, reverted, bytes) in [
            (ResetSource::Menu, vec!["mouse", "paste"], 12),
            (ResetSource::Verb, vec![], 0),
        ] {
            crate::manual_reset::record(
                49,
                &h.ctx.timeline,
                source,
                &ResetReport { reverted, bytes },
            );
        }
        for (reverted, bytes) in [(&["alt", "kitty"][..], 42), (&[][..], 0)] {
            let _ = crate::foreground_handback::record_restored(
                &mut h.ctx.timeline.lock().unwrap(),
                crate::foreground_handback::FgEdge::group(0, 300, 301),
                reverted,
                bytes,
            );
        }
    })
}

/// Two keyed writes in doubt, one with no reply at all.
fn drive_in_doubt() -> Seen {
    let (h, _store) = rig(50);
    let key = |seq: u64| {
        crate::pty_idem::parse_key(&format!("{}:3:{seq}", h.ctx.nonce.to_hex()), h.ctx.nonce)
            .expect("a key")
    };
    during(&h.ctx.timeline, || {
        let _ = crate::pty_idem::record_in_doubt(&h.ctx, "send", key(9), "ERR write failed\n");
        let _ = crate::pty_idem::record_in_doubt(&h.ctx, "paste", key(10), "");
    })
}

/// A seamless update's successor, carried and not (each past the
fn drive_handoff() -> Seen {
    let mut all = Seen::default();
    for carry in [true, false] {
        let (h, store) = rig(51);
        h.ctx
            .timeline
            .lock()
            .unwrap()
            .record_cwd_change(Some("/tmp"));
        let head = h.ctx.timeline.lock().unwrap().slog().log().last_seq();
        let (next, _next_store, _hand) = hand_over(&h, &store.read().unwrap(), 1_000, carry);
        all.extend(seen_since(&next.ctx.timeline, None, head));
    }
    let kept = test_handle(65);
    crate::spawn::mark_handoff_gap(
        &kept.ctx.timeline,
        &kept.ctx.cast,
        &kept.ctx.temporal,
        Some(9),
        0,
        crate::session_log::LogAdopt::default(),
        true,
    );
    new_store().write().unwrap().register(kept.clone());
    all.extend(seen_since(&kept.ctx.timeline, None, 0));
    all
}

/// A program's report, as the gate shows it.
fn program_report(
    state: aterm_core::terminal::program_state::ProgramStateWord,
    kind: Option<aterm_core::terminal::program_state::ProgramBlockedKind>,
) -> aterm_core::terminal::program_state::ProgramState {
    aterm_core::terminal::program_state::ProgramState { state, kind }
}

fn drive_program() -> Seen {
    use aterm_core::terminal::program_state::{ProgramBlockedKind, ProgramStateWord};
    let (h, _store) = rig(53);
    during(&h.ctx.timeline, || {
        let mut g = h.ctx.timeline.lock().unwrap();
        let _ = g.note_program_state(Some(program_report(ProgramStateWord::Working, None)));
        let _ = g.note_program_state(Some(program_report(
            ProgramStateWord::Blocked,
            Some(ProgramBlockedKind::Permission),
        )));
        let _ = g.note_program_state(None);
    })
}

/// THE PROGRAM MARKS KEEP THE ROWS UNDER THEIR RETENTION RULE (6F-4; U39):
#[test]
fn the_program_marks_keep_the_rows_under_their_retention_rule() {
    use aterm_core::terminal::program_state::{ProgramBlockedKind, ProgramStateWord as W};
    let (h, _store) = rig(54);
    let hand = super::super::tests::hand_clock(&h.ctx.timeline);
    let moves = [
        (W::Working, None),
        (W::Blocked, Some(ProgramBlockedKind::Question)),
        (W::Working, None),
        (W::Done, None),
        (W::Idle, None),
        (W::Error, None),
    ];
    let rows_and_marks = |g: &SessionTimeline| {
        let rows: Vec<String> = g
            .since(None)
            .filter(|e| e.kind == crate::session_timeline::PROGRAM_STATE)
            .map(|e| e.payload.to_string())
            .collect();
        let marks: Vec<String> = g
            .slog()
            .log()
            .window()
            .filter(|r| r.k == Kind::Program)
            .map(|r| retained_row(&aterm_slog::encode(r, Tier::Content).expect("fits")))
            .collect();
        (rows, marks)
    };
    for (i, (state, kind)) in moves.into_iter().enumerate() {
        hand.set(1_000 * (i as u64 + 1));
        let mut g = h.ctx.timeline.lock().unwrap();
        let _ = g.note_program_state(Some(program_report(state, kind)));
    }
    let (rows, marks) = rows_and_marks(&h.ctx.timeline.lock().unwrap());
    assert_eq!(rows.len(), moves.len());
    assert_eq!(marks, rows, "under the bucket: row for record");
    hand.set(60_000);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        for i in 0..60 {
            let state = if i % 2 == 0 { W::Working } else { W::Blocked };
            let kind = (state == W::Blocked).then_some(ProgramBlockedKind::Auth);
            let _ = g.note_program_state(Some(program_report(state, kind)));
        }
    }
    hand.set(61_000);
    let mut g = h.ctx.timeline.lock().unwrap();
    let at = g.slog().now();
    g.slog_mut().program_tick(at);
    let (rows, marks) = rows_and_marks(&g);
    assert_eq!(
        marks.last(),
        rows.last(),
        "past it: the newest row is the newest mark"
    );
    assert_eq!(
        rows.last().map(String::as_str),
        Some("state=blocked kind=auth")
    );
    let newest = g
        .slog()
        .log()
        .window()
        .rfind(|r| r.k == Kind::Program)
        .map(|r| aterm_slog::encode(r, Tier::Content).expect("fits"))
        .expect("a program mark");
    assert!(newest.contains(" coalesced="), "{newest}");
    assert!(
        marks.len() < rows.len(),
        "NEGATIVE CONTROL: no longer row for record"
    );
}

fn drive_policy() -> Seen {
    let app = App::headless_for_test();
    let session = app
        .front_terminal(WindowId(0))
        .expect("a front terminal")
        .session;
    let ctx = Arc::clone(&app.pool.get(session).expect("pooled").ctx);
    during(&ctx.timeline, || app.announce_harness_policy())
}

/// `rig`'s session with one delivered row (inbox id 1, off=7).
fn delivered(local: u64) -> (SessionHandle, Store) {
    let (h, store) = rig(local);
    let line = format!(
        "{} off=7 from=h-andrew kind=task trust=human text=SENTINEL",
        h.ctx.self_id.as_str()
    );
    let reply = crate::fabric::cmd_deliver_waking(&store, &line, &|_| {});
    assert_eq!(reply, "OK 1\n");
    (h, store)
}

/// Every `inbox seen` (6F-6): a verdict decided, the same again (moved
fn drive_inbox_seen() -> Seen {
    let (h, store) = delivered(54);
    let line = format!(
        "{} off=9 from=s-peer@n-lab kind=ask trust=agent text=SENTINEL",
        h.ctx.self_id.as_str()
    );
    assert_eq!(
        crate::fabric::cmd_deliver_waking(&store, &line, &|_| {}),
        "OK 2\n"
    );
    during(&h.ctx.timeline, || {
        for (args, reply) in [
            ("1 handled", "OK seen=1\n"),
            ("1 handled", "OK seen=1\n"),
            ("2", "OK seen=2\n"),
        ] {
            assert_eq!(crate::fabric::cmd_inbox_seen(&h.ctx, args), reply);
        }
    })
}

/// A post from `h` (post id 1), queued whether or not a bridge is up.
fn post(h: &SessionHandle) {
    let reply = crate::fabric::cmd_post(
        &h.ctx,
        "to=@s-peer kind=ask dl=60000 key=k1 --wait=0 SENTINEL-POST",
        None,
    );
    assert!(queued(&reply, 1), "{reply}");
}

/// A post with every option (6F-6), then a broadcast on a topic: its
fn drive_post() -> Seen {
    let (h, _store) = rig(55);
    during(&h.ctx.timeline, || {
        post(&h);
        let reply = crate::fabric::cmd_post(
            &h.ctx,
            "to=say:builds kind=note re=7 via=n-lab --wait=0 SENTINEL-POST",
            None,
        );
        assert!(queued(&reply, 2), "{reply}");
    })
}

/// Two posts retired: one landed at an offset, one nowhere (`off=-`).
fn drive_landed() -> Seen {
    let (h, store) = rig(56);
    post(&h);
    post_again(&h);
    let sid = h.ctx.self_id.as_str().to_string();
    during(&h.ctx.timeline, || {
        for line in [format!("{sid} 1 off=90"), format!("{sid} 2 off=-")] {
            let reply = crate::fabric::cmd_outbox_sent_waking(&store, &line, &|_| {});
            assert!(reply.starts_with("OK"), "{line}: {reply}");
        }
    })
}

/// A second post from `h` (post id 2).
fn post_again(h: &SessionHandle) {
    let reply = crate::fabric::cmd_post(&h.ctx, "to=@s-peer kind=note --wait=0 SENTINEL", None);
    assert!(queued(&reply, 2), "{reply}");
}

/// Whether `post`'s reply says post `id` is queued, whatever the link: `OK
pub(super) fn queued(reply: &str, id: u64) -> bool {
    let named = format!("id={id}");
    reply.starts_with(&format!("OK {id}")) || reply.split_whitespace().any(|t| t == named)
}

/// The owner's hold, on with its reason and lifted.
fn drive_hold() -> Seen {
    let (h, store) = rig(57);
    let sid = h.ctx.self_id.as_str().to_string();
    during(&h.ctx.timeline, || {
        for rest in ["on reason=demo", "off"] {
            let reply = crate::fabric::cmd_hold_waking(
                &store,
                &format!("{sid} {rest}"),
                crate::fabric::HoldIssuer::Owner,
                &|_| {},
            );
            assert!(reply.starts_with("OK hold="), "{reply}");
        }
    })
}

fn drive_topic() -> Seen {
    let (h, _store) = rig(58);
    during(&h.ctx.timeline, || {
        assert_eq!(
            h.ctx.fabric.topic_add("news", "head", &h.ctx.timeline),
            Ok(true)
        );
        assert!(h.ctx.fabric.topic_drop("news", &h.ctx.timeline));
    })
}

/// A bridge attached to `store` and its link up: the fabric's `connected`
pub(super) fn link_up(store: &Store) -> crate::fabric::BridgeGeneration {
    let generation = crate::fabric::next_bridge_generation();
    crate::fabric::bridge_attached(generation);
    assert!(crate::fabric::link_report(
        store,
        Some(generation),
        true,
        "",
        Some(1)
    ));
    generation
}

/// THE FETCH'S OWN WRITER (`inbox get @<off>`): a read the ring cannot
pub(super) fn fetch_answered(h: &SessionHandle, store: &Store, off: u64) {
    let watcher = crate::session_watch::Subscription::single(&h.ctx.watchers);
    let ctx = Arc::clone(&h.ctx);
    let reader =
        std::thread::spawn(move || crate::fabric::cmd_inbox_get(&*ctx, &format!("@{off}")));
    let asked = |h: &SessionHandle| {
        h.ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .any(|e| e.kind == "fetch" && *e.payload == *format!("off={off}"))
    };
    while !asked(h) {
        assert!(
            watcher.wait(aterm_spec::wait::MUST_HAPPEN),
            "the fetch never parked"
        );
    }
    let answer = format!("{} fetched={off} err=no-record", h.ctx.self_id.as_str());
    let reply = crate::fabric::cmd_deliver_waking(store, &answer, &|_| {});
    assert_eq!(reply, "OK\n", "{answer}");
    let read = reader.join().expect("the reader");
    assert!(read.starts_with("ERR "), "{read}");
}

fn drive_fetch() -> Seen {
    crate::fabric::with_link_reset(|| {
        let (h, store) = rig(63);
        let _ = link_up(&store);
        during(&h.ctx.timeline, || fetch_answered(&h, &store, 7))
    })
}

/// THE ADVISORY'S OWN WRITER: a bridge that governed the session (a
fn drive_advisory() -> Seen {
    crate::fabric::with_link_reset(|| {
        let (h, store) = rig(64);
        let generation = link_up(&store);
        let line = format!(
            "{} off=7 from=h-andrew kind=note trust=human text=SENTINEL",
            h.ctx.self_id.as_str()
        );
        assert_eq!(
            crate::fabric::cmd_deliver_waking(&store, &line, &|_| {}),
            "OK 1\n"
        );
        during(&h.ctx.timeline, || {
            assert_eq!(crate::fabric::bridge_lost(&store, generation), 1);
        })
    })
}

/// The longest principal of `class` the grammar admits: its prefix and
fn longest(class: &str, fill: &str) -> String {
    format!(
        "{class}-{}",
        fill.repeat(aterm_types::fabric_grammar::PRINCIPAL_NAME_MAX)
    )
}

/// MAIL AT ITS PRODUCER'S BOUNDS (the review of 6F-6): a delivery from the
fn drive_mail_at_its_bounds() -> Seen {
    let (h, store) = rig(66);
    let from = format!("{}@{}", longest("s", "c"), longest("n", "d"));
    let line = format!(
        "{} off=7 from={from} kind=ask trust=agent text=SENTINEL",
        h.ctx.self_id.as_str()
    );
    during(&h.ctx.timeline, || {
        assert_eq!(
            crate::fabric::cmd_deliver_waking(&store, &line, &|_| {}),
            "OK 1\n"
        );
        assert_eq!(
            crate::fabric::cmd_inbox_seen(&h.ctx, "1 handled"),
            "OK seen=1\n"
        );
    })
}

/// A POST AT ITS PRODUCER'S BOUNDS (the review of 6F-6): to the longest
fn drive_post_at_its_bounds() -> Seen {
    let via = vec![longest("n", "v"); aterm_types::fabric_grammar::VIA_MAX_HOPS].join(",");
    let (h, _store) = rig(67);
    during(&h.ctx.timeline, || {
        let reply = crate::fabric::cmd_post(
            &h.ctx,
            &format!(
                "to=@{}@{} kind=control re={max} dl={max} via={via} key={} --wait=0 SENTINEL",
                longest("s", "a"),
                longest("n", "b"),
                "k".repeat(crate::fabric::KEY_MAX),
                max = u64::MAX
            ),
            None,
        );
        assert!(queued(&reply, 1), "{reply}");
        let reply = crate::fabric::cmd_post(
            &h.ctx,
            &format!("to=say kind=note via={via} --wait=0 SENTINEL"),
            None,
        );
        assert!(queued(&reply, 2), "{reply}");
    })
}

/// A HOLD AT ITS PRODUCER'S BOUNDS (the review of 6F-6): an owner's local
fn drive_hold_at_its_bounds() -> Seen {
    use crate::fabric::HoldIssuer::{Bridge, Owner};
    let (h, store) = rig(68);
    let sid = h.ctx.self_id.as_str().to_string();
    let flood = format!("{}ab", "%".repeat(126));
    let escapes = format!("{}ab", "%E9".repeat(42));
    during(&h.ctx.timeline, || {
        for (rest, issuer) in [
            (format!("on reason={flood} origin=local"), Owner),
            (format!("on reason={escapes} origin=fleet"), Bridge),
            ("off origin=fleet".to_string(), Bridge),
            ("off origin=local".to_string(), Owner),
        ] {
            let reply =
                crate::fabric::cmd_hold_waking(&store, &format!("{sid} {rest}"), issuer, &|_| {});
            assert!(reply.starts_with("OK hold="), "{rest}: {reply}");
        }
    })
}

/// A KEYED WRITE IN DOUBT AT ITS PRODUCER'S BOUNDS (the review of 6F-7): a
fn drive_in_doubt_at_its_bounds() -> Seen {
    let (h, _store) = rig(69);
    let key = |seq: u64| {
        crate::pty_idem::parse_key(&format!("{}:3:{seq}", h.ctx.nonce.to_hex()), h.ctx.nonce)
            .expect("a key")
    };
    let long = format!("ERR {} tail\n", "x".repeat(300));
    during(&h.ctx.timeline, || {
        let _ = crate::pty_idem::record_in_doubt(&h.ctx, "send", key(11), &long);
        let _ =
            crate::pty_idem::record_in_doubt(&h.ctx, "send", key(12), "ERR\u{200b}hidden words\n");
    })
}

/// Every mode this test can leave on: the kitty keyboard (main and alt), the
const MODES_ON: &[u8] = b"\x1b[>1u\x1b[?1049h\x1b[>1u\x1b[?1000h\x1b[?1006h\x1b[?1004h\
    \x1b[?2048h\x1b[?2031h\x1b[?1007l\x1b[>4;2m\x1b[>4;1f\x1b[?1h\x1b=\x1b[?67h\x1b[?1035h\
    \x1b[?1036h\x1b[?1039h\x1b[4h\x1b[20h\x1b[?5h\x1b(0\x0e\x1b[?25l\x1b]8;;http://x\x1b\\\
    \x1b[?6h\x1b[?69h\x1b[2;10s\x1b[2;10r\x1b]4;1;rgb:00/00/00\x1b\\\x1b[?2026h\x1b[1";

/// A HANDBACK AT ITS PRODUCER'S BOUNDS (the review of 6F-7): a terminal a
fn drive_modes_at_its_bounds() -> Seen {
    let mut t = aterm_core::terminal::Terminal::new(24, 80);
    t.process(MODES_ON);
    let back = t.foreground_handback().expect("the alt screen is evidence");
    let joined = back.reverted.join(",");
    assert!(joined.len() > 200, "{} bytes: {joined}", joined.len());
    let (h, _store) = rig(70);
    during(&h.ctx.timeline, || {
        let _ = crate::foreground_handback::record_restored(
            &mut h.ctx.timeline.lock().unwrap(),
            crate::foreground_handback::FgEdge::group(0, 300, 301),
            &back.reverted,
            back.bytes.len(),
        );
    })
}

/// A kind that left [`KNOWN_DIFFERENCES`] for [`EXACT`] (phase 6, 6F-5 to
struct Driven {
    row: &'static str,
    drive: fn() -> Seen,
    needs: Key,
}

/// Every [`EXACT`] kind 5B-6's script does not write, through its writer.
const DRIVEN: &[Driven] = &[
    Driven {
        row: "state-change",
        drive: drive_state,
        needs: Key::State,
    },
    Driven {
        row: crate::harness_policy_verb::HARNESS_POLICY_EVENT,
        drive: drive_policy,
        needs: Key::Gen,
    },
    Driven {
        row: "hold-advisory",
        drive: drive_advisory,
        needs: Key::Reason,
    },
    Driven {
        row: "fetch",
        drive: drive_fetch,
        needs: Key::Off,
    },
    Driven {
        row: "topic",
        drive: drive_topic,
        needs: Key::TopicName,
    },
    Driven {
        row: "inbox-seen",
        drive: drive_inbox_seen,
        needs: Key::Seen,
    },
    Driven {
        row: "post",
        drive: drive_post,
        needs: Key::Idem,
    },
    Driven {
        row: "post-landed",
        drive: drive_landed,
        needs: Key::Off,
    },
    Driven {
        row: "hold",
        drive: drive_hold,
        needs: Key::Reason,
    },
    Driven {
        row: "spawned",
        drive: drive_spawned,
        needs: Key::State,
    },
    Driven {
        row: "closing",
        drive: drive_closing,
        needs: Key::Closer,
    },
    Driven {
        row: "render",
        drive: drive_render,
        needs: Key::Run,
    },
    Driven {
        row: "modes-restored",
        drive: drive_modes,
        needs: Key::Reverted,
    },
    Driven {
        row: "in-doubt",
        drive: drive_in_doubt,
        needs: Key::KeySeq,
    },
    Driven {
        row: "handoff",
        drive: drive_handoff,
        needs: Key::Build,
    },
    Driven {
        row: crate::session_timeline::TITLE_CHANGE,
        drive: drive_title,
        needs: Key::Title,
    },
    Driven {
        row: crate::session_timeline::CWD_CHANGE,
        drive: drive_cwd,
        needs: Key::Cwd,
    },
    Driven {
        row: crate::session_timeline::CWD_CHANGE,
        drive: drive_cwd_successor,
        needs: Key::Cwd,
    },
    Driven {
        row: crate::session_timeline::META_CHANGE,
        drive: drive_meta,
        needs: Key::AttnText,
    },
    Driven {
        row: crate::session_timeline::META_CHANGE,
        drive: drive_autopilot,
        needs: Key::LandAt,
    },
    Driven {
        row: crate::session_timeline::META_CHANGE,
        drive: drive_meta_successor,
        needs: Key::Value,
    },
    Driven {
        row: "human",
        drive: drive_human,
        needs: Key::Why,
    },
    Driven {
        row: "inbox",
        drive: drive_mail_at_its_bounds,
        needs: Key::From,
    },
    Driven {
        row: "post",
        drive: drive_post_at_its_bounds,
        needs: Key::Via,
    },
    Driven {
        row: "hold",
        drive: drive_hold_at_its_bounds,
        needs: Key::Reason,
    },
    Driven {
        row: "in-doubt",
        drive: drive_in_doubt_at_its_bounds,
        needs: Key::Reply,
    },
    Driven {
        row: "modes-restored",
        drive: drive_modes_at_its_bounds,
        needs: Key::Reverted,
    },
    Driven {
        row: crate::session_timeline::TITLE_CHANGE,
        drive: drive_title_at_its_bounds,
        needs: Key::Title,
    },
    Driven {
        row: crate::session_timeline::CWD_CHANGE,
        drive: drive_cwd_at_its_bounds,
        needs: Key::Cwd,
    },
    Driven {
        row: crate::session_timeline::META_CHANGE,
        drive: drive_meta_at_its_bounds,
        needs: Key::Value,
    },
];

/// EVERY EXACT KIND AGREES THROUGH ITS OWN WRITER (phase 6, 6F-5 to 6F-8):
#[test]
fn every_exact_kind_agrees_through_its_own_writer() {
    for kind in EXACT {
        assert!(
            SCRIPTED.contains(&kind) || DRIVEN.iter().any(|d| d.row == kind),
            "{kind} is checked nowhere"
        );
    }
    let mut errs = Vec::new();
    for d in DRIVEN {
        let s = (d.drive)();
        let rows = only(s.rows.clone(), &EXACT);
        let log = only(projected(&s.log, None), &EXACT);
        if !rows.iter().any(|(k, _)| k == d.row) {
            errs.push(format!("{}: the writer wrote no row: {:?}", d.row, s.rows));
        } else if log != rows {
            errs.push(format!(
                "{}: the log's records {log:?} against the ring's rows {rows:?} ({:?})",
                d.row, s.recs
            ));
        } else if only(projected(&s.log, Some(d.needs)), &EXACT) == rows {
            errs.push(format!(
                "{}: a projection without {:?} agrees",
                d.row, d.needs
            ));
        }
    }
    assert!(errs.is_empty(), "{}", errs.join("\n"));
}

/// THE SESSION ROWS' WORDS ARE THE LOG'S (6F-7): each word a session row
#[test]
fn the_session_rows_words_are_the_logs() {
    use crate::manual_reset::ResetSource;
    use crate::resize_epoch::{EpochBy, ProgramClass};
    let words = |k: Key| match k.cap() {
        aterm_slog::Cap::Word(w) => w,
        cap => panic!("{k:?} is {cap:?}"),
    };
    assert_eq!(
        words(Key::Class),
        [
            ProgramClass::Agent,
            ProgramClass::Learned,
            ProgramClass::Unknown
        ]
        .map(ProgramClass::wire)
    );
    assert_eq!(
        words(Key::HealedBy),
        [EpochBy::App, EpochBy::Undo, EpochBy::Nudge].map(EpochBy::wire)
    );
    assert_eq!(EpochBy::None.wire(), "-");
    assert_eq!(
        words(Key::Source),
        [ResetSource::Verb, ResetSource::Menu].map(ResetSource::as_str)
    );
    for r in ExitReason::ALL {
        let out = aterm_slog::Out::Session(crate::session_log::exit_out(r));
        assert_eq!(out.word(), r.as_str(), "{r:?}");
    }
    assert!(Fields::new().word(Key::Source, "palette").is_err());
}

/// THE TABLE IS WHOLE: every kind of §0.1 is an exact pair or a named
#[test]
fn every_timeline_kind_is_an_exact_pair_or_a_named_difference() {
    let mut named: Vec<&str> = EXACT
        .iter()
        .copied()
        .chain(KNOWN_DIFFERENCES.iter().map(|k| k.row))
        .collect();
    named.sort_unstable();
    let len = named.len();
    named.dedup();
    assert_eq!(named.len(), len, "a kind named twice: {named:?}");
    let mut section: Vec<&str> = SECTION_0_1.to_vec();
    section.sort_unstable();
    assert_eq!(named, section);
    let scripted = crate::control::wire_goldens::scripted(false);
    let mut written: Vec<&str> = scripted
        .ctx
        .timeline
        .lock()
        .unwrap()
        .since(None)
        .map(|e| e.kind)
        .collect();
    written.sort_unstable();
    written.dedup();
    assert_eq!(written, section, "the goldens' script writes §0.1's kinds");
    for kind in crate::fabric::FABRIC_EVENT_KINDS {
        assert!(named.contains(kind), "{kind} is not named");
    }
}

/// EVERY NAMED DIFFERENCE STILL HOLDS, through its row's own writer: a
#[test]
fn every_named_difference_still_holds() {
    let mut errs = Vec::new();
    let mut seen = std::collections::BTreeMap::new();
    for k in KNOWN_DIFFERENCES {
        let Some(drive) = k.drive else { continue };
        let s = drive();
        if let Err(e) = holds(k, &s) {
            errs.push(format!("{e} ({}; until {:?})", k.why, k.until));
        }
        seen.insert(k.row, s);
    }
    assert!(errs.is_empty(), "{}", errs.join("\n"));
    let undriven: Vec<&str> = KNOWN_DIFFERENCES
        .iter()
        .filter(|k| k.drive.is_none())
        .map(|k| k.row)
        .collect();
    assert!(undriven.is_empty(), "every writer is driven: {undriven:?}");
    let known = |row: &str| {
        KNOWN_DIFFERENCES
            .iter()
            .find(|k| k.row == row)
            .expect("named")
    };
    let mut program = seen["program-state"].clone();
    if let Some(l) = program.recs.last_mut() {
        *l = l.replace(" state=none ", " state=working ");
    }
    assert!(
        holds(known("program-state"), &program).is_err(),
        "a stale newest program record"
    );
    let mut program = seen["program-state"].clone();
    let first = program.recs[0].clone();
    program.recs.insert(0, first);
    assert!(
        holds(known("program-state"), &program).is_err(),
        "a program record more than its rows"
    );
    let none = Seen::default();
    assert!(holds(known("program-state"), &none).is_err(), "no row");
}

/// Rows with their ids, `(id, kind, payload)`, oldest first.
type IdRows = Vec<(u64, String, String)>;

/// The ring's rows.
fn ring_ids(tl: &SessionTimeline) -> IdRows {
    tl.since(None)
        .map(|e| (e.id, e.kind.to_string(), e.payload.to_string()))
        .collect()
}

/// THE VIEW's rows ([`crate::session_timeline_view::timeline_rows`]).
fn view_ids(log: &Log) -> IdRows {
    crate::session_timeline_view::timeline_rows(log)
        .into_iter()
        .map(|e| (e.id, e.kind.to_string(), e.payload.to_string()))
        .collect()
}

/// Whether a row is of an exact pair.
fn is_exact(r: &(u64, String, String)) -> bool {
    EXACT.contains(&r.1.as_str())
}

/// `rows`' exact pairs, ids aside.
fn exact_of(rows: &[(u64, String, String)]) -> Vec<(String, String)> {
    rows.iter()
        .filter(|r| is_exact(r))
        .map(|r| (r.1.clone(), r.2.clone()))
        .collect()
}

/// THE RETENTION RULE (6F-4; U39) the view's `program-state` rows keep
fn program_rule(
    view: &[(u64, String, String)],
    ring: &[(u64, String, String)],
) -> Result<(), String> {
    let of = |rows: &[(u64, String, String)]| -> Vec<String> {
        rows.iter()
            .filter(|r| r.1 == crate::session_timeline::PROGRAM_STATE)
            .map(|r| r.2.clone())
            .collect()
    };
    let (v, r) = (of(view), of(ring));
    if v.len() > r.len() {
        return Err(format!(
            "{} program rows in the view for the ring's {}",
            v.len(),
            r.len()
        ));
    }
    if v.last() != r.last() {
        return Err(format!(
            "the view's newest program row {:?} is not the ring's {:?}",
            v.last(),
            r.last()
        ));
    }
    Ok(())
}

/// How [`view_against_ring`] holds the view to the ring.
#[derive(Clone, Copy)]
struct Hold<'a> {
    /// The first ring id of the view's epoch (the ring's `handoff` row's,
    from: u64,
    /// Every row (`false`: the ring dropped its oldest, so the two are
    whole: bool,
    /// The ids too: every ring row is a record's, so each id is the ring's.
    ids: bool,
    /// Ring kinds a script hand-wrote with no writer (no record's).
    hand_written: &'a [&'a str],
}

/// THE VIEW AGAINST THE RING (6G-2): over the same span, the view's exact
fn view_against_ring(tl: &SessionTimeline, hold: Hold<'_>) -> Result<(), String> {
    let view = view_ids(tl.slog().log());
    let mut ring: IdRows = ring_ids(tl)
        .into_iter()
        .filter(|r| r.0 >= hold.from && !hold.hand_written.contains(&r.1.as_str()))
        .collect();
    let mut view = view.as_slice();
    if !hold.whole {
        let first = ring.iter().position(is_exact).unwrap_or(ring.len());
        ring.drain(..first);
        let n = ring.iter().filter(|r| is_exact(r)).count();
        let at = view
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, r)| is_exact(r))
            .nth(n.saturating_sub(1))
            .map_or(0, |(i, _)| i);
        view = &view[at..];
    }
    let (ve, re) = (exact_of(view), exact_of(&ring));
    if ve != re {
        return Err(format!(
            "the view's exact rows {ve:?}\n against the ring's {re:?}"
        ));
    }
    program_rule(view, &ring)?;
    if hold.ids && view != ring.as_slice() {
        return Err(format!(
            "by id: the view {view:?}\n against the ring {ring:?}"
        ));
    }
    Ok(())
}

/// A published status transition, as the status sweep records it: the
fn status_move(g: &mut SessionTimeline, failed: bool) {
    use crate::session_status::{Confidence, Outcome, Phase, Status};
    let status = Status {
        phase: if failed { Phase::Idle } else { Phase::Running },
        last_outcome: if failed {
            Outcome::Failure { exit_code: 1 }
        } else {
            Outcome::None
        },
        since: Instant::now(),
        detail: None,
        confidence: Confidence::Exact,
        reasons: Vec::new(),
        conflict: false,
    };
    g.record(
        crate::session_status::STATUS_CHANGE,
        crate::session_status::status_event_payload(&status),
    );
    let at = g.slog().now();
    g.slog_mut()
        .observe_status(crate::session_status::status_facts(&status), at);
}

/// A run of the shell's, opened and closed: records no row prints.
fn shell_run(g: &mut SessionTimeline) {
    let at = g.slog().now();
    let log = g.slog_mut().log_mut_for_tests();
    let run = log
        .open(
            aterm_slog::Open::new(Kind::Cmd, aterm_slog::Src::Shell, aterm_slog::Conf::Strong),
            at,
        )
        .expect("a run opens");
    log.close(
        run,
        aterm_slog::Close::new(
            aterm_slog::Out::Run(aterm_slog::RunOut::Ok),
            aterm_slog::Src::Shell,
            aterm_slog::Conf::Exact,
        ),
        at,
    )
    .expect("the run closes");
}

/// Every writer of a row and its record, interleaved on one session, round
fn every_writer(h: &SessionHandle, store: &Store, round: u64) {
    use aterm_core::terminal::program_state::ProgramStateWord as W;
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        g.record_cwd_change(Some(&format!("/tmp/x/{round}")));
        let word = if round.is_multiple_of(2) {
            "busy"
        } else {
            "idle"
        };
        let _ = g.publish_agent(
            word,
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(round + 1),
        );
        assert!(g.record_title(&format!("build {round}")));
    }
    let mark = if round.is_multiple_of(2) {
        "mark working"
    } else {
        "mark done why=marker"
    };
    let _ = crate::task_record::cmd_task(&h.ctx, h.local_id, crate::control::Scope::Owner, mark);
    let _ = crate::app_input::note_person(&h.ctx, crate::human_input::Gesture::Other);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let _ = g.note_harness_act("approve", "codex-alab@1", Actor::Ctl);
        status_move(&mut g, round % 2 == 1);
        let state = if round.is_multiple_of(2) {
            W::Working
        } else {
            W::Done
        };
        let _ = g.note_program_state(Some(program_report(state, None)));
    }
    let edit = if round.is_multiple_of(2) {
        crate::session_timeline::MetaEdit::Set("look here")
    } else {
        crate::session_timeline::MetaEdit::Clear
    };
    let _ = crate::session_timeline::write_attention_owned(
        &h.ctx,
        crate::harness_host::ATTENTION_OWNER,
        edit,
        Actor::Harness,
    );
    let wake = |_: &aterm_session::SessionId| {};
    let line = format!(
        "{} off={} from=s-a kind=ask trust=agent text=SENTINEL",
        h.ctx.self_id.as_str(),
        10 + round
    );
    let reply = crate::fabric::cmd_deliver_waking(store, &line, &wake);
    assert!(reply.starts_with("OK "), "{reply}");
}

/// The view held to the ring row for row, by id.
const BY_ID: Hold<'static> = Hold {
    from: 0,
    whole: true,
    ids: true,
    hand_written: &[],
};

/// THE VIEW IS THE RING OVER 5B-5's SCRIPT (6G-2): the goldens' session,
#[test]
fn the_view_is_the_ring_over_the_goldens_script() {
    let h = crate::control::wire_goldens::scripted(false);
    let g = h.ctx.timeline.lock().unwrap();
    let from = g
        .since(None)
        .rfind(|e| e.kind == "handoff")
        .map(|e| e.id)
        .expect("the update's row");
    let hold = Hold {
        from,
        whole: true,
        ids: false,
        hand_written: &["fetch", "hold-advisory"],
    };
    view_against_ring(&g, hold).unwrap_or_else(|e| panic!("{e}"));
    let view = view_ids(g.slog().log());
    let kinds: Vec<&str> = view.iter().map(|r| r.1.as_str()).collect();
    assert_eq!(kinds.first(), Some(&"handoff"), "{kinds:?}");
    assert!(kinds.contains(&crate::session_timeline::PROGRAM_STATE));
    assert_eq!(kinds[kinds.len() - 2..], ["closing", "state-change"]);
    let every = Hold {
        hand_written: &[],
        ..hold
    };
    assert!(
        view_against_ring(&g, every).is_err(),
        "NEGATIVE CONTROL: the hand-written rows are the ring's alone"
    );
}

/// THE VIEW IS THE RING ACROSS A SEAMLESS UPDATE, carried and not, and
#[test]
fn the_view_is_the_ring_across_an_update_and_a_crash_adoption() {
    for carry in [true, false] {
        let (h, store) = rig(72);
        let hand = super::super::tests::hand_clock(&h.ctx.timeline);
        every_writer(&h, &store, 0);
        let pred_high = {
            let g = h.ctx.timeline.lock().unwrap();
            view_against_ring(&g, BY_ID).unwrap_or_else(|e| panic!("predecessor: {e}"));
            g.high_id().expect("rows")
        };
        hand.set(5_000);
        let (next, next_store, _next_hand) = hand_over(&h, &store.read().unwrap(), 10_000, carry);
        let shared: Store = Arc::new(std::sync::RwLock::new(next_store));
        every_writer(&next, &shared, 1);
        let g = next.ctx.timeline.lock().unwrap();
        view_against_ring(&g, BY_ID).unwrap_or_else(|e| panic!("successor (carry {carry}): {e}"));
        let first = view_ids(g.slog().log())[0].clone();
        assert_eq!(first.1, "handoff", "carry {carry}");
        assert!(
            first.0 > pred_high,
            "carry {carry}: the successor's first id {} is past the predecessor's {pred_high}",
            first.0
        );
    }
    let kept = test_handle(73);
    crate::spawn::mark_handoff_gap(
        &kept.ctx.timeline,
        &kept.ctx.cast,
        &kept.ctx.temporal,
        Some(9),
        0,
        crate::session_log::LogAdopt::default(),
        true,
    );
    let store = new_store();
    store.write().unwrap().register(kept.clone());
    every_writer(&kept, &store, 2);
    let g = kept.ctx.timeline.lock().unwrap();
    view_against_ring(&g, BY_ID).unwrap_or_else(|e| panic!("crash adoption: {e}"));
    let first = &view_ids(g.slog().log())[0];
    assert_eq!(
        (first.0, first.1.as_str()),
        (1, "handoff"),
        "a crash adoption's ids start at 1"
    );
}

/// What the ring's own digest tells a watcher at `after`
fn ring_gap(tl: &SessionTimeline, after: Option<u64>) -> u64 {
    let mut out = String::new();
    let _ = crate::subscribe::drain_timeline_events(tl, "7", after, &mut out);
    out.lines()
        .find_map(|l| l.strip_prefix("GAP 7 events-dropped="))
        .map_or(0, |n| n.parse().expect("a count"))
}

/// Every reader's id in `anchors` against the ring: the view past it is the
fn readers_against_ring(
    tl: &SessionTimeline,
    case: &str,
    anchors: impl IntoIterator<Item = Option<u64>>,
) -> Vec<String> {
    let mut wrong = Vec::new();
    for after in anchors {
        let view = crate::session_timeline_view::timeline_rows_since(tl.slog().log(), after);
        let rows: IdRows = view
            .rows
            .iter()
            .map(|e| (e.id, e.kind.to_string(), e.payload.to_string()))
            .collect();
        let ring: IdRows = tl
            .since(after)
            .map(|e| (e.id, e.kind.to_string(), e.payload.to_string()))
            .collect();
        if rows != ring {
            wrong.push(format!(
                "{case} after {after:?}: the view shows {} rows from {:?}, the ring {} from {:?}",
                rows.len(),
                rows.first().map(|r| r.0),
                ring.len(),
                ring.first().map(|r| r.0)
            ));
        }
        let gap = ring_gap(tl, after);
        if view.lost != gap {
            wrong.push(format!(
                "{case} after {after:?}: the view tells lost={}, the ring's digest GAP {gap}",
                view.lost
            ));
        }
    }
    wrong
}

/// EVERY READER IS TOLD WHAT THE RING TELLS IT (6G-2, the review of 6G-2):
#[test]
fn the_view_tells_every_reader_what_the_ring_tells_it() {
    use crate::session_timeline::HANDOFF_ID_RESERVE;
    let mut wrong = Vec::new();
    for carry in [true, false] {
        let (h, store) = rig(78);
        let hand = super::super::tests::hand_clock(&h.ctx.timeline);
        every_writer(&h, &store, 0);
        let pred_high = h.ctx.timeline.lock().unwrap().high_id().expect("rows");
        hand.set(5_000);
        let (next, next_store, _next_hand) = hand_over(&h, &store.read().unwrap(), 10_000, carry);
        let shared: Store = Arc::new(std::sync::RwLock::new(next_store));
        every_writer(&next, &shared, 1);
        let g = next.ctx.timeline.lock().unwrap();
        let (low, head) = (g.low_id().expect("rows"), g.high_id().expect("rows"));
        assert_eq!(
            ring_gap(&g, Some(pred_high)),
            HANDOFF_ID_RESERVE,
            "CONTROL (carry {carry}): the ring's digest tells the update's jump"
        );
        let anchors = [None, Some(0)]
            .into_iter()
            .chain((1..=pred_high + 1).map(Some))
            .chain([low - 2, low - 1].map(Some))
            .chain((low..=head + 1).map(Some))
            .chain([Some(head + HANDOFF_ID_RESERVE)]);
        wrong.extend(readers_against_ring(
            &g,
            &format!("update carry={carry}"),
            anchors,
        ));
    }
    let kept = test_handle(79);
    crate::spawn::mark_handoff_gap(
        &kept.ctx.timeline,
        &kept.ctx.cast,
        &kept.ctx.temporal,
        Some(9),
        0,
        crate::session_log::LogAdopt::default(),
        true,
    );
    let store = new_store();
    store.write().unwrap().register(kept.clone());
    every_writer(&kept, &store, 2);
    let g = kept.ctx.timeline.lock().unwrap();
    let head = g.high_id().expect("rows");
    let crashed = 3 + HANDOFF_ID_RESERVE + 2;
    assert!(
        crashed > g.slog().log().row_head(),
        "CONTROL: the crashed window's anchor is past the recovery's head"
    );
    let anchors = [None, Some(0)]
        .into_iter()
        .chain((1..=head + 1).map(Some))
        .chain([Some(crashed), Some(500)]);
    wrong.extend(readers_against_ring(&g, "crash recovery", anchors));
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A KEEPER'S RECOVERY NAMES ITS ROWS APART (6G-1's `(epoch, id)`, the
#[test]
fn a_keeper_recovery_names_its_rows_apart_by_epoch() {
    let keyed = |h: &SessionHandle| -> std::collections::BTreeSet<(u32, u64)> {
        let g = h.ctx.timeline.lock().unwrap();
        let log = g.slog().log();
        view_ids(log).iter().map(|r| (log.ep(), r.0)).collect()
    };
    let ep_of = |h: &SessionHandle| h.ctx.timeline.lock().unwrap().slog().log().ep();
    let (first, store) = rig(80);
    let _hand = super::super::tests::hand_clock(&first.ctx.timeline);
    every_writer(&first, &store, 0);
    let mut windows = vec![first];
    for round in 1..=2u64 {
        let crashed = windows.last().expect("a window");
        let kept = test_handle(80 + round);
        let _hand = super::super::tests::hand_clock(&kept.ctx.timeline);
        crate::spawn::mark_handoff_gap(
            &kept.ctx.timeline,
            &kept.ctx.cast,
            &kept.ctx.temporal,
            None,
            0,
            crate::session_log::LogAdopt {
                keeper_ep: Some(ep_of(crashed)),
                ..Default::default()
            },
            true,
        );
        let store = new_store();
        store.write().unwrap().register(kept.clone());
        every_writer(&kept, &store, round);
        windows.push(kept);
    }
    let eps: Vec<u32> = windows.iter().map(ep_of).collect();
    assert!(
        eps.windows(2).all(|w| w[0] < w[1]),
        "each recovery's epoch is past its crashed window's: {eps:?}"
    );
    let rows: Vec<_> = windows.iter().map(keyed).collect();
    for (i, a) in rows.iter().enumerate() {
        for b in &rows[i + 1..] {
            assert!(a.is_disjoint(b), "two windows name one row: {eps:?}");
        }
    }
    let ids = |k: &std::collections::BTreeSet<(u32, u64)>| -> std::collections::BTreeSet<u64> {
        k.iter().map(|r| r.1).collect()
    };
    assert!(
        rows.windows(2)
            .all(|w| !ids(&w[0]).is_disjoint(&ids(&w[1]))),
        "CONTROL: the ids alone collide"
    );
}

/// THE VIEW IS THE RING OF A CLOSED SESSION, row for row by id, over
#[test]
fn the_view_is_the_ring_of_a_closed_session() {
    let (h, store) = rig(74);
    for round in 0..3 {
        every_writer(&h, &store, round);
    }
    let _ = store
        .write()
        .unwrap()
        .deregister_local_as(74, ExitReason::CtlClose, ExitActor::Ctl);
    let g = h.ctx.timeline.lock().unwrap();
    view_against_ring(&g, BY_ID).unwrap_or_else(|e| panic!("{e}"));
    let ring = ring_ids(&g);
    for kind in [
        "spawned",
        crate::session_timeline::CWD_CHANGE,
        "agent-change",
        crate::session_timeline::TITLE_CHANGE,
        "task-change",
        "human",
        crate::session_timeline::HARNESS_ACT,
        crate::session_status::STATUS_CHANGE,
        crate::session_timeline::PROGRAM_STATE,
        crate::session_timeline::META_CHANGE,
        "inbox",
    ] {
        assert!(
            ring.iter().any(|r| r.1 == kind),
            "every writer wrote: no {kind} row in {ring:?}"
        );
    }
    let view = view_ids(g.slog().log());
    let tail: Vec<&str> = view[view.len() - 2..]
        .iter()
        .map(|r| r.1.as_str())
        .collect();
    assert_eq!(tail, ["closing", "state-change"]);
    let close = g.slog().log().rows().next_back().expect("the close's row");
    assert_eq!((close.rec.op, close.span), (Op::Close, 2));
    assert_eq!(close.id, view[view.len() - 2].0);
}

/// THE VIEW UNDER A FLOOD (6G-2 over 6G-1's floor): 10^4 status changes,
#[test]
fn the_view_is_the_ring_under_a_flood() {
    use aterm_core::terminal::program_state::{ProgramBlockedKind, ProgramStateWord as W};
    let (h, _store) = rig(75);
    let hand = super::super::tests::hand_clock(&h.ctx.timeline);
    let mut g = h.ctx.timeline.lock().unwrap();
    let (mut status, mut cwd, mut program, mut runs) = (0, 0, 0, 0);
    for block in 0..300u64 {
        hand.advance(10);
        let state = if block.is_multiple_of(2) {
            W::Working
        } else {
            W::Blocked
        };
        let kind = (state == W::Blocked).then_some(ProgramBlockedKind::Auth);
        let _ = g.note_program_state(Some(program_report(state, kind)));
        program += 1;
        for _ in 0..2 {
            hand.advance(10);
            g.record_cwd_change(Some(&format!("/tmp/x/{cwd}")));
            cwd += 1;
        }
        for _ in 0..5 {
            hand.advance(10);
            shell_run(&mut g);
            runs += 1;
        }
        for _ in 0..(33 + u64::from(block < 100)) {
            hand.advance(10);
            status_move(&mut g, status % 2 == 0);
            status += 1;
        }
    }
    for _ in 0..500 {
        hand.advance(10);
        shell_run(&mut g);
        runs += 1;
    }
    assert_eq!((status, cwd, program, runs), (10_000, 600, 300, 2_000));
    hand.advance(60_000);
    let at = g.slog().now();
    g.slog_mut().program_tick(at);
    let hold = Hold {
        from: 0,
        whole: false,
        ids: false,
        hand_written: &[],
    };
    view_against_ring(&g, hold).unwrap_or_else(|e| panic!("{e}"));
    let (view, ring) = (view_ids(g.slog().log()), ring_ids(&g));
    assert_eq!(ring.len(), crate::session_timeline::TIMELINE_CAP);
    let floor = aterm_slog::row_floor(crate::session_log::ring_limits()).records;
    assert!(
        view.len() >= floor,
        "{} rows, the floor {floor}",
        view.len()
    );
    let program_rows = |rows: &IdRows| {
        rows.iter()
            .filter(|r| r.1 == crate::session_timeline::PROGRAM_STATE)
            .count()
    };
    assert!(
        program_rows(&ring) > 0,
        "the ring's span holds program rows"
    );
    let mut stale = ring.clone();
    let newest = stale
        .iter_mut()
        .rfind(|r| r.1 == crate::session_timeline::PROGRAM_STATE)
        .expect("a program row");
    newest.2 = if newest.2 == "state=working kind=-" {
        "state=blocked kind=auth".to_string()
    } else {
        "state=working kind=-".to_string()
    };
    assert_eq!(
        program_rule(&ring, &ring),
        Ok(()),
        "CONTROL: the ring itself"
    );
    assert!(
        program_rule(&stale, &ring).is_err(),
        "a stale newest program row"
    );
    let mut more = ring.clone();
    let one = more
        .iter()
        .find(|r| r.1 == crate::session_timeline::PROGRAM_STATE)
        .cloned()
        .expect("a program row");
    more.insert(0, one);
    assert!(program_rule(&more, &ring).is_err(), "more program rows");
}

/// A HOLE REPORTS A GAP (6G-2): 3000 directory moves, more rows than the
#[test]
fn a_hole_in_the_view_reports_a_gap() {
    use crate::session_timeline_view::timeline_rows_since;
    let (h, _store) = rig(76);
    let mut g = h.ctx.timeline.lock().unwrap();
    for i in 0..600 {
        g.record_cwd_change(Some(&format!("/tmp/x/{i}")));
    }
    assert_eq!(
        timeline_rows_since(g.slog().log(), None).lost,
        0,
        "CONTROL: nothing taken yet"
    );
    for i in 600..3_000 {
        g.record_cwd_change(Some(&format!("/tmp/x/{i}")));
    }
    let log = g.slog().log();
    let all = timeline_rows_since(log, None);
    assert!(all.lost > 0, "the log took its oldest rows");
    let issued = log.rows_issued();
    assert_eq!(
        all.lost + all.rows.len() as u64,
        issued,
        "every id is shown or told lost"
    );
    let newest_lost = (log.row_first()..=log.row_head())
        .rev()
        .find(|id| !all.rows.iter().any(|e| e.id == *id))
        .expect("a hole");
    let past = timeline_rows_since(log, Some(newest_lost));
    assert_eq!(past.lost, 0, "past the holes, no gap");
    assert_eq!(past.rows.len() as u64, log.row_head() - newest_lost);
    let before = timeline_rows_since(log, Some(newest_lost - 1));
    assert_eq!(before.lost, 1, "one hole crossed");
}

/// THE ROW FOLLOWS ITS RECORD for a self-claim's act (6G-2; the residual
#[test]
fn an_act_the_limiter_refuses_is_no_row() {
    let (h, _store) = rig(77);
    let _hand = super::super::tests::hand_clock(&h.ctx.timeline);
    let mut g = h.ctx.timeline.lock().unwrap();
    let ids: Vec<Option<u64>> = (0..40)
        .map(|_| g.note_harness_act("approve", "codex-alab@1", Actor::Ctl))
        .collect();
    let rows = ring_rows(&g, &[crate::session_timeline::HARNESS_ACT]).len();
    let acts = g.slog().log().window().filter(|r| r.k == Kind::Act).count();
    assert!(acts < 40, "CONTROL: the limiter refused some: {acts}");
    assert_eq!(rows, acts, "a row for each recorded act, none for the rest");
    assert_eq!(ids.iter().flatten().count(), acts);
    view_against_ring(&g, BY_ID).unwrap_or_else(|e| panic!("{e}"));
}

/// The `by=` an actor's close should say, under 3A's R1.
fn by_r1(actor: &ExitActor) -> String {
    match actor {
        ExitActor::Sid(sid) if aterm_session::is_minted_sid(sid) => sid.clone(),
        ExitActor::Sid(_) | ExitActor::Unknown => "-".to_string(),
        ExitActor::Human => "human".to_string(),
        ExitActor::Ctl => "ctl".to_string(),
        ExitActor::Bridge => "bridge".to_string(),
    }
}

/// The closed index's session close for `local`: `(out, code, by)`.
fn index_close(app: &App, local: u64) -> Option<(String, Option<i64>, String)> {
    let source = app
        .store
        .read()
        .unwrap()
        .closed_logs()
        .by_local(local)?
        .source();
    let log = source.into_log()?;
    let r = log
        .window()
        .rev()
        .find(|r| r.op == Op::Close && r.node == Node::SESSION)?;
    let code = match r.f.get(Key::Code) {
        Some(Val::I(n)) => Some(*n),
        _ => None,
    };
    Some((
        r.f.get_word(Key::Out)?.to_string(),
        code,
        r.by.word().to_string(),
    ))
}

/// Whether every roster `Exited` row and the closed index agree, `out_of`
fn exits_agree(
    app: &App,
    out_of: fn(ExitReason) -> &'static str,
    unindexed: &[u64],
) -> Result<usize, String> {
    let rows: Vec<crate::session_store::RosterRecord> = app
        .store
        .read()
        .unwrap()
        .roster_since(0)
        .filter(|r| r.change == RosterChange::Exited)
        .cloned()
        .collect();
    let mut agreed = 0;
    for row in &rows {
        let got = index_close(app, row.local_id);
        if unindexed.contains(&row.local_id) {
            if got.is_some() {
                return Err(format!("{}: a keep=None close is indexed", row.local_id));
            }
            continue;
        }
        let want = (
            out_of(row.reason).to_string(),
            row.exit_code.map(i64::from),
            by_r1(&row.actor),
        );
        if got.as_ref() != Some(&want) {
            return Err(format!(
                "local {} ({:?} by {:?}): the roster says {want:?}, the index {got:?}",
                row.local_id, row.reason, row.actor
            ));
        }
        agreed += 1;
    }
    Ok(agreed)
}

/// THE ROSTER AND THE CLOSED INDEX AGREE (plan 5B-6, P2): every reason ×
#[test]
fn the_roster_and_the_closed_index_agree_on_every_exit() {
    let mut app = App::headless_for_test();
    let minted = aterm_session::SessionId::generate().as_str().to_string();
    assert!(aterm_session::is_minted_sid(&minted));
    let actors = [
        ExitActor::Sid(minted),
        ExitActor::Sid("s-driver".into()),
        ExitActor::Human,
        ExitActor::Ctl,
        ExitActor::Bridge,
        ExitActor::Unknown,
    ];
    let mut n = 0u64;
    let mut close = |app: &mut App, reason, actor: ExitActor, keep| {
        let local = app.next_session_id;
        app.push_stub_tab(WindowId(0), crate::stub_session(local));
        {
            let ctx = Arc::clone(&app.pool.get(local).expect("pooled").ctx);
            let mut g = ctx.timeline.lock().unwrap();
            if g.slog().log().is_empty() {
                let now = g.slog().now();
                g.slog_mut().open_session(None, None, now);
            }
        }
        n += 1;
        if n.is_multiple_of(2) {
            app.store
                .write()
                .unwrap()
                .note_exit_code(local, Some(i32::try_from(n).unwrap_or(1)));
        }
        let _closing = CloseAttribution::enter(reason, actor);
        app.retire_session_registration_keeping(local, keep);
        local
    };
    for reason in ExitReason::ALL {
        for actor in &actors {
            let _ = close(
                &mut app,
                reason,
                actor.clone(),
                Some(crate::session_log::ClosedKeep::Skeleton),
            );
        }
    }
    let unindexed = close(&mut app, ExitReason::UiClose, ExitActor::Human, None);
    let shipped = exits_agree(&app, ExitReason::as_str, &[unindexed]);
    assert_eq!(shipped, Ok(36), "{shipped:?}");
    let swapped = |r: ExitReason| match r {
        ExitReason::CtlClose => "ui-close",
        ExitReason::UiClose => "ctl-close",
        other => other.as_str(),
    };
    assert!(exits_agree(&app, swapped, &[unindexed]).is_err());
    assert!(exits_agree(&app, ExitReason::as_str, &[]).is_err());
}

/// The `ctl story` word a told verb has, `None` for a presence verb.
fn told_word(verb: StoryVerb) -> Option<&'static str> {
    StoryVerb::TOLD_WORDS
        .into_iter()
        .find(|w| StoryVerb::parse_told(w) == Some(verb))
}

/// The seq of the record that opened `log`'s epoch (its first record
fn epoch_start(log: &Log) -> u64 {
    let ep = u64::from(log.ep());
    log.window()
        .find(|r| r.f.get_u(Key::Epoch) == Some(ep))
        .map_or(0, |r| r.seq)
}

/// The `story` marks of `log` from `from` on, as `(word, by)`.
fn story_marks(log: &Log, from: u64) -> Vec<(String, String)> {
    log.window()
        .filter(|r| r.seq >= from && r.op == Op::Mark && r.k == Kind::Story)
        .map(|r| {
            (
                r.f.get_word(Key::Word).unwrap_or("-").to_string(),
                r.by.word().to_string(),
            )
        })
        .collect()
}

/// Whether presence's told points are `told` (each `(seq, verb, teller)`)
fn story_agrees(
    told: &[(u64, StoryVerb, Actor)],
    points: &[(u64, StoryVerb)],
    log: &Log,
) -> Result<(), String> {
    let told_points: Vec<(u64, StoryVerb)> = points
        .iter()
        .copied()
        .filter(|(_, v)| told_word(*v).is_some())
        .collect();
    let want_points: Vec<(u64, StoryVerb)> = told.iter().map(|(s, v, _)| (*s, *v)).collect();
    if told_points != want_points {
        return Err(format!(
            "presence holds {told_points:?}, told {want_points:?}"
        ));
    }
    let marks = story_marks(log, epoch_start(log));
    let want: Vec<(String, String)> = told
        .iter()
        .map(|(_, v, by)| {
            (
                told_word(*v).unwrap_or("-").to_string(),
                by.word().to_string(),
            )
        })
        .collect();
    if marks != want {
        return Err(format!("the epoch's story marks {marks:?}, told {want:?}"));
    }
    Ok(())
}

/// EACH TOLD POINT IS ONE STORY MARK BY ITS TELLER (T17): five points told
#[test]
fn each_told_point_is_one_story_mark_by_its_teller() {
    let mut app = App::headless_for_test();
    let session = app
        .front_terminal(WindowId(0))
        .expect("a front terminal")
        .session;
    let ctx = Arc::clone(&app.pool.get(session).expect("pooled").ctx);
    let minted = aterm_session::SessionId::generate();
    let sid = Actor::sid(ShortId::new(minted.as_str()).expect("a short id"));
    let mut told = Vec::new();
    for (verb, by) in [
        (StoryVerb::Chose, Actor::Harness),
        (StoryVerb::Chose, Actor::Ctl),
        (StoryVerb::Chose, sid),
        (StoryVerb::Chose, Actor::Human),
        (StoryVerb::Chose, Actor::Ctl),
    ] {
        let seq = app.tell_story_by(session, verb, by).expect("told");
        told.push((seq, verb, by));
    }
    let points: Vec<(u64, StoryVerb)> = app
        .presence
        .slot(session)
        .expect("a slot")
        .story_since(0)
        .map(|p| (p.seq, p.verb))
        .collect();
    {
        let g = ctx.timeline.lock().unwrap();
        assert_eq!(story_agrees(&told, &points, g.slog().log()), Ok(()));
    }
    {
        let mut g = ctx.timeline.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().observe_story("chose", Actor::Ctl, at);
        assert!(story_agrees(&told, &points, g.slog().log()).is_err());
    }
    let (h, store) = rig(60);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().observe_story("chose", Actor::Harness, at);
    }
    let (next, _next_store, _hand) = hand_over(&h, &store.read().unwrap(), 1_000, true);
    let g = next.ctx.timeline.lock().unwrap();
    let log = g.slog().log();
    assert_eq!(log.ep(), 2);
    assert_eq!(story_agrees(&[], &[], log), Ok(()));
    assert_eq!(
        story_marks(log, 0),
        [("chose".to_string(), "harness".to_string())],
        "the carried mark, before the epoch"
    );
}
