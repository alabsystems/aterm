// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Overlap tests between the session log, its timeline view, the closed-session index, and presence's story (the ring the view replaced was deleted in phase 6, 6G-5).

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

/// The timeline kinds the log writes row for record, the whole payload
/// (T19's exact pairs, held equal to the ring's rows until 6G-5 deleted the
/// ring on a tree T22 ran on; `program-state` is the one kind kept under a
/// retention rule, U39).
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
/// ([`the_scripted_session_writes_its_exact_kinds_through_their_writers`]);
/// the others are [`DRIVEN`]'s, each through its own writer.
const SCRIPTED: [&str; 7] = [
    "agent-change",
    crate::session_status::STATUS_CHANGE,
    "inbox",
    "task-change",
    crate::session_timeline::HARNESS_ACT,
    crate::session_timeline::META_CHANGE,
    "human",
];

/// The scripted session's [`EXACT`] rows, `<kind> <payload>` a line, oldest
/// first ([`as_lines`]): the ring's own rows at 5b3bb8c388 (6G-5's parent,
/// where each writer formatted its ring row itself and the ring was held to
/// the log), so the view is held to an oracle its grammar did not write.
const SCRIPTED_GOLDEN: &str = concat!(
    "spawned state=spawning\n",
    "status-change phase=idle outcome=failure exit_code=1 signal=- confidence=strong\n",
    "agent-change busy rev=1 gen=0.1 fp=000000000000002a\n",
    "agent-change prompt rev=2 gen=0.2 fp=000000000000002a\n",
    "agent-change question rev=3 gen=0.3 fp=000000000000002a\n",
    "agent-change idle rev=4 gen=0.4 fp=000000000000002a\n",
    "human \n",
    "inbox 1 from=h-andrew kind=task off=7\n",
    "inbox 2 from=s-peer@n-lab kind=ask off=8\n",
    "task-change rev=1 state=working attention=0\n",
    "task-change rev=2 state=done attention=0 why=marker\n",
    "task-change rev=3 state=exited attention=0 why=exit exit=2\n",
    "meta-change field=attention value=look%20here\n",
    "task-change rev=4 state=exited attention=1 why=exit exit=2\n",
    "harness-act act=approve-all@v1 holder=codex-alab@1\n",
    "harness-act act=finished@v1 holder=codex-alab@1\n",
    "harness-act act=usage-resume@v1 holder=codex-alab@1\n",
    "status-change phase=idle outcome=success exit_code=- signal=- confidence=strong\n",
    "agent-change - rev=5 gen=0.3 fp=fd34daff5d4709b4\n"
);

/// Every timeline kind of plan §0.1 (the view's own list, which a hand-written
/// row may name).
use crate::session_timeline_view::SECTION_0_1;

/// One record as the rows it stands for: the view's grammar
/// ([`crate::session_timeline_view::rows_of`]), each payload cut by the
/// view's rule, 256 bytes never inside an escape
/// ([`crate::session_timeline::clamp_payload`]).
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
/// as `(kind, payload)`, oldest first.
pub(crate) fn overlap_rows(log: &Log) -> Vec<(String, String)> {
    crate::session_timeline_view::timeline_rows(log)
        .into_iter()
        .map(|e| (e.kind.to_string(), e.payload.to_string()))
        .collect()
}

fn overlap_rows_without(log: &Log, drop: Option<Key>) -> Vec<(String, String)> {
    projected(log.window(), drop)
}

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

fn only(rows: Vec<(String, String)>, kinds: &[&str]) -> Vec<(String, String)> {
    rows.into_iter()
        .filter(|(k, _)| kinds.contains(&k.as_str()))
        .collect()
}

/// THE SCRIPTED SESSION WRITES ITS EXACT KINDS THROUGH THEIR WRITERS (T19,
/// on the view since 6G-5): over one scripted session, through each row's
/// production writer — the status sweep's published transitions (a failing
/// command, then a prompt), the agent publication (each verdict, one with no
/// screen stamp), a person's burst, two bridge deliveries (and a
/// redelivery, which writes neither) and a worker's task moved through
/// `task mark` and the supervisor's attention (and a repeated mark, which
/// writes neither; its `meta-change` row's text the mark's `attn_text=`) —
/// the view holds a row of each scripted kind, its rows are the script's
/// pinned rows byte for byte ([`SCRIPTED_GOLDEN`], the ring's at 6G-5's
/// parent: the oracle the deleted ring was), the grammar over the log's
/// records is the view, and each key a writer marks is one the view reads
/// (dropped, the rows differ).
#[test]
fn the_scripted_session_writes_its_exact_kinds_through_their_writers() {
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
    let view = only(overlap_rows(g.slog().log()), &EXACT);
    for kind in SCRIPTED {
        assert!(
            view.iter().any(|(k, _)| k == kind),
            "the script wrote no {kind} row: {view:?}"
        );
    }
    assert_eq!(
        as_lines(&view),
        SCRIPTED_GOLDEN,
        "the view's rows are the script's pinned rows"
    );
    assert_eq!(
        only(overlap_rows_without(g.slog().log(), None), &EXACT),
        view,
        "CONTROL: the grammar over the log's records is the view"
    );
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
            view,
            "a projection with {key:?} dropped agrees"
        );
    }
}

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
/// person's burst, a delivered mail, then the same person typing on inside
/// that 30 s burst, twice ([`drive_human_at`]): the view keeps one `human`
/// row (the burst's); the log holds its mark and one more `human` mark, the
/// mail latch's (the first input after the mail), which says `why=mail`
/// and is no row; the input after that writes neither.
#[test]
fn the_human_rows_are_the_burst_marks_and_the_latchs_own_says_why() {
    let (h, _seen) = drive_human_at(61);
    let g = h.ctx.timeline.lock().unwrap();
    let human = ["human"];
    let log = g.slog().log();
    let view = only(overlap_rows(log), &human);
    assert_eq!(view.len(), 1, "the view keeps the burst rule: {view:?}");
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
        view,
        "NEGATIVE CONTROL: without why= the grammar shows the latch's mark as a row"
    );
}

/// A PERSON'S BURST ROW RINGS THE SESSION'S WATCHERS ONCE (6G-5): a burst's
/// `human` mark is a row no timeline writer wrote, so the person's key
/// ([`crate::app_input::note_person`], `deliver_noted`'s path) rings the
/// session's watchers for it (`SessionTimeline::ring_rows`), once, and a
/// `subscribe … events` watch learns of the row then; the next key inside
/// the burst writes no row and rings nothing.
#[test]
fn a_persons_burst_row_rings_the_sessions_watchers_once() {
    let (h, _store) = rig(63);
    let ctx = &h.ctx;
    let watch = crate::session_watch::Subscription::single(&ctx.watchers);
    let rung = ctx.watchers.records_rung();
    let head = ctx.timeline.lock().unwrap().view_head();
    let _ = crate::app_input::note_person(ctx, crate::human_input::Gesture::Other);
    let kinds: Vec<&str> = ctx
        .timeline
        .lock()
        .unwrap()
        .since(head)
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, ["human"], "the burst's mark is a row");
    assert_eq!(
        ctx.watchers.records_rung(),
        rung + 1,
        "the burst's row rang the watchers once"
    );
    assert!(watch.wait(Duration::ZERO), "and woke the watch");
    let _ = crate::app_input::note_person(ctx, crate::human_input::Gesture::PlainEnter);
    assert_eq!(
        ctx.watchers.records_rung(),
        rung + 1,
        "CONTROL: a key inside the burst writes no row and rings nothing"
    );
    assert!(!watch.wait(Duration::ZERO), "nor wakes the watch");
}

/// What one driven call wrote: the view's rows, and the log's records at
/// the content tier (`recs`, as lines; `log`, as records).
#[derive(Clone, Debug, Default)]
struct Seen {
    rows: Vec<(String, String)>,
    recs: Vec<String>,
    log: Vec<Rec>,
}

impl Seen {
    fn extend(&mut self, more: Seen) {
        self.rows.extend(more.rows);
        self.recs.extend(more.recs);
        self.log.extend(more.log);
    }
}

fn during(tl: &Mutex<SessionTimeline>, f: impl FnOnce()) -> Seen {
    let (id, seq) = {
        let g = tl.lock().unwrap();
        (g.view_head(), g.slog().log().last_seq())
    };
    f();
    seen_since(tl, id, seq)
}

/// What `tl` holds past row id `id` and log seq `seq`.
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

fn rig(local: u64) -> (SessionHandle, Store) {
    let h = test_handle(local);
    let store = new_store();
    store.write().unwrap().register(h.clone());
    (h, store)
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

fn drive_title() -> Seen {
    drifted(&[
        b"\x1b]2;vim build\x07",
        "\x1b]2;\u{280b} vim build\x07".as_bytes(),
        "\x1b]2;\u{2819} vim build\x07".as_bytes(),
        b"\x1b]2;make test\x07",
        b"\x1b]2;\x07",
    ])
}

fn drive_title_at_its_bounds() -> Seen {
    let (h, _store) = rig(71);
    let long = format!("vim {}end", "t\u{e9} ".repeat(80));
    during(&h.ctx.timeline, || {
        let mut g = h.ctx.timeline.lock().unwrap();
        assert!(g.record_title(&long));
        assert!(g.record_title("vim\u{200b} hidden"));
    })
}

fn drive_cwd() -> Seen {
    drifted(&[
        b"\x1b]7;file://localhost/tmp/a%20b\x07",
        b"\x1b]7;file://localhost/tmp/c\x07",
        b"\x1b]7;file://localhost/tmp/c\x07",
    ])
}

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

fn drive_cwd_at_its_bounds() -> Seen {
    let (h, _store) = rig(72);
    let long = format!("/tmp/{}end", "d \u{e9}/".repeat(80));
    during(&h.ctx.timeline, || {
        let mut g = h.ctx.timeline.lock().unwrap();
        g.record_cwd_change(Some(&long));
        g.record_cwd_change(Some("/tmp/hidden\u{200b}dir"));
    })
}

fn meta_lines(h: &SessionHandle, store: &Store, lines: &[&str]) {
    for line in lines {
        let reply = crate::control::meta_line_for_test(store, &h.term, h.local_id, &h.ctx, line);
        assert_eq!(reply, "OK\n", "{line}");
    }
}

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
/// predecessor's records: no row is carried), then a shell the PTY
/// keeper held through a crash: each one's handoff and registration.
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

fn program_report(
    state: aterm_core::terminal::program_state::ProgramStateWord,
    kind: Option<aterm_core::terminal::program_state::ProgramBlockedKind>,
) -> aterm_core::terminal::program_state::ProgramState {
    aterm_core::terminal::program_state::ProgramState {
        state,
        kind,
        app: aterm_core::terminal::program_state::ProgramApp::Codex,
    }
}

/// THE PROGRAM MARKS ARE THE ROWS UNDER THEIR RETENTION RULE (6F-4; U39,
/// U44): moves a second apart (under the run's bucket) are a row each, in
/// order; a burst of 60 at one instant is past it, so the view holds fewer
/// rows than moves, and once a token frees its newest row is the newest
/// state, the states it replaced counted on the mark (`coalesced=`).
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
    let rows = |g: &SessionTimeline| -> Vec<String> {
        g.since(None)
            .filter(|e| e.kind == crate::session_timeline::PROGRAM_STATE)
            .map(|e| e.payload.to_string())
            .collect()
    };
    for (i, (state, kind)) in moves.into_iter().enumerate() {
        hand.set(1_000 * (i as u64 + 1));
        let mut g = h.ctx.timeline.lock().unwrap();
        g.note_program_state(Some(program_report(state, kind)), 1);
    }
    assert_eq!(
        rows(&h.ctx.timeline.lock().unwrap()),
        [
            "state=working kind=-",
            "state=blocked kind=question",
            "state=working kind=-",
            "state=done kind=-",
            "state=idle kind=-",
            "state=error kind=-",
        ],
        "under the bucket: a row a move"
    );
    hand.set(60_000);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        for i in 0..60 {
            let state = if i % 2 == 0 { W::Working } else { W::Blocked };
            let kind = (state == W::Blocked).then_some(ProgramBlockedKind::Auth);
            g.note_program_state(Some(program_report(state, kind)), 1);
        }
    }
    let newest = |g: &SessionTimeline| rows(g).last().cloned();
    let before = rows(&h.ctx.timeline.lock().unwrap()).len();
    assert_eq!(
        before,
        moves.len() + crate::session_log::observed::PROGRAM_BURST as usize,
        "NEGATIVE CONTROL: before a token frees, the burst is rows and the newest state pending"
    );
    hand.set(61_000);
    let mut g = h.ctx.timeline.lock().unwrap();
    let at = g.slog().now();
    g.slog_mut().program_tick(at);
    assert_eq!(rows(&g).len(), before + 1, "a token frees: one row more");
    assert_eq!(
        newest(&g).as_deref(),
        Some("state=blocked kind=auth"),
        "past it: the newest row is the newest state"
    );
    let marked = g
        .slog()
        .log()
        .window()
        .rfind(|r| r.k == Kind::Program)
        .map(|r| aterm_slog::encode(r, Tier::Content).expect("fits"))
        .expect("a program mark");
    assert!(marked.contains(" coalesced="), "{marked}");
    assert!(
        rows(&g).len() < moves.len() + 60,
        "fewer rows than moves past the bucket"
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

fn post(h: &SessionHandle) {
    let reply = crate::fabric::cmd_post(
        &h.ctx,
        "to=@s-peer kind=ask dl=60000 key=k1 --wait=0 SENTINEL-POST",
        None,
    );
    assert!(queued(&reply, 1), "{reply}");
}

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

fn post_again(h: &SessionHandle) {
    let reply = crate::fabric::cmd_post(&h.ctx, "to=@s-peer kind=note --wait=0 SENTINEL", None);
    assert!(queued(&reply, 2), "{reply}");
}

/// Whether `post`'s reply says post `id` is queued, whatever the link: `OK <id>`, or a refusal of the wait that names the queued id (`ERR fabric absent id=<id>` with no bridge, `ERR timeout id=<id>` while a parallel test's link section has one up).
pub(super) fn queued(reply: &str, id: u64) -> bool {
    let named = format!("id={id}");
    reply.starts_with(&format!("OK {id}")) || reply.split_whitespace().any(|t| t == named)
}

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

/// A bridge attached to `store` and its link up: the fabric's `connected` (inside [`crate::fabric::with_link_reset`]).
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

/// THE FETCH'S OWN WRITER (`inbox get @<off>`): a read the ring cannot answer parks, writing its `fetch` row as it parks (the watchers' ring a one-minute hang detector), and the bridge's `fetched= err=no-record` answer ends it (inside [`crate::fabric::with_link_reset`]).
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

fn longest(class: &str, fill: &str) -> String {
    format!(
        "{class}-{}",
        fill.repeat(aterm_types::fabric_grammar::PRINCIPAL_NAME_MAX)
    )
}

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
/// address (`@s-<32>@n-<32>`), of the longest kind, `re=` and `dl=` at their
/// widest, through sixteen hops of the longest principal (559 bytes) and
/// with a 64-byte key — a record the hops leave no room in, so they are cut
/// there, its row cut by the timeline's 256-byte payload rule — then a broadcast whose row
/// shows 233 bytes of the same hops.
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

const MODES_ON: &[u8] = b"\x1b[>1u\x1b[?1049h\x1b[>1u\x1b[?1000h\x1b[?1006h\x1b[?1004h\
    \x1b[?2048h\x1b[?2031h\x1b[?1007l\x1b[>4;2m\x1b[>4;1f\x1b[?1h\x1b=\x1b[?67h\x1b[?1035h\
    \x1b[?1036h\x1b[?1039h\x1b[4h\x1b[20h\x1b[?5h\x1b(0\x0e\x1b[?25l\x1b]8;;http://x\x1b\\\
    \x1b[?6h\x1b[?69h\x1b[2;10s\x1b[2;10r\x1b]4;1;rgb:00/00/00\x1b\\\x1b[?2026h\x1b[1";

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

/// A kind that left the ring's named differences for [`EXACT`] (phase 6,
/// 6F-5 to 6F-7), with its row's own writer, driven, a key its projection
/// cannot do without (the negative control's), and the rows that drive
/// writes.
struct Driven {
    row: &'static str,
    drive: fn() -> Seen,
    needs: Key,
    /// The drive's [`EXACT`] rows, `<kind> <payload>` a line, oldest first
    /// ([`as_lines`]): the ring's own rows at 5b3bb8c388 (6G-5's parent,
    /// where each writer formatted its ring row itself and the ring was
    /// held to the view), so a row's bytes are pinned by an oracle the
    /// view's grammar did not write.
    golden: &'static str,
}

/// `rows` as `<kind> <payload>` lines, oldest first.
fn as_lines(rows: &[(String, String)]) -> String {
    rows.iter().map(|(k, p)| format!("{k} {p}\n")).collect()
}

const DRIVEN: &[Driven] = &[
    Driven {
        row: "state-change",
        drive: drive_state,
        needs: Key::State,
        golden: concat!(
            "state-change state=spawning\n",
            "state-change state=alive\n",
            "state-change state=exited\n",
            "closing reason=shell-exit by=-\n",
            "state-change state=closed\n"
        ),
    },
    Driven {
        row: crate::harness_policy_verb::HARNESS_POLICY_EVENT,
        drive: drive_policy,
        needs: Key::Gen,
        golden: "harness-policy gen=0\n",
    },
    Driven {
        row: "hold-advisory",
        drive: drive_advisory,
        needs: Key::Reason,
        golden: "hold-advisory 1 reason=fabric-lost origin=fleet\n",
    },
    Driven {
        row: "fetch",
        drive: drive_fetch,
        needs: Key::Off,
        golden: "fetch off=7\n",
    },
    Driven {
        row: "topic",
        drive: drive_topic,
        needs: Key::TopicName,
        golden: concat!("topic add news since=head\n", "topic drop news\n"),
    },
    Driven {
        row: "inbox-seen",
        drive: drive_inbox_seen,
        needs: Key::Seen,
        golden: concat!(
            "inbox-seen 1 off=7 floor=9 seen=1 verdict=handled kind=task from=h-andrew\n",
            "inbox-seen 1 off=7 floor=9 seen=1\n",
            "inbox-seen 2 off=9 floor=10 seen=2\n"
        ),
    },
    Driven {
        row: "post",
        drive: drive_post,
        needs: Key::Idem,
        golden: concat!(
            "post 1 to=@s-peer kind=ask dl=60000 key=k1\n",
            "post 2 to=say:builds kind=note re=7 via=n-lab\n"
        ),
    },
    Driven {
        row: "post-landed",
        drive: drive_landed,
        needs: Key::Off,
        golden: concat!("post-landed 1 off=90\n", "post-landed 2 off=-\n"),
    },
    Driven {
        row: "hold",
        drive: drive_hold,
        needs: Key::Reason,
        golden: concat!(
            "hold 1 reason=demo origin=local hold_origin=local\n",
            "hold 0 reason=- origin=local hold_origin=-\n"
        ),
    },
    Driven {
        row: "spawned",
        drive: drive_spawned,
        needs: Key::State,
        golden: "spawned state=alive\n",
    },
    Driven {
        row: "closing",
        drive: drive_closing,
        needs: Key::Closer,
        golden: concat!(
            "closing reason=ctl-close by=s-driver\n",
            "state-change state=closed\n"
        ),
    },
    Driven {
        row: "render",
        drive: drive_render,
        needs: Key::Run,
        golden: concat!(
            "render desync-risk run=1 displaced=+2 net=zero at=3\n",
            "render healed run=1 after_ms=40\n",
            "render desync-risk run=2 displaced=-3 net=changed at=50\n",
            "render desync-risk epoch=4 displaced=-2 pushed=1 cut=6 drew=1 class=learned at=70\n",
            "render healed epoch=4 after_ms=80 by=undo\n",
            "render healed epoch=5 after_ms=90 by=-\n"
        ),
    },
    Driven {
        row: "modes-restored",
        drive: drive_modes,
        needs: Key::Reverted,
        golden: concat!(
            "modes-restored reason=manual source=menu reverted=mouse,paste bytes=12\n",
            "modes-restored reason=manual source=ctl reverted=- bytes=0\n",
            "modes-restored from=300 to=301 program=- reverted=alt,kitty bytes=42\n",
            "modes-restored from=300 to=301 program=- reverted= bytes=0\n"
        ),
    },
    Driven {
        row: "in-doubt",
        drive: drive_in_doubt,
        needs: Key::KeySeq,
        golden: concat!(
            "in-doubt producer=3 seq=9 reply=ERR-write\n",
            "in-doubt producer=3 seq=10 reply=\n"
        ),
    },
    Driven {
        row: "handoff",
        drive: drive_handoff,
        needs: Key::Build,
        golden: concat!(
            "handoff carried=0 from_build=7\n",
            "spawned state=alive\n",
            "handoff carried=0 from_build=7\n",
            "spawned state=alive\n",
            "handoff carried=0 from_build=9\n",
            "spawned state=alive\n"
        ),
    },
    Driven {
        row: crate::session_timeline::TITLE_CHANGE,
        drive: drive_title,
        needs: Key::Title,
        golden: concat!(
            "title-change title=vim%20build\n",
            "title-change title=%E2%A0%8B%20vim%20build\n",
            "title-change title=make%20test\n",
            "title-change title=\n"
        ),
    },
    Driven {
        row: crate::session_timeline::CWD_CHANGE,
        drive: drive_cwd,
        needs: Key::Cwd,
        golden: concat!("cwd-change cwd=/tmp/a%20b\n", "cwd-change cwd=/tmp/c\n"),
    },
    Driven {
        row: crate::session_timeline::CWD_CHANGE,
        drive: drive_cwd_successor,
        needs: Key::Cwd,
        golden: concat!("cwd-change cwd=/tmp/c\n", "cwd-change cwd=-\n"),
    },
    Driven {
        row: crate::session_timeline::META_CHANGE,
        drive: drive_meta,
        needs: Key::AttnText,
        golden: concat!(
            "meta-change field=attention value=look%20here\n",
            "meta-change field=attention value=look%20again\n",
            "meta-change field=attention value=-\n",
            "meta-change field=role value=builder\n"
        ),
    },
    Driven {
        row: crate::session_timeline::META_CHANGE,
        drive: drive_autopilot,
        needs: Key::LandAt,
        golden: concat!(
            "meta-change field=autopilot value=cruise land_at=1900000000\n",
            "meta-change field=autopilot value=cruise land_at=1900000060\n",
            "meta-change field=autopilot value=cruise\n",
            "meta-change field=autopilot value=off\n"
        ),
    },
    Driven {
        row: crate::session_timeline::META_CHANGE,
        drive: drive_meta_successor,
        needs: Key::Value,
        golden: concat!(
            "meta-change field=description value=build\n",
            "meta-change field=role value=builder\n",
            "meta-change field=attention value=look%20here\n",
            "meta-change field=role value=lead\n"
        ),
    },
    Driven {
        row: "human",
        drive: drive_human,
        needs: Key::Why,
        golden: concat!("human \n", "inbox 1 from=h-andrew kind=task off=9\n"),
    },
    Driven {
        row: "inbox",
        drive: drive_mail_at_its_bounds,
        needs: Key::From,
        golden: concat!(
            "inbox 1 from=s-cccccccccccccccccccccccccccccccc@n-dddddddddddddddddddddddddddddddd kind=ask off=7\n",
            "inbox-seen 1 off=7 floor=8 seen=1 verdict=handled kind=ask from=s-cccccccccccccccccccccccccccccccc@n-dddddddddddddddddddddddddddddddd\n"
        ),
    },
    Driven {
        row: "post",
        drive: drive_post_at_its_bounds,
        needs: Key::Via,
        golden: concat!(
            "post 1 to=@s-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa@n-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb kind=control re=18446744073709551615 dl=18446744073709551615 via=n-vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv,n-vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv,n-vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv,n-vvvvvvvv\n",
            "post 2 to=say kind=note via=n-vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv,n-vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv,n-vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv,n-vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv,n-vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv,n-vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv,n-vvvvvvvvvvvvvvvvvvvvv\n"
        ),
    },
    Driven {
        row: "hold",
        drive: drive_hold_at_its_bounds,
        needs: Key::Reason,
        golden: concat!(
            "hold 1 reason=%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25 origin=local hold_origin=local\n",
            "hold 1 reason=%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9%E9ab origin=fleet hold_origin=fleet,local\n",
            "hold 1 reason=%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25%25 origin=local hold_origin=local\n",
            "hold 0 reason=- origin=local hold_origin=-\n"
        ),
    },
    Driven {
        row: "in-doubt",
        drive: drive_in_doubt_at_its_bounds,
        needs: Key::Reply,
        golden: concat!(
            "in-doubt producer=3 seq=11 reply=ERR-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n",
            "in-doubt producer=3 seq=12 reply=ERRhidden-words\n"
        ),
    },
    Driven {
        row: "modes-restored",
        drive: drive_modes_at_its_bounds,
        needs: Key::Reverted,
        golden: "modes-restored from=300 to=301 program=- reverted=parser,sync,kitty-alt,alt,kitty,mouse,mouse-encoding,focus,size-reports,color-scheme-reports,mok,fok,cursor-keys,keypad,backarrow,meta-sends-escape,insert,newline,reverse-video,g0,gl,cursor,margins,region bytes=137\n",
    },
    Driven {
        row: crate::session_timeline::TITLE_CHANGE,
        drive: drive_title_at_its_bounds,
        needs: Key::Title,
        golden: concat!(
            "title-change title=vim%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t%C3%A9%20t\n",
            "title-change title=vim%20hidden\n"
        ),
    },
    Driven {
        row: crate::session_timeline::CWD_CHANGE,
        drive: drive_cwd_at_its_bounds,
        needs: Key::Cwd,
        golden: concat!(
            "cwd-change cwd=/tmp/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20%C3%A9/d%20\n",
            "cwd-change cwd=/tmp/hiddendir\n"
        ),
    },
    Driven {
        row: crate::session_timeline::META_CHANGE,
        drive: drive_meta_at_its_bounds,
        needs: Key::Value,
        golden: concat!(
            "meta-change field=description value=d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d\n",
            "meta-change field=attention value=d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3%A9%20d%C3\n"
        ),
    },
];

/// EVERY EXACT KIND IS WRITTEN THROUGH ITS OWN WRITER (phase 6, 6F-5 to
/// 6F-8; on the view since 6G-5): each [`DRIVEN`] kind's writer, driven as
/// production runs it, writes a row of its kind; the view's rows are the
/// drive's pinned rows byte for byte (`golden`, the ring's at 6G-5's
/// parent: the oracle the deleted ring was); they are the grammar over the
/// records it wrote, row for record and in order; and the row reads the
/// key its mark carries (dropped, the rows differ).
#[test]
fn every_exact_kind_is_written_through_its_own_writer() {
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
        } else if as_lines(&rows) != d.golden {
            errs.push(format!(
                "{}: the view's rows\n{}are not the pinned rows\n{}",
                d.row,
                as_lines(&rows),
                d.golden
            ));
        } else if log != rows {
            errs.push(format!(
                "{}: the log's records {log:?} against the view's rows {rows:?} ({:?})",
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

/// THE TABLE IS WHOLE: every kind of §0.1 is an exact pair or
/// `program-state` (the one kind under a retention rule, U39), once; the
/// kinds are the ones 5B-5's script writes (one row of each, the goldens'
/// script), and every fabric kind the code lists
/// ([`crate::fabric::FABRIC_EVENT_KINDS`]) is among them.
#[test]
fn every_timeline_kind_is_an_exact_pair_or_the_retained_program_state() {
    let mut named: Vec<&str> = EXACT
        .iter()
        .copied()
        .chain([crate::session_timeline::PROGRAM_STATE])
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

/// Rows with their ids, `(id, kind, payload)`, oldest first.
type IdRows = Vec<(u64, String, String)>;

/// THE VIEW's rows ([`crate::session_timeline_view::timeline_rows`]).
fn view_ids(log: &Log) -> IdRows {
    crate::session_timeline_view::timeline_rows(log)
        .into_iter()
        .map(|e| (e.id, e.kind.to_string(), e.payload.to_string()))
        .collect()
}

/// A published status transition, as the status sweep records it: the
/// log's mark, which is the row.
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
    let at = g.slog().now();
    g.slog_mut()
        .observe_status(crate::session_status::status_facts(&status), at);
    g.record(crate::session_status::STATUS_CHANGE);
}

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
/// `round`: a directory, the agent's verdict, the title, a task through
/// `task mark`, a person's burst (its mark the row), a
/// self-claim's act, a status transition, a program report, the
/// supervisor's attention and a delivered mail.
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
        g.note_program_state(Some(program_report(state, None)), 1);
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

/// THE SUCCESSOR'S VIEW STARTS PAST ITS PREDECESSOR'S, carried and not,
/// and a crash adoption's at 1 (6G-1's ids, on the view since 6G-5): the
/// successor's first row, its `handoff`, takes an id past every id the
/// predecessor handed out (the count its manifest carried); a crash
/// adoption's first row is 1.
#[test]
fn the_successors_view_starts_past_its_predecessors_and_a_crash_adoptions_at_one() {
    for carry in [true, false] {
        let (h, store) = rig(72);
        let hand = super::super::tests::hand_clock(&h.ctx.timeline);
        every_writer(&h, &store, 0);
        let pred_high = h.ctx.timeline.lock().unwrap().high_id().expect("rows");
        hand.set(5_000);
        let (next, next_store, _next_hand) = hand_over(&h, &store.read().unwrap(), 10_000, carry);
        let shared: Store = Arc::new(std::sync::RwLock::new(next_store));
        every_writer(&next, &shared, 1);
        let g = next.ctx.timeline.lock().unwrap();
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
    let first = &view_ids(g.slog().log())[0];
    assert_eq!(
        (first.0, first.1.as_str()),
        (1, "handoff"),
        "a crash adoption's ids start at 1"
    );
}

/// What the `events` digest tells a watcher at `after`
/// ([`crate::subscribe::drain_timeline_events`]): its `GAP
/// events-dropped=` count, 0 for none.
fn digest_gap(tl: &SessionTimeline, after: Option<u64>) -> u64 {
    let mut out = String::new();
    let _ = crate::subscribe::drain_timeline_events(tl, "7", after, &mut out);
    out.lines()
        .find_map(|l| l.strip_prefix("GAP 7 events-dropped="))
        .map_or(0, |n| n.parse().expect("a count"))
}

/// Every reader's id in `anchors` against the linear reading of the whole
/// view: shown every row past its id (every row for none, or for an id past
/// the log's head, U13), by id; and told lost every id in between it was
/// not shown that the log issued (from the log's first row id), plus the
/// ids an anchor below that first id jumps; and the digest tells it that
/// same count.
fn readers_against_the_linear_reading(
    tl: &SessionTimeline,
    case: &str,
    anchors: impl IntoIterator<Item = Option<u64>>,
) -> Vec<String> {
    let log = tl.slog().log();
    let whole = view_ids(log);
    let head = log.row_head();
    let mut wrong = Vec::new();
    for after in anchors {
        let view = crate::session_timeline_view::timeline_rows_since(log, after);
        let rows: IdRows = view
            .rows
            .iter()
            .map(|e| (e.id, e.kind.to_string(), e.payload.to_string()))
            .collect();
        let from = after.filter(|&a| a <= head);
        let want: IdRows = whole
            .iter()
            .filter(|r| from.is_none_or(|a| r.0 > a))
            .cloned()
            .collect();
        if rows != want {
            wrong.push(format!(
                "{case} after {after:?}: shown {} rows from {:?}, the linear reading {} from {:?}",
                rows.len(),
                rows.first().map(|r| r.0),
                want.len(),
                want.first().map(|r| r.0)
            ));
        }
        let first = log.row_first();
        let issued = (first.max(from.unwrap_or(0) + 1)..=head)
            .filter(|id| !whole.iter().any(|r| r.0 == *id))
            .count() as u64;
        let jumped = match from {
            Some(a) if log.rows_issued() > 0 => first.saturating_sub(1).saturating_sub(a),
            _ => 0,
        };
        if view.lost != issued + jumped {
            wrong.push(format!(
                "{case} after {after:?}: told lost={}, the linear reading {}",
                view.lost,
                issued + jumped
            ));
        }
        let gap = digest_gap(tl, after);
        if gap != view.lost {
            wrong.push(format!(
                "{case} after {after:?}: the view tells lost={}, the digest GAP {gap}",
                view.lost
            ));
        }
    }
    wrong
}

/// EVERY READER IS TOLD ITS ROWS AND ITS LOSSES (6G-2, the review of
/// 6G-2; held to the ring until 6G-5, now to the linear reading): across a
/// seamless update, carried and not, and a keeper's crash recovery, a
/// reader at every id (none, each of the predecessor's, the reserve
/// between, each of the successor's, its head and past it, and a crashed
/// window's 65,536 above) is shown the rows past it and told the ids it
/// lost, as the digest's GAP: the update's jump past the predecessor's id
/// is a loss of `HANDOFF_ID_RESERVE` (`events-dropped=65536`, so a watcher
/// re-reads its topics, T22), and a crashed window's id above the
/// recovery's head reads from its first row (U13).
#[test]
fn the_view_tells_every_reader_its_rows_and_its_losses() {
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
            digest_gap(&g, Some(pred_high)),
            HANDOFF_ID_RESERVE,
            "carry {carry}: the digest tells the update's jump"
        );
        let anchors = [None, Some(0)]
            .into_iter()
            .chain((1..=pred_high + 1).map(Some))
            .chain([low - 2, low - 1].map(Some))
            .chain((low..=head + 1).map(Some))
            .chain([Some(head + HANDOFF_ID_RESERVE)]);
        wrong.extend(readers_against_the_linear_reading(
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
    let all = crate::session_timeline_view::timeline_rows_since(g.slog().log(), None);
    assert_eq!(
        crate::session_timeline_view::timeline_rows_since(g.slog().log(), Some(crashed)),
        all,
        "the crashed window's anchor reads from the recovery's first row"
    );
    let anchors = [None, Some(0)]
        .into_iter()
        .chain((1..=head + 1).map(Some))
        .chain([Some(crashed), Some(500)]);
    wrong.extend(readers_against_the_linear_reading(
        &g,
        "crash recovery",
        anchors,
    ));
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

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

/// THE VIEW OF A CLOSED SESSION ENDS AT ITS CLOSE, over three rounds of
/// every writer (each wrote its row): its close's two rows (`closing`, the
/// last `state-change`) the session close's two ids.
#[test]
fn the_view_of_a_closed_session_ends_at_its_close() {
    let (h, store) = rig(74);
    for round in 0..3 {
        every_writer(&h, &store, round);
    }
    let _ = store
        .write()
        .unwrap()
        .deregister_local_as(74, ExitReason::CtlClose, ExitActor::Ctl);
    let g = h.ctx.timeline.lock().unwrap();
    let view = view_ids(g.slog().log());
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
            view.iter().any(|r| r.1 == kind),
            "every writer wrote: no {kind} row in {view:?}"
        );
    }
    let tail: Vec<&str> = view[view.len() - 2..]
        .iter()
        .map(|r| r.1.as_str())
        .collect();
    assert_eq!(tail, ["closing", "state-change"]);
    let close = g.slog().log().rows().next_back().expect("the close's row");
    assert_eq!((close.rec.op, close.span), (Op::Close, 2));
    assert_eq!(close.id, view[view.len() - 2].0);
}

/// THE VIEW UNDER A FLOOD (6G-2 over 6G-1's floor; held to the ring until
/// 6G-5): 10^4 status changes, 600 directory moves and 300 program flaps
/// (one each 430 ms, past the run's bucket), interleaved with 1500 runs,
/// then 500 runs more: the view keeps the floor's 512 rows, and its newest
/// row of each kind is that kind's last move, the program's once the
/// pending state is written.
#[test]
fn the_view_keeps_its_newest_rows_under_a_flood() {
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
        g.note_program_state(Some(program_report(state, kind)), 1);
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
    let view = view_ids(g.slog().log());
    let floor = aterm_slog::row_floor(crate::session_log::ring_limits()).records;
    assert!(
        view.len() >= floor,
        "{} rows, the floor {floor}",
        view.len()
    );
    let newest = |kind: &str| {
        view.iter()
            .rev()
            .find(|r| r.1 == kind)
            .map(|r| r.2.as_str())
    };
    assert_eq!(
        newest(crate::session_timeline::PROGRAM_STATE),
        Some("state=blocked kind=auth"),
        "the newest program row is the newest state, once the pending one is written"
    );
    assert_eq!(
        newest(crate::session_status::STATUS_CHANGE),
        Some("phase=running outcome=none exit_code=- signal=- confidence=exact"),
        "the newest status row is the last move"
    );
    assert_eq!(
        newest(crate::session_timeline::CWD_CHANGE),
        Some("cwd=/tmp/x/599"),
        "the newest directory row is the last move"
    );
}

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
/// the review of 6F-7 named): a burst of 40 acts at one instant, past the
/// actor's mark bucket, writes a `harness-act` row for each act the log
/// recorded and none for one the limiter refused, so the view's rows are
/// the recorded acts, row for record.
#[test]
fn an_act_the_limiter_refuses_is_no_row() {
    let (h, _store) = rig(77);
    let _hand = super::super::tests::hand_clock(&h.ctx.timeline);
    let mut g = h.ctx.timeline.lock().unwrap();
    let ids: Vec<Option<u64>> = (0..40)
        .map(|_| g.note_harness_act("approve", "codex-alab@1", Actor::Ctl))
        .collect();
    let rows: Vec<u64> = g
        .since(None)
        .filter(|e| e.kind == crate::session_timeline::HARNESS_ACT)
        .map(|e| e.id)
        .collect();
    let acts = g.slog().log().window().filter(|r| r.k == Kind::Act).count();
    assert!(acts < 40, "CONTROL: the limiter refused some: {acts}");
    assert_eq!(
        rows.len(),
        acts,
        "a row for each recorded act, none for the rest"
    );
    let named: Vec<u64> = ids.into_iter().flatten().collect();
    assert_eq!(named, rows, "each recorded act names its own row");
}

/// A SUPERVISED SESSION'S ACT CLAIMS ARE NO ROWS (forever r11, Finding 5:
/// the view printed `harness-act act=- holder=-` for each claim, where the
/// ring 6G-5 deleted had none). The harness that supervises a session writes
/// each act as a `mark - act word= rule=` claim through the `mark` verb,
/// between the status sweep's transitions; a self-claimed agent's act goes
/// through `meta set harness_act`, the one writer of `harness_act=`. So the
/// view's one `harness-act` row is the self-claim's, every reader reads the
/// same rows, and every transition is a row.
#[test]
fn a_supervised_sessions_act_claims_are_no_rows_of_the_view() {
    use crate::session_timeline::HARNESS_ACT;
    let (h, _store) = rig(83);
    let claims = [
        "- act word=continue rule=continue.v1",
        "- act word=dispatch rule=land.v1",
        "- act word=escalate",
    ];
    for claim in claims {
        status_move(&mut h.ctx.timeline.lock().unwrap(), true);
        let (reply, _) = crate::session_log::cmd_mark(&h.ctx, Actor::Harness, true, claim);
        assert!(reply.starts_with("OK seq="), "{claim}: {reply}");
        status_move(&mut h.ctx.timeline.lock().unwrap(), false);
    }
    let mut g = h.ctx.timeline.lock().unwrap();
    assert!(
        g.note_harness_act("approve", "codex-alab@1", Actor::Ctl)
            .is_some(),
        "CONTROL: the self-claim's act is recorded"
    );
    status_move(&mut g, true);
    let marked = g
        .slog()
        .log()
        .window()
        .filter(|r| r.k == Kind::Act && r.src == Src::Mark && r.f.get(Key::Holder).is_none())
        .count();
    assert_eq!(marked, claims.len(), "CONTROL: each claim is a record");
    let view = g.view_since(None).rows;
    assert_eq!(g.view_copy(None, 0).rows(), view);
    assert_eq!(g.view_newest_first().count(), view.len());
    let acts: Vec<&str> = view
        .iter()
        .filter(|e| e.kind == HARNESS_ACT)
        .map(|e| &*e.payload)
        .collect();
    assert_eq!(
        acts,
        ["act=approve holder=codex-alab@1"],
        "the self-claim's act alone is a harness-act row"
    );
    let statuses = view
        .iter()
        .filter(|e| e.kind == crate::session_status::STATUS_CHANGE)
        .count();
    assert_eq!(statuses, 2 * claims.len() + 1, "every transition a row");
}

/// EVERY READER READS THE VIEW: the wire copy, the seek, the head, the
/// chrome's newest-first tail and the events digest each read the view's
/// rows, a burst's `human` row (written outside the timeline's writers)
/// among them.
#[test]
fn every_reader_reads_the_view() {
    let (h, store) = rig(81);
    every_writer(&h, &store, 0);
    every_writer(&h, &store, 1);
    let mut g = h.ctx.timeline.lock().unwrap();
    let at = g.slog().now();
    g.slog_mut().human_input(true, at);
    let view = crate::session_timeline_view::timeline_rows(g.slog().log());
    assert_eq!(
        view.last().map(|e| e.kind),
        Some("human"),
        "PRECONDITION: the burst's row is the view's newest"
    );
    let readers = |g: &SessionTimeline| {
        let tail: Vec<(&str, u64)> = g.view_newest_first().collect();
        let tail: Vec<(String, u64)> = tail.into_iter().map(|(k, t)| (k.to_string(), t)).collect();
        (
            g.view_copy(None, 0).rows(),
            g.view_since(None).rows,
            g.view_head(),
            tail,
        )
    };
    let want = |view: &[crate::session_timeline::TimelineEvent]| {
        (
            view.to_vec(),
            view.to_vec(),
            view.last().map(|e| e.id),
            view.iter()
                .rev()
                .map(|e| (e.kind.to_string(), e.t_ms))
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(readers(&g), want(&view));
    let mut frames = String::new();
    let _ = crate::subscribe::drain_timeline_events(&g, "7", None, &mut frames);
    assert!(frames.ends_with("EVENT 7 human\n"), "{frames}");
    // NEGATIVE CONTROL: a row past the reading is in every reader, not in it.
    status_move(&mut g, false);
    assert_ne!(readers(&g), want(&view), "NEGATIVE CONTROL: a newer row");
    let view = crate::session_timeline_view::timeline_rows(g.slog().log());
    assert_eq!(readers(&g), want(&view));
}

#[test]
fn every_row_only_kind_is_a_row_mark_the_view_prints_whole() {
    use crate::session_timeline_view::ROW_ONLY;
    let (h, _store) = rig(84);
    let mut g = h.ctx.timeline.lock().unwrap();
    for (i, kind) in ROW_ONLY.into_iter().enumerate() {
        let before = g.view_head();
        let payload = format!("n={i} tier=warm age_ms=12 at%20b QZP6ROW{i}");
        g.record_row(kind, payload.clone());
        let shown = g.view_since(before);
        let rows: Vec<(u64, &str, &str)> = shown
            .rows
            .iter()
            .map(|e| (e.id, e.kind, &*e.payload))
            .collect();
        assert_eq!(
            rows,
            [(before.unwrap_or(0) + 1, kind, payload.as_str())],
            "{kind}"
        );
        let rec = g.slog().log().window().last().cloned().expect("its mark");
        assert_eq!((rec.op, rec.k), (Op::Mark, Kind::Row), "{kind}");
        assert_eq!(
            rec.f.get_str(Key::RowKind),
            Some(crate::session_timeline_view::row_token(kind).as_str())
        );
        let host = aterm_slog::encode(&rec, Tier::Host).expect("encodes");
        assert!(
            !host.contains("QZP6ROW") && !host.contains("row_text"),
            "{host}"
        );
        let mut frames = String::new();
        let _ = crate::subscribe::drain_timeline_events(&g, "7", before, &mut frames);
        let want = match kind {
            crate::sys_host::JOB_CHANGE => format!("EVENT 7 job {payload}\n"),
            crate::occupancy_wakes::WAKE_READY => format!("EVENT 7 wake {payload}\n"),
            _ => String::new(),
        };
        assert_eq!(frames, want, "{kind}");
    }
    let before = g.view_head();
    let at = g.slog().now();
    let fields = aterm_slog::Fields::new()
        .tok(Key::RowKind, "no-such-row")
        .and_then(|f| f.text(Key::RowText, "x=1"))
        .expect("fields");
    g.slog_mut()
        .log_mut_for_tests()
        .mark(
            aterm_slog::Mark::new(Kind::Row, aterm_slog::Src::Aterm, aterm_slog::Conf::Exact)
                .at(Node::SESSION)
                .fields(fields),
            at,
        )
        .expect("a row mark no kind spells");
    assert_eq!(g.view_head(), before, "no id for a token no kind spells");
    let rec = g.slog().log().window().last().cloned().expect("the mark");
    let mut leaky = rec.clone();
    leaky.f = aterm_slog::Fields::new()
        .text(Key::Text, "QZP6ROW1")
        .expect("a host text");
    assert!(
        aterm_slog::encode(&leaky, Tier::Host)
            .expect("encodes")
            .contains("QZP6ROW1")
    );
}

fn app_timeline(app: &App) -> Arc<Mutex<SessionTimeline>> {
    app.store
        .read()
        .unwrap()
        .by_local(0)
        .expect("session 0")
        .ctx
        .timeline
        .clone()
}

/// THE TAB CHROME'S TAIL READS THE VIEW: a move past a floor's worth of
/// newer rows is still the menu's.
#[test]
fn the_tab_chromes_tail_reads_the_view() {
    use crate::session_chrome::TabMenuEntry;
    let mut app = App::headless_for_test();
    let tl = app_timeline(&app);
    {
        let mut g = tl.lock().unwrap();
        assert!(g.record_cwd_change(Some("/private/tmp/x")));
        let moved = g.view_head();
        let floor = aterm_slog::row_floor(crate::session_log::ring_limits()).records;
        for i in 0..=floor {
            let _ = g.record_hand_row("topic", format!("add t{i} since=head"));
        }
        assert!(
            g.since(moved).len() > floor,
            "PRECONDITION: the view holds more than the floor's rows past the move"
        );
        assert!(
            g.since(None).any(|e| e.kind == "cwd-change"),
            "PRECONDITION: the view still holds the move"
        );
    }
    let menu = app.tab_context_menu_model(WindowId(0), 0);
    assert!(
        menu.iter().any(
            |e| matches!(e, TabMenuEntry::Header(row) if row.starts_with("changed directory ·"))
        ),
        "the tail is the view's: {menu:?}"
    );
}

/// THE TAB CHROME'S CACHE KEY IS THE VIEW'S HEAD: a burst's `human` row,
/// written outside the timeline's writers, moves it.
#[test]
fn the_tab_chromes_cache_key_is_the_views_head() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let tl = app_timeline(&app);
    let titles = app.tab_titles(wid);
    let _ = app.tab_chrome_ext(wid, &titles);
    let before = app.session_chrome.get(&0).map(|c| c.high_id);
    {
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().human_input(true, at);
        assert_ne!(
            g.view_head(),
            before,
            "PRECONDITION: the burst moved the view's head"
        );
    }
    let _ = app.tab_chrome_ext(wid, &titles);
    let head = tl.lock().unwrap().view_head();
    assert!(head.is_some());
    assert_eq!(
        app.session_chrome.get(&0).map(|c| c.high_id),
        head,
        "the chrome recomposed at the view's head"
    );
}

fn by_r1(actor: &ExitActor) -> String {
    match actor {
        ExitActor::Sid(sid) if aterm_session::is_minted_sid(sid) => sid.clone(),
        ExitActor::Sid(_) | ExitActor::Unknown => "-".to_string(),
        ExitActor::Human => "human".to_string(),
        ExitActor::Ctl => "ctl".to_string(),
        ExitActor::Bridge => "bridge".to_string(),
    }
}

fn index_close(app: &App, local: u64) -> Option<(String, Option<i64>, String, Option<u64>)> {
    let source = app
        .store
        .read()
        .unwrap()
        .closed_logs()
        .by_local(local)?
        .source();
    let skeleton = matches!(source, crate::session_log::ClosedSource::Skeleton(_));
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
        skeleton.then(|| log.mono_of(r.seq)).flatten(),
    ))
}

fn exited(app: &App) -> Vec<crate::session_store::RosterRecord> {
    app.store
        .read()
        .unwrap()
        .roster_since(0)
        .filter(|r| r.change == RosterChange::Exited)
        .cloned()
        .collect()
}

/// The closed index's exits, oldest first.
fn closed_exits(app: &App) -> Vec<crate::session_log::ClosedExit> {
    app.store
        .read()
        .unwrap()
        .closed_logs()
        .exits_since(0)
        .cloned()
        .collect()
}

/// Whether `exits` (the closed index's) answer every roster `Exited` row of
/// `rows`: one slot each, in the rows' order, saying every field the row
/// does (its seq, instant, sid, local id and reason: since 6G-9 the roster
/// names no code and no closer, which the index alone holds), and each
/// exit's log's session close the same `out=` (`out_of` naming each
/// reason's), `code=` and `by=` as the exit, and, from a skeleton, the
/// row's instant. Returns how many agreed.
fn exits_agree(
    app: &App,
    rows: &[crate::session_store::RosterRecord],
    exits: &[crate::session_log::ClosedExit],
    out_of: fn(ExitReason) -> &'static str,
) -> Result<usize, String> {
    let (held, said): (Vec<u64>, Vec<u64>) = (
        exits.iter().map(|e| e.seq).collect(),
        rows.iter().map(|r| r.seq).collect(),
    );
    if held != said {
        return Err(format!(
            "the index holds seqs {held:?}, the roster {said:?}"
        ));
    }
    for (row, exit) in rows.iter().zip(exits) {
        let says = (row.t_ms, row.sid.as_str(), row.local_id, row.reason);
        let holds = (exit.t_ms, exit.sid.as_str(), exit.local_id, exit.reason);
        if says != holds {
            return Err(format!(
                "seq {}: the roster says {says:?}, the index {holds:?}",
                row.seq
            ));
        }
        let want = (
            out_of(exit.reason).to_string(),
            exit.exit_code.map(i64::from),
            by_r1(&exit.actor),
        );
        let Some((out, code, by, mono)) = index_close(app, row.local_id) else {
            return Err(format!("local {}: no log attached", row.local_id));
        };
        if (&out, &code, &by) != (&want.0, &want.1, &want.2) {
            return Err(format!(
                "local {} ({:?} by {:?}): the index says {want:?}, the log {:?}",
                row.local_id,
                exit.reason,
                exit.actor,
                (out, code, by)
            ));
        }
        if mono.is_some_and(|m| m != row.t_ms) {
            return Err(format!(
                "local {}: the close at {mono:?}, the row at {}",
                row.local_id, row.t_ms
            ));
        }
    }
    Ok(rows.len())
}

/// THE ROSTER AND THE CLOSED INDEX AGREE on every reason, the last close,
/// interleaved; the index holds each closer raw and each code, as the log's
/// close says them.
#[test]
fn the_roster_and_the_closed_index_agree_on_every_exit() {
    use crate::session_log::{ClosedKeep, ClosedSource};
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
    let mut open = |app: &mut App| {
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
        local
    };
    let close = |app: &mut App, local, reason, actor: ExitActor, keep| {
        let _closing = CloseAttribution::enter(reason, actor);
        app.retire_session_registration_keeping(local, keep);
    };
    for reason in ExitReason::ALL {
        for actor in &actors {
            let local = open(&mut app);
            close(&mut app, local, reason, actor.clone(), ClosedKeep::Skeleton);
        }
    }
    let last = open(&mut app);
    close(
        &mut app,
        last,
        ExitReason::UiClose,
        ExitActor::Human,
        ClosedKeep::Summary,
    );
    let (first, second) = (open(&mut app), open(&mut app));
    crate::app_tabs::CLOSE_BETWEEN_WRITES.with(|b| {
        *b.borrow_mut() = Some(Box::new(move |app: &mut App| {
            app.retire_session_registration_keeping(second, ClosedKeep::Skeleton);
        }));
    });
    close(
        &mut app,
        first,
        ExitReason::CtlClose,
        ExitActor::Ctl,
        ClosedKeep::Skeleton,
    );
    assert!(
        crate::app_tabs::CLOSE_BETWEEN_WRITES.with(|b| b.borrow().is_none()),
        "PRECONDITION: the second close ran between the first's writes"
    );
    let rows = exited(&app);
    let at = |local: u64| rows.iter().position(|r| r.local_id == local);
    let (Some(a), Some(b)) = (at(first), at(second)) else {
        panic!("both interleaved closes are in the roster: {rows:?}");
    };
    assert_eq!(b, a + 1, "the first's deregister minted the lower seq");
    let exits = closed_exits(&app);
    let shipped = exits_agree(&app, &rows, &exits, ExitReason::as_str);
    assert_eq!(shipped, Ok(39), "{shipped:?}");
    let closers = |exits: &[crate::session_log::ClosedExit]| -> Vec<ExitActor> {
        exits
            .iter()
            .take(ExitReason::ALL.len() * actors.len())
            .map(|e| e.actor.clone())
            .collect()
    };
    let want: Vec<ExitActor> = ExitReason::ALL
        .iter()
        .flat_map(|_| actors.iter().cloned())
        .collect();
    assert_eq!(closers(&exits), want, "each closer raw, in order");
    assert!(
        app.store
            .read()
            .unwrap()
            .closed_logs()
            .by_local(last)
            .is_some_and(|e| matches!(e.source(), ClosedSource::Summary(_))),
        "the app's last close is indexed from its summary"
    );
    let swapped = |r: ExitReason| match r {
        ExitReason::CtlClose => "ui-close",
        ExitReason::UiClose => "ctl-close",
        other => other.as_str(),
    };
    assert!(exits_agree(&app, &rows, &exits, swapped).is_err());
    let mut later = rows.clone();
    later[0].t_ms += 1;
    assert!(exits_agree(&app, &later, &exits, ExitReason::as_str).is_err());
    // An unminted closer reads `by=-` in the log, as an unknown one does:
    // only the index tells them apart.
    let unminted = exits
        .iter()
        .position(|e| e.actor == ExitActor::Sid("s-driver".into()))
        .expect("an unminted closer");
    let mut other_closer = exits.clone();
    other_closer[unminted].actor = ExitActor::Unknown;
    assert_eq!(by_r1(&ExitActor::Unknown), by_r1(&exits[unminted].actor));
    assert_ne!(
        closers(&other_closer),
        want,
        "NEGATIVE CONTROL: another closer"
    );
    let coded = exits
        .iter()
        .position(|e| e.exit_code.is_some())
        .expect("a close with a code");
    let mut other_code = exits.clone();
    other_code[coded].exit_code = other_code[coded].exit_code.map(|c| c + 1);
    assert!(exits_agree(&app, &rows, &other_code, ExitReason::as_str).is_err());
    let mut pair = rows.clone();
    pair.swap(a, b);
    assert!(exits_agree(&app, &pair, &exits, ExitReason::as_str).is_err());
}

/// The `ctl story` word a told verb has.
fn told_word(verb: StoryVerb) -> Option<&'static str> {
    StoryVerb::TOLD_WORDS
        .into_iter()
        .find(|w| StoryVerb::parse_told(w) == Some(verb))
}

fn epoch_start(log: &Log) -> u64 {
    let ep = u64::from(log.ep());
    log.window()
        .find(|r| r.f.get_u(Key::Epoch) == Some(ep))
        .map_or(0, |r| r.seq)
}

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

/// Whether presence counted the told points `told` (each `(seq, verb,
/// teller)`: seqs that rise, the newest presence's `story_seq`, since its
/// story keeps no points, 6G-6) and the log's epoch holds exactly one
/// `story` mark per told point by its teller, in order (T17: no
/// inequality).
fn story_agrees(told: &[(u64, StoryVerb, Actor)], story_seq: u64, log: &Log) -> Result<(), String> {
    let seqs: Vec<u64> = told.iter().map(|(s, _, _)| *s).collect();
    if !seqs.windows(2).all(|w| w[0] < w[1]) || seqs.last().is_some_and(|&l| l != story_seq) {
        return Err(format!("presence counts {story_seq}, told {seqs:?}"));
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
/// through `App::tell_story_by` by four tellers (the harness, an Owner
/// client, a minted session, the person) are presence's count, in order,
/// and the epoch's `story` marks, one each, `by=` the teller.
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
    let story_seq = app.presence.slot(session).expect("a slot").story_seq;
    {
        let g = ctx.timeline.lock().unwrap();
        assert_eq!(story_agrees(&told, story_seq, g.slog().log()), Ok(()));
        assert!(
            story_agrees(&told, story_seq + 1, g.slog().log()).is_err(),
            "NEGATIVE CONTROL: a count past the told points"
        );
    }
    {
        let mut g = ctx.timeline.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().observe_story("chose", Actor::Ctl, at);
        assert!(story_agrees(&told, story_seq, g.slog().log()).is_err());
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
    assert_eq!(story_agrees(&[], 0, log), Ok(()));
    assert_eq!(
        story_marks(log, 0),
        [("chose".to_string(), "harness".to_string())],
        "the carried mark, before the epoch"
    );
}
