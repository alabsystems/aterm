// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The session log's host side: what each slice-1 producer site writes (on an injected clock), the settle-wake law, the handoff carry and its version skew, the lock discipline, and the `log`/`brief` reply shapes.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use aterm_slog::{Actor, Conf, Fields, Key, Kind, Log, Mark, Node, Src, Stamp, Tier};

use super::*;
use crate::session_store::{ExitActor, ExitReason, SessionHandoff, SessionStore, test_handle};
use crate::session_timeline::{AgentStamp, SessionTimeline};

/// Wall time of the test epoch, so `t=` reads like a real one.
pub(super) const WALL0: u64 = 1_760_000_000_000;

/// A hand-driven clock: `ms` into the test, on both clocks.
#[derive(Clone)]
pub(super) struct Hand(Arc<AtomicU64>);

impl Hand {
    pub(super) fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }

    /// Move the hand on by `ms`.
    pub(super) fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

/// The stamp `ms` into the test.
pub(super) fn at(ms: u64) -> Stamp {
    Stamp::new(WALL0 + ms, ms)
}

/// Put `timeline`'s session log on a hand clock at 0.
pub(super) fn hand_clock(timeline: &Mutex<SessionTimeline>) -> Hand {
    hand_clock_from(timeline, 0)
}

/// [`hand_clock`] counting on from `base` ms on both clocks: a base of the host's monotonic now puts the hand on the event loop's own scale.
pub(super) fn hand_clock_from(timeline: &Mutex<SessionTimeline>, base: u64) -> Hand {
    let ms = Arc::new(AtomicU64::new(0));
    let read = Arc::clone(&ms);
    timeline
        .lock()
        .unwrap()
        .slog_mut()
        .set_clock(Clock::reading(move || {
            at(base + read.load(Ordering::SeqCst))
        }));
    Hand(ms)
}

/// Every live record of `log`, at the host tier (what `log` prints by default).
pub(super) fn lines(log: &Log) -> Vec<String> {
    log.window()
        .map(|r| aterm_slog::encode(r, Tier::Host).expect("a logged record fits"))
        .collect()
}

/// The records of `timeline`'s log.
pub(super) fn lines_of(timeline: &Mutex<SessionTimeline>) -> Vec<String> {
    lines(timeline.lock().unwrap().slog().log())
}

/// A screen stamp for a published verdict.
pub(super) fn stamp(seq: u64) -> AgentStamp {
    AgentStamp {
        generation: crate::control::ScreenGen { epoch: 0, seq },
        fp: 0x2a,
    }
}

/// A timeline whose session is open (spawned), its clock on the hand.
pub(super) fn opened() -> (Mutex<SessionTimeline>, Hand) {
    let tl = Mutex::new(SessionTimeline::for_test());
    let hand = hand_clock(&tl);
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, None, now);
        g.note_root_group(100);
    }
    (tl, hand)
}

/// `timeline` with `claude` in front (group 300) under the root shell (100), its agent run open (the sweep the name wakes opened it): the record count before the agent's first verdict.
pub(super) fn with_claude(tl: &Mutex<SessionTimeline>) {
    with_agent(tl, "claude");
}

/// [`with_claude`] for the agent named `name` (`codex`: a Codex run).
pub(super) fn with_agent(tl: &Mutex<SessionTimeline>, name: &str) {
    let mut g = tl.lock().unwrap();
    assert!(g.note_foreground_group(300));
    assert!(g.set_program(300, Some(name.into())));
    swept(&mut g);
}

fn swept_with(g: &mut SessionTimeline, read: &BlockRead) {
    let pgid = g.agent().program_pgid;
    let _ = g.sweep_slog(read, false, 100, pgid);
}

fn swept(g: &mut SessionTimeline) {
    swept_with(g, &BlockRead::default());
}

#[test]
fn register_opens_the_session_and_deregister_closes_it_with_the_ledgers_tale() {
    let mut h = test_handle(1);
    let parent = aterm_session::SessionId::new("s-0123456789abcdef0123");
    h.parent = Some(parent.clone());
    let timeline = Arc::clone(&h.ctx.timeline);
    let hand = hand_clock(&timeline);
    let mut store = SessionStore::default();
    store.register(h.clone());
    assert_eq!(
        lines_of(&timeline),
        vec![format!(
            "1 open - spawn parent=s-0123456789abcdef0123 epoch=1 state=alive src=aterm conf=exact \
             t={WALL0}"
        )]
    );
    store.register(h);
    assert_eq!(lines_of(&timeline).len(), 1, "a replace writes nothing");

    hand.set(500);
    store.note_exit_code(1, Some(3));
    store.deregister_local_as(
        1,
        ExitReason::CtlClose,
        ExitActor::Sid("s-ffffffffffffffffffff".into()),
    );
    assert_eq!(
        lines_of(&timeline)[1],
        format!(
            "2 close - spawn out=ctl-close dur_ms=500 code=3 by=s-ffffffffffffffffffff \
             src=aterm conf=exact t={}",
            WALL0 + 500
        )
    );
    let tl = timeline.lock().unwrap();
    let st = tl.slog().log().state();
    assert!(st.closed.is_some());
    assert_eq!(st.top_need().map(|n| n.word), Some("exited"));
}

#[test]
fn exit_actors_and_reasons_map_onto_the_logs_words() {
    assert_eq!(exit_actor(&ExitActor::Human), Actor::Human);
    assert_eq!(exit_actor(&ExitActor::Ctl), Actor::Ctl);
    assert_eq!(exit_actor(&ExitActor::Bridge), Actor::Bridge);
    assert_eq!(exit_actor(&ExitActor::Unknown), Actor::Nobody);
    assert_eq!(
        exit_actor(&ExitActor::Sid("s-0123456789abcdef0123".into())).word(),
        "s-0123456789abcdef0123"
    );
    for chosen in ["s-1", "harness", "human", "ctl", "bridge"] {
        assert_eq!(
            exit_actor(&ExitActor::Sid(chosen.into())).word(),
            "-",
            "{chosen}"
        );
    }
    for reason in ExitReason::ALL {
        assert_eq!(
            aterm_slog::Out::Session(exit_out(reason)).word(),
            reason.as_str(),
            "{reason:?}"
        );
    }
}

#[test]
fn a_restored_adopted_or_kept_session_opens_as_what_it_is() {
    let restored = test_handle(1);
    let _hand = hand_clock(&restored.ctx.timeline);
    {
        let mut tl = restored.ctx.timeline.lock().unwrap();
        let at = tl.slog().now();
        tl.slog_mut().note_restored(None, None, at);
    }
    let mut store = SessionStore::default();
    store.register(restored.clone());
    assert!(
        lines_of(&restored.ctx.timeline)[0].starts_with("1 open - restore epoch=1 "),
        "{:?}",
        lines_of(&restored.ctx.timeline)
    );

    let kept = test_handle(2);
    let _hand = hand_clock(&kept.ctx.timeline);
    crate::spawn::mark_handoff_gap(
        &kept.ctx.timeline,
        &kept.ctx.cast,
        &kept.ctx.temporal,
        None,
        0,
        LogAdopt::default(),
        true,
    );
    store.register(kept.clone());
    assert_eq!(
        lines_of(&kept.ctx.timeline),
        vec![
            format!("1 gap - gap why=crash by=aterm src=aterm conf=exact t={WALL0}"),
            format!(
                "2 open - adopt epoch={} state=alive src=aterm conf=exact t={WALL0}",
                WALL0 / 1000
            ),
        ]
    );
}

#[test]
fn a_seamless_update_carries_the_epoch_and_never_reissues_a_node_id() {
    let producer = test_handle(1);
    let producer_tl = Arc::clone(&producer.ctx.timeline);
    let hand = hand_clock(&producer_tl);
    let mut store = SessionStore::default();
    store.register(producer);
    {
        let mut g = producer_tl.lock().unwrap();
        g.note_root_group(100);
        for (i, pgid) in [201, 202, 203].into_iter().enumerate() {
            hand.set(10 * (i as u64 + 1));
            g.note_foreground_group(pgid);
            g.set_program(pgid, Some("less".into()));
            swept(&mut g);
        }
    }
    let manifest = SessionHandoff::from_store(&store);
    let read = SessionHandoff::from_toml(&manifest.to_toml().unwrap()).unwrap();
    let reserve = LOG_ID_RESERVE;
    assert_eq!(
        read.sessions[0].log_ids.as_deref(),
        Some(format!("1 {} {reserve} {reserve}", 3 + reserve).as_str()),
    );
    let carry = read.sessions[0]
        .log_ids
        .as_deref()
        .and_then(LogCarry::parse)
        .expect("the row reads");

    let successor = test_handle(1);
    let successor_tl = Arc::clone(&successor.ctx.timeline);
    let hand2 = hand_clock(&successor_tl);
    crate::spawn::mark_handoff_gap(
        &successor.ctx.timeline,
        &successor.ctx.cast,
        &successor.ctx.temporal,
        Some(7),
        read.sessions[0].timeline_id,
        LogAdopt {
            ids: Some(carry),
            seq: read.sessions[0].log_seq,
            lines: None,
            keeper_ep: None,
            mirror: None,
        },
        false,
    );
    let mut next = SessionStore::default();
    next.register(successor);
    {
        let mut g = successor_tl.lock().unwrap();
        g.note_root_group(100);
        hand2.set(5);
        g.note_foreground_group(301);
        g.set_program(301, Some("vim".into()));
        swept(&mut g);
    }
    let after = lines_of(&successor_tl);
    let s0 = 6 + LOG_SEQ_RESERVE;
    assert_eq!(read.sessions[0].log_seq, s0);
    assert_eq!(
        after[..2],
        [
            format!(
                "{} gap - gap why=handoff epoch=2 build=7 by=aterm src=aterm conf=exact \
                 t={WALL0}",
                s0 + 1
            ),
            format!(
                "{} open - adopt epoch=2 state=alive src=aterm conf=exact t={WALL0}",
                s0 + 2
            ),
        ]
    );
    let first_run = 4 + reserve;
    assert!(
        after[2].starts_with(&format!(
            "{} open r{first_run} prog par=- prog=vim ",
            s0 + 3
        )),
        "{after:?}"
    );
    assert_eq!(successor_tl.lock().unwrap().slog().log().ep(), 2);

    {
        let mut g = producer_tl.lock().unwrap();
        for pgid in 0..reserve as i32 {
            g.note_foreground_group(1000 + pgid);
            g.set_program(1000 + pgid, Some("less".into()));
        }
        swept(&mut g);
        assert!(!g.past_handoff_ceiling(), "at the reserve: fine");
        let past = 1000 + reserve as i32;
        g.note_foreground_group(past);
        g.set_program(past, Some("less".into()));
        swept(&mut g);
        assert!(g.past_handoff_ceiling(), "past it: the Commit stands down");
    }

    let bare = test_handle(1);
    let _hand3 = hand_clock(&bare.ctx.timeline);
    crate::spawn::mark_handoff_gap(
        &bare.ctx.timeline,
        &bare.ctx.cast,
        &bare.ctx.temporal,
        None,
        0,
        LogAdopt::default(),
        false,
    );
    {
        let mut g = bare.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, None, now);
        g.note_root_group(100);
        g.note_foreground_group(301);
        g.set_program(301, Some("vim".into()));
        swept(&mut g);
    }
    assert!(
        lines_of(&bare.ctx.timeline)
            .iter()
            .any(|l| l.starts_with("3 open r1 prog ")),
        "{:?}",
        lines_of(&bare.ctx.timeline)
    );
}

#[test]
fn the_log_carry_is_an_additive_key_every_build_reads_past() {
    let base = "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-old\"\nstate = \"alive\"\ntitle = \"zsh\"\n";
    let older = SessionHandoff::from_toml(base).expect("an older producer's manifest reads");
    assert_eq!(older.sessions[0].log_ids, None);
    let newer = SessionHandoff::from_toml(&format!(
        "{base}log_ids = \"4 10 20 30\"\nlog_seq = 99\nfuture_field = {{ x = 1 }}\n"
    ))
    .expect("a newer producer's manifest reads");
    assert_eq!(
        newer.sessions[0]
            .log_ids
            .as_deref()
            .and_then(LogCarry::parse),
        Some(LogCarry {
            ep: 4,
            ids: aterm_slog::IdCounters {
                run: 10,
                turn: 20,
                step: 30
            }
        })
    );
    for bad in [
        "5",
        "\"x\"",
        "[1, 2]",
        "\"0 1 2 3\"",
        "\"1 2 3\"",
        "\"1 2 3 4 5\"",
    ] {
        let m = SessionHandoff::from_toml(&format!("{base}log_ids = {bad}\n"))
            .unwrap_or_else(|| panic!("`log_ids = {bad}` cost the manifest"));
        assert_eq!(
            m.sessions[0].log_ids.as_deref().and_then(LogCarry::parse),
            None,
            "`{bad}` carries nothing"
        );
    }
    assert_eq!(older.sessions[0].log_seq, 0);
    assert_eq!(newer.sessions[0].log_seq, 99);
    for bad in ["\"x\"", "-1", "[1]", "{ y = 2 }", "1.5"] {
        let m = SessionHandoff::from_toml(&format!("{base}log_seq = {bad}\n"))
            .unwrap_or_else(|| panic!("`log_seq = {bad}` cost the manifest"));
        assert_eq!(m.sessions[0].log_seq, 0, "`{bad}` carries none");
    }
    #[derive(serde::Deserialize)]
    struct OlderRecord {
        sid: String,
        log_ids: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct OlderManifest {
        sessions: Vec<OlderRecord>,
    }
    let written = {
        let mut h = newer.clone();
        h.sessions[0].log_seq = 77;
        h.to_toml().expect("writes")
    };
    assert!(written.contains("log_seq = 77"), "{written}");
    let old: OlderManifest = aterm_toml::from_str(&written).expect("an older build reads it");
    assert_eq!(old.sessions[0].sid, "s-old");
    assert_eq!(old.sessions[0].log_ids.as_deref(), Some("4 10 20 30"));
    let c = LogCarry {
        ep: 2,
        ids: aterm_slog::IdCounters {
            run: 1,
            turn: 2,
            step: 3,
        },
    };
    assert_eq!(LogCarry::parse(&c.row()), Some(c));
}

#[test]
fn a_process_past_the_seq_reserve_stands_its_commit_down() {
    let (tl, _hand) = opened();
    let mut g = tl.lock().unwrap();
    let carried = g.handoff_log_seq();
    assert_eq!(carried, g.slog().log().last_seq() + LOG_SEQ_RESERVE);
    let now = g.slog().now();
    while g.slog().log().last_seq() < carried {
        g.slog_mut().human_input(true, now);
    }
    assert_eq!(g.slog().log().last_seq(), carried);
    assert!(
        !g.past_handoff_ceiling(),
        "NEGATIVE CONTROL: at the reserve, the Commit stands"
    );
    g.slog_mut().human_input(true, now);
    assert!(g.past_handoff_ceiling(), "past it, the Commit stands down");
}

#[test]
fn a_seq_count_past_any_log_never_stands_the_commit_down() {
    let adopted = |seq: u64, lines: Option<Vec<String>>| -> Mutex<SessionTimeline> {
        let tl = Mutex::new(SessionTimeline::for_test());
        let _hand = hand_clock(&tl);
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        let ids = LogCarry {
            ep: 3,
            ids: aterm_slog::IdCounters::default(),
        };
        g.slog_mut().adopt(
            LogAdopt {
                ids: Some(ids),
                seq,
                lines,
                keeper_ep: None,
                mirror: None,
            },
            false,
            None,
            at,
        );
        for _ in 0..4 {
            g.slog_mut().human_input(true, at);
        }
        drop(g);
        tl
    };
    let seqs = |tl: &Mutex<SessionTimeline>| -> Vec<u64> {
        tl.lock()
            .unwrap()
            .slog()
            .log()
            .window()
            .map(|r| r.seq)
            .collect()
    };
    let stands = |tl: &Mutex<SessionTimeline>| -> bool {
        let mut g = tl.lock().unwrap();
        let carried = g.handoff_log_seq();
        assert!(carried <= i64::MAX as u64, "the count fits TOML");
        !g.past_handoff_ceiling()
    };
    let huge = adopted(i64::MAX as u64, None);
    let got = seqs(&huge);
    assert_eq!(got.first(), Some(&1), "a new seq space: {got:?}");
    assert!(stands(&huge), "the next Commit stands");
    let (p, store, _hand) = producer();
    let rec = SessionHandoff::from_store(&store).sessions[0].clone();
    let mut lines = p
        .ctx
        .timeline
        .lock()
        .unwrap()
        .slog_carry_source()
        .encode(aterm_slog::CarryBudget::SESSION)
        .expect("carried")
        .lines;
    let last = p.ctx.timeline.lock().unwrap().slog().log().last_seq();
    lines[0] = lines[0].replace(
        &format!(" last={last} "),
        &format!(" last={} ", u64::MAX - 2),
    );
    let edited = adopted(rec.log_seq, Some(lines.clone()));
    let got = seqs(&edited);
    assert!(
        got.windows(2).all(|w| w[0] < w[1]) && got.iter().all(|s| *s <= rec.log_seq + 5),
        "never saturated, never repeated: {got:?}"
    );
    assert!(
        lines_of(&edited)[0].contains(" gap - gap why=handoff "),
        "uncarried: {:?}",
        lines_of(&edited)
    );
    assert!(stands(&edited), "the next Commit stands");

    let fine = adopted(1_000, None);
    assert_eq!(seqs(&fine).first(), Some(&1_001));
    assert!(stands(&fine));
}

#[test]
fn the_successors_first_seq_is_above_every_predecessor_seq() {
    let producer = test_handle(1);
    let _hand = hand_clock(&producer.ctx.timeline);
    let mut store = SessionStore::default();
    store.register(producer.clone());
    let manifest = SessionHandoff::from_store(&store);
    let read = SessionHandoff::from_toml(&manifest.to_toml().unwrap()).unwrap();
    let last_written = {
        let mut g = producer.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        for _ in 0..50 {
            g.slog_mut().human_input(true, now);
        }
        assert!(!g.past_handoff_ceiling());
        g.slog().log().last_seq()
    };
    let rec = &read.sessions[0];
    let successor = test_handle(1);
    let _hand2 = hand_clock(&successor.ctx.timeline);
    crate::spawn::mark_handoff_gap(
        &successor.ctx.timeline,
        &successor.ctx.cast,
        &successor.ctx.temporal,
        None,
        rec.timeline_id,
        LogAdopt {
            ids: rec.log_ids.as_deref().and_then(LogCarry::parse),
            seq: rec.log_seq,
            lines: None,
            keeper_ep: None,
            mirror: None,
        },
        false,
    );
    let first = successor
        .ctx
        .timeline
        .lock()
        .unwrap()
        .slog()
        .log()
        .window()
        .next()
        .map(|r| r.seq)
        .expect("the successor's gap");
    assert!(first > last_written, "{first} is above {last_written}");
    assert_eq!(first, rec.log_seq + 1);

    let older = test_handle(1);
    let _hand3 = hand_clock(&older.ctx.timeline);
    crate::spawn::mark_handoff_gap(
        &older.ctx.timeline,
        &older.ctx.cast,
        &older.ctx.temporal,
        None,
        0,
        LogAdopt {
            ids: rec.log_ids.as_deref().and_then(LogCarry::parse),
            seq: 0,
            lines: None,
            keeper_ep: None,
            mirror: None,
        },
        false,
    );
    assert_eq!(
        older
            .ctx
            .timeline
            .lock()
            .unwrap()
            .slog()
            .log()
            .window()
            .next()
            .map(|r| r.seq),
        Some(1),
        "NEGATIVE CONTROL: an older manifest's successor starts at 1"
    );
}

fn fact(blk: u64, start: u64, end: BlockEnd, detail: Option<&str>) -> BlockFact {
    BlockFact {
        id: blk,
        started_ms: Some(WALL0 + start),
        end,
        detail: detail.map(str::to_string),
    }
}

pub(super) fn running(blk: u64, start: u64, detail: &str) -> BlockRead {
    BlockRead {
        facts: vec![fact(blk, start, BlockEnd::Running, Some(detail))],
        ..BlockRead::default()
    }
}

fn done(blk: u64, code: Option<i32>, start: u64, end: u64) -> BlockEnd {
    BlockEnd::Done(crate::session_status::CompletedBlock {
        id: blk,
        exit_code: code,
        started_ms: Some(WALL0 + start),
        ended_ms: Some(WALL0 + end),
    })
}

pub(super) fn finished(
    blk: u64,
    code: Option<i32>,
    start: u64,
    end: u64,
    detail: &str,
) -> BlockRead {
    BlockRead {
        facts: vec![fact(blk, start, done(blk, code, start, end), Some(detail))],
        ..BlockRead::default()
    }
}

pub(super) fn blocks(tl: &Mutex<SessionTimeline>, hand: &Hand, ms: u64, read: &BlockRead) {
    hand.set(ms);
    let mut g = tl.lock().unwrap();
    let now = g.slog().now();
    g.slog_mut().shell_blocks(read, None, now);
}

#[test]
fn shell_blocks_open_and_close_cmd_runs_from_their_stamps() {
    let (tl, hand) = opened();
    blocks(&tl, &hand, 150, &running(0, 100, "true"));
    assert_eq!(tl.lock().unwrap().slog().block_cursor(), Some(0));
    blocks(&tl, &hand, 400, &finished(0, Some(0), 100, 130, "true"));
    assert_eq!(tl.lock().unwrap().slog().block_cursor(), Some(1));
    blocks(&tl, &hand, 900, &finished(1, Some(1), 500, 520, "false"));
    blocks(&tl, &hand, 1150, &finished(1, Some(1), 500, 520, "false"));
    let got = lines_of(&tl);
    assert_eq!(
        got[1..],
        [
            format!(
                "2 open r1 cmd par=- blk=0 detail=true src=shell conf=strong t={}",
                WALL0 + 100
            ),
            format!(
                "3 close r1 cmd out=ok dur_ms=30 code=0 src=shell conf=exact t={}",
                WALL0 + 130
            ),
            format!(
                "4 open r2 cmd par=- blk=1 detail=false src=shell conf=strong t={}",
                WALL0 + 500
            ),
            format!(
                "5 close r2 cmd out=fail dur_ms=20 dur=wall code=1 src=shell conf=exact t={}",
                WALL0 + 520
            ),
        ],
        "{got:?}"
    );
    assert_eq!(
        tl.lock()
            .unwrap()
            .slog()
            .log()
            .state()
            .top_need()
            .map(|n| n.word),
        Some("failure")
    );
}

#[test]
fn a_sweep_that_comes_late_still_reads_every_block_in_order() {
    let (tl, hand) = opened();
    blocks(&tl, &hand, 100, &running(2, 50, "sleep"));
    let read = BlockRead {
        facts: vec![
            fact(2, 50, done(2, Some(0), 50, 2_050), Some("sleep")),
            fact(3, 2_060, done(3, Some(0), 2_060, 2_061), Some("true")),
            fact(4, 2_070, done(4, Some(1), 2_070, 2_071), Some("false")),
            fact(5, 2_080, BlockEnd::Running, Some("less")),
        ],
        ..BlockRead::default()
    };
    blocks(&tl, &hand, 2_200, &read);
    let got = lines_of(&tl);
    let runs: Vec<&str> = got[1..]
        .iter()
        .map(|l| l.split(" src=").next().unwrap_or(""))
        .collect();
    assert_eq!(
        runs,
        [
            "2 open r1 cmd par=- blk=2 detail=sleep",
            "3 close r1 cmd out=ok dur_ms=2000 code=0",
            "4 open r2 cmd par=- blk=3 detail=true",
            "5 close r2 cmd out=ok dur_ms=1 dur=wall code=0",
            "6 open r3 cmd par=- blk=4 detail=false",
            "7 close r3 cmd out=fail dur_ms=1 dur=wall code=1",
            "8 open r4 cmd par=- blk=5 detail=less",
        ],
        "{got:?}"
    );
}

#[test]
fn a_block_closed_without_a_d_ends_unknown_and_an_older_one_is_not_ours() {
    let (tl, hand) = opened();
    blocks(&tl, &hand, 50, &running(1, 40, "sleep"));
    blocks(
        &tl,
        &hand,
        300,
        &BlockRead {
            facts: vec![fact(
                1,
                40,
                BlockEnd::Unended {
                    ended_ms: Some(WALL0 + 280),
                },
                Some("sleep"),
            )],
            ..BlockRead::default()
        },
    );
    let got = lines_of(&tl);
    assert_eq!(
        got[2],
        format!(
            "3 close r1 cmd out=unknown dur_ms=240 src=shell conf=strong t={}",
            WALL0 + 280
        )
    );
    blocks(&tl, &hand, 400, &running(2, 390, "vim"));
    blocks(&tl, &hand, 500, &BlockRead::default());
    assert!(lines_of(&tl)[4].starts_with("5 close r2 cmd out=unknown "));

    let adopted = Mutex::new(SessionTimeline::for_test());
    let hand = hand_clock(&adopted);
    hand.set(10_000);
    {
        let mut g = adopted.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().adopt(LogAdopt::default(), false, None, now);
        g.slog_mut().open_session(None, None, now);
    }
    blocks(
        &adopted,
        &hand,
        10_100,
        &finished(7, Some(0), 1_000, 2_000, "make"),
    );
    assert_eq!(lines_of(&adopted).len(), 2, "{:?}", lines_of(&adopted));
    assert_eq!(adopted.lock().unwrap().slog().block_cursor(), Some(8));
    blocks(&adopted, &hand, 10_200, &running(8, 3_000, "claude"));
    assert!(lines_of(&adopted)[2].starts_with("3 open r1 cmd par=- blk=8 detail=claude "));
}

#[test]
fn block_read_reports_every_block_past_the_cursor() {
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    let prompt = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    term.process(prompt);
    term.process(b"true\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
    term.process(prompt);
    term.process(b"false\r\n\x1b]133;C\x1b\\\x1b]133;D;1\x1b\\");
    term.process(prompt);
    term.process(b"sleep 9\r\n\x1b]133;C\x1b\\");
    let read = crate::session_status::block_read(&term, None, None, Some("sleep".into()));
    let ends: Vec<(u64, Option<i32>, bool)> = read
        .facts
        .iter()
        .map(|f| match f.end {
            BlockEnd::Done(d) => (
                f.id,
                d.exit_code,
                d.started_ms.is_some() && d.ended_ms.is_some(),
            ),
            BlockEnd::Running => (f.id, None, f.started_ms.is_some()),
            BlockEnd::Unended { .. } => (f.id, Some(-1), false),
        })
        .collect();
    assert_eq!(
        ends,
        [(0, Some(0), true), (1, Some(1), true), (2, None, true)]
    );
    assert_eq!(read.facts[2].detail.as_deref(), Some("sleep"));
    let from_two = crate::session_status::block_read(&term, Some(2), None, Some("sleep".into()));
    assert_eq!(
        from_two.facts.iter().map(|f| f.id).collect::<Vec<_>>(),
        [2],
        "older blocks are not read again"
    );
    let done_block = crate::session_status::completed_block(&term);
    assert_eq!(done_block, None, "a running command reports nothing there");

    let at = |wall_ms: u64| aterm_core::terminal::ClockReading {
        monotonic: aterm_core::terminal::ClockReading::now().monotonic,
        wall_ms: Some(wall_ms),
    };
    let began = read.facts[2].started_ms.expect("C stamped");
    term.process_at(prompt, at(began + 1_000));
    let cut = crate::session_status::block_read(&term, Some(2), None, None);
    assert_eq!(
        cut.facts.iter().map(|f| (f.id, f.end)).collect::<Vec<_>>(),
        [(
            2,
            BlockEnd::Unended {
                ended_ms: Some(began + 1_000)
            }
        )]
    );
    term.process_at(
        b"make\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\",
        at(began + 2_000),
    );
    let at_open = crate::session_status::block_read(&term, Some(3), Some(began + 2_000), None);
    assert_eq!(at_open.facts.iter().map(|f| f.id).collect::<Vec<_>>(), [3]);
    let before = crate::session_status::block_read(&term, Some(3), Some(began + 2_001), None);
    assert_eq!((before.facts.len(), before.through), (0, Some(3)));
    let unended = crate::session_status::block_read(&term, Some(2), Some(began + 1_001), None);
    assert_eq!(unended.facts.iter().map(|f| f.id).collect::<Vec<_>>(), [3]);
}

#[test]
fn a_finished_block_is_logged_by_its_program_alone() {
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    let prompt = b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\";
    term.process(prompt);
    term.process(b"echo QZP6SECRET1 42\r\n\x1b]133;C\x1b\\QZP6SECRET1 42\r\n\x1b]133;D;0\x1b\\");
    term.process(prompt);
    term.process(b"\x1b]633;E;export QZP6SECRET2=1\x1b\\\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
    term.process(prompt);
    let read = crate::session_status::block_read(&term, None, None, None);
    assert_eq!(
        read.facts
            .iter()
            .map(|f| (f.id, f.detail.as_deref()))
            .collect::<Vec<_>>(),
        [(0, Some("echo")), (1, Some("export"))],
        "each finished block names its program"
    );
    let (tl, hand) = opened();
    blocks(&tl, &hand, 100, &read);
    let lines = lines_of(&tl);
    assert!(
        lines
            .iter()
            .any(|l| l.contains(" open r") && l.contains(" cmd ") && l.contains(" detail=echo ")),
        "a positive control: the command is logged: {lines:?}"
    );
    assert!(
        lines.iter().all(|l| !l.contains("QZP6SECRET")),
        "no later word of a command is logged: {lines:?}"
    );
}

#[test]
fn the_program_in_front_opens_and_closes_its_run() {
    let (tl, hand) = opened();
    blocks(&tl, &hand, 10, &running(1, 5, "less"));
    {
        let mut g = tl.lock().unwrap();
        hand.set(20);
        assert!(g.note_foreground_group(200));
        assert!(g.set_program(200, Some("less".into())));
        hand.set(1_020);
        assert!(g.note_foreground_group(100));
        assert!(g.set_program(100, Some("zsh".into())));
    }
    let got = lines_of(&tl);
    assert_eq!(
        got[2..],
        [
            format!(
                "3 open r2 prog par=r1 prog=less src=status conf=strong t={}",
                WALL0 + 20
            ),
            format!(
                "4 close r2 prog out=unknown dur_ms=1000 src=status conf=strong t={}",
                WALL0 + 1_020
            ),
        ],
        "the root shell's name opened nothing: {got:?}"
    );
    {
        let mut g = tl.lock().unwrap();
        assert!(g.note_foreground_group(400));
        assert!(g.set_program(400, Some("bash".into())));
        swept_with(&mut g, &running(1, 5, "less"));
    }
    assert!(
        lines_of(&tl)[4].starts_with("5 open r3 prog par=r1 prog=bash "),
        "a nested shell is a run: {:?}",
        lines_of(&tl)
    );
    {
        let mut g = tl.lock().unwrap();
        assert!(g.note_foreground_group(300));
        assert!(g.set_program(300, Some("claude".into())));
        swept_with(&mut g, &running(1, 5, "less"));
    }
    let got = lines_of(&tl);
    assert!(
        got.iter()
            .any(|l| l.contains(" open r4 agent par=r1 prog=claude agent=claude ")),
        "{got:?}"
    );
    {
        let mut g = tl.lock().unwrap();
        assert!(g.set_program(300, Some("vim".into())));
        swept_with(&mut g, &running(1, 5, "less"));
    }
    let got = lines_of(&tl);
    assert!(
        got.iter()
            .any(|l| l.contains(" close r4 agent out=superseded "))
            && got
                .iter()
                .any(|l| l.contains(" open r5 prog par=r1 prog=vim ")),
        "{got:?}"
    );
}

#[test]
fn the_agent_verdict_marks_and_drives_the_turn_tracker() {
    let (tl, hand) = opened();
    with_claude(&tl);
    let base = lines_of(&tl).len();
    {
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
        g.publish_agent(
            "prompt",
            Some("bash:not-read-only".into()),
            None,
            Some(aterm_phase::Program::Claude),
            stamp(6),
        );
        hand.set(3_000);
        g.publish_agent(
            "busy",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(7),
        );
        hand.set(4_000);
        g.publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(8),
        );
        assert_eq!(g.slog_wake(), Some(5_000), "the settle is armed");
        hand.set(4_999);
        assert_eq!(g.slog_tick(), Some(5_000), "an early tick changes nothing");
        hand.set(5_000);
        assert_eq!(g.slog_tick(), None, "the settle is spent");
    }
    let got = lines_of(&tl);
    let got = &got[base..];
    assert_eq!(
        got[0],
        format!(
            "{} mark r1 agent word=busy rev=1 gen=0.5 fp=000000000000002a src=screen \
             conf=strong t={}",
            base + 1,
            WALL0 + 1_000
        )
    );
    assert!(
        got[1].starts_with(&format!(
            "{} open t1 turn par=r1 src=screen conf=strong",
            base + 2
        )),
        "{got:?}"
    );
    assert!(
        got.iter()
            .any(|l| l.contains(" open p1 approval par=t1 box=bash:not-read-only ")),
        "{got:?}"
    );
    assert!(
        got.iter()
            .any(|l| l.contains(" close p1 approval out=passed ")),
        "{got:?}"
    );
    let close = got.last().unwrap();
    assert!(
        close.contains(" close t1 turn out=done dur_ms=3000 ")
            && close.ends_with(&format!("t={}", WALL0 + 4_000)),
        "dated when the idle first held: {close}"
    );
}

#[test]
fn a_screen_named_agent_opens_its_own_run_under_the_program() {
    let (tl, hand) = opened();
    {
        let mut g = tl.lock().unwrap();
        hand.set(10);
        g.note_foreground_group(300);
        g.set_program(300, Some("node".into()));
        hand.set(20);
        g.publish_agent(
            "busy",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(1),
        );
        hand.set(30);
        g.publish_agent("-", None, None, None, stamp(2));
    }
    let got = lines_of(&tl);
    assert!(
        got.iter()
            .any(|l| l.starts_with("3 open r2 agent par=r1 agent=claude src=screen ")),
        "{got:?}"
    );
    assert!(
        got.iter()
            .any(|l| l.contains(" close r2 agent out=unknown ")),
        "{got:?}"
    );
    assert!(
        tl.lock()
            .unwrap()
            .slog()
            .log()
            .is_open(Node::new(aterm_slog::Lvl::Run, 1)),
        "the program's own run stays open"
    );
}

#[test]
fn a_persons_input_marks_its_burst_and_attributes_the_turn() {
    let h = test_handle(1);
    let hand = hand_clock(&h.ctx.timeline);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, None, now);
        g.note_root_group(100);
    }
    with_claude(&h.ctx.timeline);
    hand.set(100);
    crate::app_input::note_person(&h.ctx, crate::human_input::Gesture::Other);
    hand.set(200);
    crate::app_input::note_person(&h.ctx, crate::human_input::Gesture::Other);
    hand.set(300);
    h.ctx.timeline.lock().unwrap().publish_agent(
        "busy",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        stamp(1),
    );
    let got = lines_of(&h.ctx.timeline);
    let humans: Vec<&String> = got
        .iter()
        .filter(|l| l.contains(" mark "))
        .filter(|l| l.contains(" human "))
        .collect();
    assert_eq!(humans.len(), 1, "one mark per burst: {got:?}");
    // Seq 4: a test build's `holder-named` dev row (the agent named) is a
    // `row` mark, one record before it.
    assert_eq!(
        *humans[0],
        format!(
            "4 mark - human by=human src=input conf=strong t={}",
            WALL0 + 100
        )
    );
    assert!(
        got.iter()
            .any(|l| l.contains(" open t1 turn par=r1 by=human ")),
        "{got:?}"
    );
}

#[test]
fn a_ctl_turn_labels_the_turn_it_opens_with_its_ledger_id() {
    let h = test_handle(0);
    let hand = hand_clock(&h.ctx.timeline);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, None, now);
        g.note_root_group(100);
    }
    with_claude(&h.ctx.timeline);
    let store = crate::session_store::new_store();
    hand.set(10);
    let reply = crate::control::cmd_turn(
        &h.term,
        &store,
        0,
        "idle=1 timeout=60000 submit=none hello",
        &h.ctx,
        &crate::control::TurnIo {
            paste: &|_: &str| true,
            ..crate::control::TurnIo::paste_only()
        },
    );
    assert!(reply.starts_with("OK "), "{reply}");
    let ledger = h.ctx.turns.lock().unwrap().since(None).next().map(|r| r.id);
    let ledger = ledger.expect("the exchange is on the ledger");
    hand.set(20);
    h.ctx.timeline.lock().unwrap().publish_agent(
        "busy",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        stamp(1),
    );
    let got = lines_of(&h.ctx.timeline);
    assert!(
        got.iter()
            .any(|l| l.contains(&format!(" open t1 turn par=r1 ledger={ledger} by=ctl "))),
        "{got:?}"
    );
}

#[test]
fn a_ctl_turn_never_leaves_a_settle_for_the_loop() {
    let (tl, hand) = opened();
    with_claude(&tl);
    let mut g = tl.lock().unwrap();
    hand.set(100);
    g.publish_agent(
        "busy",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        stamp(1),
    );
    let now = g.slog().now();
    assert_eq!(
        g.slog_mut().ctl_turn(Actor::Ctl, 1, None, now),
        None,
        "busy"
    );
    hand.set(200);
    g.publish_agent(
        "idle",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        stamp(2),
    );
    assert!(g.slog_wake().is_some(), "resting: a settle is armed");
    hand.set(300);
    let now = g.slog().now();
    assert_eq!(
        g.slog_mut().ctl_turn(Actor::Ctl, 2, None, now),
        None,
        "at rest"
    );
    assert_eq!(g.slog_wake(), None, "…and the resting turn closed");
}

#[test]
fn an_idle_session_arms_nothing_and_a_pending_settle_is_ticked() {
    let mut app = crate::App::headless_for_test();
    let timeline = app.pool.get(0).expect("session").ctx.timeline.clone();
    assert!(
        app.next_session_log_wake().is_none(),
        "nothing owed at rest"
    );
    app.tick_session_logs();
    assert!(app.session_status.slog_wakes.is_empty());

    let now = crate::turn_ledger::now_ms();
    let hand = hand_clock_from(&timeline, now);
    {
        let mut g = timeline.lock().unwrap();
        g.note_root_group(100);
        g.note_foreground_group(300);
        g.set_program(300, Some("claude".into()));
        g.publish_agent(
            "busy",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(1),
        );
        g.publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(2),
        );
        let wake = g.slog_wake().expect("armed");
        assert!(wake >= now + aterm_slog::SETTLE_MS);
        app.session_status.slog_wakes.note(0, Some(wake));
    }
    let deadline = app.next_session_log_wake().expect("one wake owed");
    assert!(deadline > std::time::Instant::now(), "a future instant");
    app.tick_session_logs();
    assert_eq!(
        app.session_status.slog_wakes.next(),
        timeline.lock().unwrap().slog_wake()
    );
    let turn_open = |tl: &Mutex<SessionTimeline>| {
        tl.lock()
            .unwrap()
            .slog()
            .log()
            .open_nodes()
            .iter()
            .any(|e| e.kind == Kind::Turn)
    };
    assert!(turn_open(&timeline));

    hand.set(aterm_slog::SETTLE_MS);
    app.session_status.slog_wakes.note(0, Some(0));
    app.tick_session_logs();
    assert!(!turn_open(&timeline), "the tick closed the turn");
    assert!(app.session_status.slog_wakes.is_empty(), "nothing re-armed");
    assert!(app.next_session_log_wake().is_none());

    {
        let mut g = timeline.lock().unwrap();
        g.publish_agent(
            "busy",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(3),
        );
        g.publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(4),
        );
    }
    hand.set(3 * aterm_slog::SETTLE_MS);
    assert!(
        turn_open(&timeline),
        "no tick, no close, past the settle too"
    );
}

#[test]
fn the_wake_board_is_a_min_over_sessions_and_forgets_on_none() {
    let mut w = Wakes::default();
    assert!(w.is_empty() && w.next().is_none());
    w.note(1, Some(500));
    w.note(2, Some(300));
    assert_eq!(w.next(), Some(300));
    assert_eq!(w.due(299), Vec::<u64>::new());
    assert_eq!(w.due(500), vec![1, 2]);
    w.note(2, None);
    assert_eq!(w.next(), Some(500));
    w.note(1, None);
    assert!(w.is_empty());
}

#[test]
fn every_producer_runs_under_the_timeline_lock_alone() {
    use std::sync::mpsc;
    let h = test_handle(1);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    let (held_tx, held_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let (store2, ctx2) = (Arc::clone(&store), Arc::clone(&h.ctx));
    let holder = std::thread::spawn(move || {
        let _s = store2.write().unwrap();
        let _t = ctx2.turns.lock().unwrap();
        let _m = ctx2.meta.lock().unwrap();
        held_tx.send(()).unwrap();
        let _ = done_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    });
    held_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the holder took its locks");
    assert!(store.try_write().is_err(), "the store is held");
    assert!(h.ctx.turns.try_lock().is_err(), "the ledger is held");
    assert!(h.ctx.meta.try_lock().is_err(), "the meta is held");
    let (ran_tx, ran_rx) = mpsc::channel();
    let ctx = Arc::clone(&h.ctx);
    let worker = std::thread::spawn(move || {
        {
            let mut g = ctx.timeline.lock().unwrap();
            g.note_root_group(100);
            g.note_foreground_group(300);
            g.set_program(300, Some("claude".into()));
            g.publish_agent(
                "busy",
                None,
                None,
                Some(aterm_phase::Program::Claude),
                stamp(1),
            );
            g.publish_agent(
                "idle",
                None,
                None,
                Some(aterm_phase::Program::Claude),
                stamp(2),
            );
            let _ = g.slog_tick();
            let now = g.slog().now();
            g.slog_mut().shell_blocks(&running(1, 0, "x"), None, now);
            let _ = g.slog_mut().ctl_turn(Actor::Ctl, 1, None, now);
            g.slog_mut().failure_seen(now);
            let _ = g.handoff_log_ids();
            g.slog_mut().ctl_input(
                Actor::Ctl,
                super::InputVerb::Send,
                None,
                super::InputAct::default(),
                now,
            );
            let _ = g.slog_take_wake();
            let engine = super::observed::EngineRead {
                integration: aterm_core::terminal::ShellIntegrationPosture::On,
                notify_total: 1,
                bell_total: 1,
                progress: None,
            };
            g.slog_mut().observe_engine(&engine, now);
            let facts = super::observed::StatusFacts {
                phase: "idle",
                out: Some(("ok", None)),
                conf: Conf::Strong,
            };
            g.slog_mut().observe_status(facts, now);
            g.slog_mut().agent_interrupted();
            g.slog_mut().observe_screen_facts(Some(40), Some(true), now);
            let card = aterm_phase::LaunchCard {
                version: "2.1.300".into(),
                model: "Opus 5.5".into(),
                effort: None,
            };
            g.slog_mut().observe_card(&card, now);
            g.slog_mut().observe_render(
                &crate::resize_ledger::Transition::Healed {
                    run: 1,
                    after_ms: 2,
                },
                now,
            );
            g.record_cwd_change(Some("/a"));
            g.slog_mut().observe_title("t", now);
            let _ = g.slog_take_wake();
            let _ = g.note_harness_act("approve-all@v1", "codex-alab@1", Actor::Ctl);
            let working = crate::task_record::TaskMark::new(crate::task_record::TaskState::Working);
            let _ = g.mark_task(&working, false, std::time::Instant::now(), 1);
            let _ = g.note_task_attention(true);
            let _ = g.slog_take_wake();
        }
        let report = crate::manual_reset::ResetReport {
            reverted: vec!["mouse"],
            bytes: 1,
        };
        crate::manual_reset::record(
            1,
            &ctx.timeline,
            crate::manual_reset::ResetSource::Menu,
            &report,
        );
        let key = format!("{}:4:1", ctx.nonce.to_hex());
        let _ = crate::pty_idem::guarded_waking(
            &ctx,
            crate::control::Scope::Owner,
            "send",
            Some(&key),
            &|| {},
            || "ERR write failed\n".to_string(),
        );
        let _ = super::validate_cause(&ctx, super::CauseRef::Rec(1));
        let _ = super::validate_cause(&ctx, super::CauseRef::Mail(7));
        let (reply, _) = super::cmd_mark(&ctx, Actor::Ctl, true, "- note text=x re=1");
        assert!(reply.starts_with("OK "), "{reply}");
        crate::app_input::note_person(&ctx, crate::human_input::Gesture::Other);
        crate::app_input::note_person(&ctx, crate::human_input::Gesture::Other);
        ran_tx.send(()).unwrap();
    });
    ran_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("a producer waited on a lock other than the timeline's");
    worker.join().unwrap();
    done_tx.send(()).unwrap();
    holder.join().unwrap();
}

#[test]
fn the_meta_and_fabric_producers_take_only_their_order() {
    use crate::session_timeline::{self as st, MetaEdit, MetaField};
    use std::sync::mpsc;
    let h = test_handle(1);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    let sid = h.ctx.self_id.as_str().to_string();
    let row = format!("{sid} off=7 from=h-a kind=task trust=human text=x");
    assert_eq!(crate::fabric::cmd_deliver(&store, &row), "OK 1\n");
    let (held_tx, held_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let (store2, ctx2) = (Arc::clone(&store), Arc::clone(&h.ctx));
    let holder = std::thread::spawn(move || {
        let _s = store2.write().unwrap();
        let _t = ctx2.turns.lock().unwrap();
        held_tx.send(()).unwrap();
        let _ = done_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    });
    held_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the holder took its locks");
    assert!(store.try_write().is_err(), "the store is held");
    assert!(h.ctx.turns.try_lock().is_err(), "the ledger is held");
    let (ran_tx, ran_rx) = mpsc::channel();
    let ctx = Arc::clone(&h.ctx);
    let worker = std::thread::spawn(move || {
        let now = crate::metrics::now_us();
        let owner = aterm_agent::supervise::run::ATTENTION_OWNER;
        let _ = st::apply_meta_value(&ctx, MetaField::Role, Some("lead".into()), Actor::Ctl);
        let _ = st::write_session_meta(&ctx, MetaField::Title, MetaEdit::Set("t"), Actor::Human);
        let _ = st::apply_autopilot(
            &ctx,
            Some("cruise".into()),
            Some(i64::MAX),
            Actor::Ctl,
            None,
        );
        let _ = st::cancel_land_at(&ctx, Some(i64::MAX), crate::autopilot::Surface::Chip);
        let _ = st::fire_land_at(&ctx, 1, 2);
        let _ = st::write_attention_owned(&ctx, owner, MetaEdit::Set("stuck"), Actor::Harness);
        let _ = st::write_server_attention(&ctx, "aterm", Some("stalled".into()));
        let _ = st::claim_supervisor_as(&ctx, "h", None, Some(now + 1), None, now, Actor::Ctl);
        let _ = st::lapse_supervisor(&ctx, now + 2);
        let _ = st::claim_supervisor_as(&ctx, "h", Some(9), None, None, now, Actor::Ctl);
        let _ = st::release_supervisor_held_by(&ctx, "h", now, Actor::Ctl);
        let _ = st::claim_supervisor_as(&ctx, "h", Some(9), None, None, now, Actor::Ctl);
        let _ = st::release_supervisor(&ctx, Some(9), now, Actor::Aterm);
        st::seed_carried_claims(&ctx.meta, &ctx.timeline, None, &["a=b".to_string()], now);
        let none = || {};
        let _ = crate::fabric::cmd_inbox_seen(&*ctx, "1 handled");
        let _ = crate::fabric::cmd_post(&*ctx, "to=@s-peer kind=ask --wait=0 x", None);
        let hold = crate::fabric::Hold {
            reason: "demo".into(),
            origin: "local".into(),
        };
        let _ = crate::fabric::carry_hold_waking(&*ctx, hold, &none);
        let _ = crate::fabric::apply_hold_for_test(&*ctx, None);
        ran_tx.send(()).unwrap();
    });
    ran_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("a meta or fabric producer waited on a lock outside its order");
    worker.join().unwrap();
    done_tx.send(()).unwrap();
    holder.join().unwrap();
    let got = lines_of(&h.ctx.timeline);
    for kind in [
        " mark - meta ",
        " mark - mail-seen ",
        " mark - mail dir=out ",
        " mark - hold ",
    ] {
        assert!(got.iter().any(|l| l.contains(kind)), "{kind}: {got:?}");
    }
}

fn sample_log() -> Log {
    let mut log = Log::new(3);
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Nobody,
        Fields::new(),
        at(0),
    )
    .unwrap();
    let run = log
        .open(
            aterm_slog::Open::new(Kind::Cmd, Src::Shell, Conf::Strong)
                .fields(Fields::new().u(Key::Blk, 1).unwrap()),
            at(10),
        )
        .unwrap();
    log.mark(
        Mark::new(Kind::Cwd, Src::Engine, Conf::Strong)
            .at(Node::SESSION)
            .fields(Fields::new().text(Key::Cwd, "/home/dev").unwrap()),
        at(20),
    )
    .unwrap();
    log.mark(
        Mark::new(Kind::Title, Src::Engine, Conf::Strong)
            .at(run)
            .fields(Fields::new().text(Key::Title, "SECRET TITLE").unwrap()),
        at(30),
    )
    .unwrap();
    log
}

fn query(rest: &str) -> LogQuery {
    LogQuery::parse(rest).unwrap_or_else(|e| panic!("`{rest}`: {e}"))
}

#[test]
fn log_answers_its_header_then_one_record_per_line() {
    let log = sample_log();
    let reply = render_log(&log, &query(""));
    let mut it = reply.lines();
    assert_eq!(it.next(), Some("OK 4 v=2 as_of=4 ep=3 oldest=1"));
    let body: Vec<&str> = it.collect();
    assert_eq!(body.len(), 4);
    assert!(body[1].starts_with("2 open r1 cmd par=- blk=1 src=shell conf=strong t="));
    assert_eq!(
        render_log(&log, &query("r1")).lines().count(),
        1 + 2,
        "{}",
        render_log(&log, &query("r1"))
    );
    assert_eq!(
        render_log(&log, &query("lvl=session")).lines().count(),
        1 + 2
    );
    assert_eq!(
        render_log(&log, &query("kinds=cwd,title")).lines().count(),
        1 + 2
    );
    assert_eq!(
        render_log(&log, &query("class=turn")).lines().count(),
        1 + 2
    );
    let newest = render_log(&log, &query("n=1"));
    assert!(newest.starts_with("OK 1 v=2 as_of=4 ep=3 oldest=1 more=3\n4 mark r1 title"));
    for bad in [
        "bogus",
        "r1 r2",
        "lvl=up",
        "kinds=nope",
        "class=x",
        "tier=wire",
        "since=3",
        "n=-1",
    ] {
        assert!(LogQuery::parse(bad).is_err(), "`{bad}` should be refused");
    }
}

#[test]
fn log_stops_at_the_host_tier_unless_content_is_asked_for() {
    let log = sample_log();
    let default = render_log(&log, &query(""));
    assert!(default.contains("cwd=/home/dev"), "{default}");
    assert!(!default.contains("SECRET"), "{default}");
    let content = render_log(&log, &query("tier=content"));
    assert!(content.contains("title=SECRET%20TITLE"), "{content}");
    let json = render_log(&log, &query("--json"));
    assert!(!json.contains("SECRET"), "{json}");
    let json = render_log(&log, &query("tier=content --json"));
    let title_line = json.lines().last().unwrap();
    let v: aterm_json::Value = aterm_json::from_str(title_line).expect("one JSON object");
    assert_eq!(
        v.get("title").and_then(|t| t.as_str()),
        Some("SECRET TITLE")
    );
    assert_eq!(v.get("seq").and_then(|t| t.as_u64()), Some(4));
    assert_eq!(v.get("node").and_then(|t| t.as_str()), Some("r1"));
}

#[test]
fn log_tells_a_reader_behind_the_ring_or_of_another_epoch_with_a_gap_line() {
    let log = sample_log();
    let on = render_log(&log, &query("since=2 ep=3"));
    assert!(
        on.starts_with("OK 2 v=2 as_of=4 ep=3 oldest=1\n3 mark "),
        "{on}"
    );
    let stale = render_log(&log, &query("since=2 ep=2"));
    assert!(
        stale.starts_with(
            "OK 5 v=2 as_of=4 ep=3 oldest=1\ngap why=reset lost=0 resync_to=0\n1 open - spawn"
        ),
        "{stale}"
    );

    let mut small = Log::with_limits(1, aterm_slog::Limits::new(3, 4096).unwrap());
    small
        .open_session(
            Kind::Spawn,
            Src::Aterm,
            Conf::Exact,
            Actor::Nobody,
            Fields::new(),
            at(0),
        )
        .unwrap();
    for i in 0..5 {
        small
            .mark(Mark::new(Kind::Human, Src::Input, Conf::Strong), at(i))
            .unwrap();
    }
    let behind = render_log(&small, &query("since=1 ep=1"));
    assert!(
        behind.starts_with(
            "OK 2 v=2 as_of=6 ep=1 oldest=6 lost=4\ngap why=evicted lost=4 resync_to=5\n6 mark"
        ),
        "{behind}"
    );
    let from_start = render_log(&small, &query(""));
    assert!(
        from_start.starts_with(
            "OK 3 v=2 as_of=6 ep=1 oldest=6 lost=4\ngap why=evicted lost=4 resync_to=0\n1 open - spawn"
        ),
        "the pinned open first, the loss spread after it: {from_start}"
    );
}

#[test]
fn log_node_reads_its_subtree_after_its_open_record_is_evicted() {
    let mut log = Log::with_limits(1, aterm_slog::Limits::new(8, 1 << 20).unwrap());
    let open = |log: &mut Log, kind: Kind, under: Option<Node>, ms: u64| {
        let mut o = aterm_slog::Open::new(kind, Src::Shell, Conf::Strong);
        if let Some(p) = under {
            o = o.under(p);
        }
        log.open(o, at(ms)).unwrap()
    };
    let close = |log: &mut Log, n: Node, ms: u64| {
        log.close(
            n,
            aterm_slog::Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
            at(ms),
        )
        .unwrap();
    };
    let r1 = open(&mut log, Kind::Cmd, None, 1);
    let r2 = open(&mut log, Kind::Prog, Some(r1), 2);
    close(&mut log, r2, 3);
    close(&mut log, r1, 4);
    let r3 = open(&mut log, Kind::Cmd, None, 5);
    close(&mut log, r3, 6);
    for i in 0..4 {
        log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(7 + i))
            .unwrap();
    }
    assert!(
        !log.window()
            .any(|r| r.node == r2 && r.op == aterm_slog::Op::Open),
        "PRECONDITION: r2's open record (its parent's name) was evicted"
    );
    let sub = render_log(&log, &query("r1"));
    let body: Vec<&str> = sub.lines().skip(1).collect();
    assert!(
        body.iter().any(|l| l.contains(" close r2 ")),
        "r2's close is r1's: {sub}"
    );
    assert!(
        body.iter().any(|l| l.contains(" close r1 ")),
        "and r1's own: {sub}"
    );
    assert!(
        !body.iter().any(|l| l.contains(" r3 ")),
        "NEGATIVE CONTROL: the sibling is not: {sub}"
    );
}

#[test]
fn brief_names_turns_whose_opens_were_evicted() {
    let mut log = Log::with_limits(1, aterm_slog::Limits::new(24, 1 << 20).unwrap());
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Nobody,
        Fields::new(),
        at(0),
    )
    .unwrap();
    let run = log
        .open(
            aterm_slog::Open::new(Kind::Agent, Src::Status, Conf::Strong),
            at(1),
        )
        .unwrap();
    let mut ms = 10;
    for _ in 0..30 {
        let t = log
            .open(
                aterm_slog::Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                at(ms),
            )
            .unwrap();
        ms += 5_000;
        log.close(
            t,
            aterm_slog::Close::new(
                Out::Turn(aterm_slog::TurnOut::Done),
                Src::Screen,
                Conf::Strong,
            ),
            at(ms),
        )
        .unwrap();
    }
    assert!(
        log.window()
            .filter(|r| r.node.lvl == Lvl::Turn && r.op == aterm_slog::Op::Open)
            .count()
            < 30,
        "PRECONDITION: some turns' opens were evicted"
    );
    let text = render_brief(&log, "s-1", Node::SESSION, None, 100_000, WALL0 + ms);
    assert!(text.contains("t30 done"), "{text}");
    assert_eq!(log.state().totals.turns, 30, "the fold counts every turn");
    let closes_live = log
        .window()
        .filter(|r| r.node.lvl == Lvl::Turn && r.op == aterm_slog::Op::Close)
        .count();
    assert!(closes_live >= 10, "the closes stay: {closes_live}");
}

#[test]
fn cmd_log_reads_the_targets_log() {
    let h = test_handle(1);
    let _hand = hand_clock(&h.ctx.timeline);
    let mut store = SessionStore::default();
    store.register(h.clone());
    let reply = crate::control::cmd_log(&h.ctx, "");
    assert!(
        reply.starts_with("OK 1 v=2 as_of=1 ep=1 oldest=1\n1 open - spawn epoch=1 "),
        "{reply}"
    );
    assert!(crate::control::cmd_log(&h.ctx, "since=1").starts_with("ERR usage: log since="));
}

#[test]
fn brief_answers_the_targets_rollup_and_all_of_them_for_the_owner() {
    let a = test_handle(1);
    let b = test_handle(2);
    let store = crate::session_store::new_store();
    for h in [&a, &b] {
        let _ = hand_clock(&h.ctx.timeline);
        store.write().unwrap().register(h.clone());
    }
    let reply = crate::control::cmd_brief(&a.ctx, &store, "budget=200");
    let head = reply.lines().next().unwrap();
    assert!(
        head.starts_with(&format!(
            "OK {} brief sid={} v=2 as_of=1 ep=1 budget=200 used=",
            reply.lines().count() - 1,
            a.sid.as_str()
        )),
        "{reply}"
    );
    let all = crate::control::cmd_brief(&a.ctx, &store, "all");
    let mut lines = all.lines();
    let head = lines.next().unwrap();
    let body: Vec<&str> = lines.collect();
    assert!(
        head.starts_with(&format!(
            "OK {} brief all sessions=2 v=2 budget=1000 used=",
            body.len()
        )),
        "{all}"
    );
    assert_eq!(
        body.iter().filter(|l| l.starts_with("brief sid=")).count(),
        2,
        "{all}"
    );
    assert!(!body.iter().any(|l| l.starts_with("OK ")), "{all}");
    for bad in ["t1 t2", "budget=x", "all since=1 ep=1", "since=1", "nope"] {
        assert!(BriefQuery::parse(bad).is_err(), "`{bad}`");
    }
}

#[test]
fn a_command_line_never_reaches_the_log() {
    let (tl, hand) = opened();
    let read = BlockRead {
        facts: vec![BlockFact {
            id: 1,
            started_ms: Some(WALL0),
            end: BlockEnd::Running,
            detail: crate::session_status::command_detail("git commit -m 'secret message'"),
        }],
        ..BlockRead::default()
    };
    blocks(&tl, &hand, 10, &read);
    let all = render_log(tl.lock().unwrap().slog().log(), &query("tier=content"));
    assert!(all.contains("detail=git%20commit"), "{all}");
    assert!(!all.contains("secret"), "{all}");
}

#[test]
fn an_interpreter_renamed_to_its_agent_confirms_the_run_and_keeps_the_turn() {
    let claude = Some(aterm_phase::Program::Claude);
    let (tl, hand) = opened();
    {
        let mut g = tl.lock().unwrap();
        hand.set(10);
        g.note_foreground_group(300);
        g.set_program(300, Some("node".into()));
        hand.set(20);
        g.publish_agent("busy", None, None, claude, stamp(1));
        hand.set(600);
        assert!(g.set_program(300, Some("claude".into())));
        hand.set(700);
        g.publish_agent(
            "prompt",
            Some("bash:not-read-only".into()),
            None,
            claude,
            stamp(2),
        );
    }
    let got = lines_of(&tl);
    assert!(!got.iter().any(|l| l.contains(" close ")), "{got:?}");
    assert!(
        got.iter()
            .any(|l| l.starts_with("3 open r2 agent par=r1 agent=claude src=screen ")),
        "{got:?}"
    );
    assert!(
        got.iter()
            .any(|l| l.contains(" amend r2 agent prog=claude src=status ")),
        "{got:?}"
    );
    assert_eq!(
        got.iter().filter(|l| l.contains(" turn par=")).count(),
        1,
        "one turn: {got:?}"
    );
    assert!(
        got.iter()
            .any(|l| l.contains(" open p1 approval par=t1 box=bash:not-read-only ")),
        "{got:?}"
    );
    let run2 = Node::new(aterm_slog::Lvl::Run, 2);
    {
        let mut g = tl.lock().unwrap();
        hand.set(800);
        g.publish_agent("-", None, None, None, stamp(3));
        assert!(
            g.slog().log().is_open(run2),
            "confirmed: the screen's `-` is not its end"
        );
        hand.set(900);
        g.note_foreground_group(100);
        assert!(!g.slog().log().is_open(run2), "its group left: it ended");
    }

    let (tl, hand) = opened();
    {
        let mut g = tl.lock().unwrap();
        hand.set(10);
        g.note_foreground_group(300);
        g.set_program(300, Some("node".into()));
        hand.set(20);
        g.publish_agent("busy", None, None, claude, stamp(1));
        hand.set(600);
        g.set_program(300, Some("vim".into()));
        swept(&mut g);
    }
    let got = lines_of(&tl);
    assert!(
        got.iter().any(|l| l.contains(" close r2 agent out=gone "))
            && got
                .iter()
                .any(|l| l.contains(" close r1 prog out=superseded "))
            && got
                .iter()
                .any(|l| l.contains(" open r3 prog par=- prog=vim ")),
        "{got:?}"
    );
    assert!(
        !got.iter().any(|l| l.contains(" amend ")),
        "a closed node is never amended: {got:?}"
    );
}

fn ctl_turn_through_the_verb(
    h: &crate::session_store::SessionHandle,
    args: &str,
    io: &crate::control::TurnIo<'_>,
) -> u64 {
    let store = crate::session_store::new_store();
    let reply = crate::control::cmd_turn(&h.term, &store, 0, args, &h.ctx, io);
    assert!(reply.starts_with("OK "), "{reply}");
    let ledger = h.ctx.turns.lock().unwrap().since(None).last().map(|r| r.id);
    ledger.expect("the exchange is on the ledger")
}

fn claude_handle() -> (crate::session_store::SessionHandle, Hand) {
    let h = test_handle(0);
    let hand = hand_clock(&h.ctx.timeline);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, None, now);
        g.note_root_group(100);
    }
    with_claude(&h.ctx.timeline);
    (h, hand)
}

#[test]
fn a_persons_keys_during_the_yield_do_not_take_the_exchanges_turn() {
    let (h, hand) = claude_handle();
    let (ctx, during) = (Arc::clone(&h.ctx), hand.clone());
    let momentum = move || {
        during.set(1_000);
        crate::app_input::note_person(&ctx, crate::human_input::Gesture::Other);
        Ok((0.0_f32, std::time::Instant::now()))
    };
    hand.set(10);
    let ledger = ctl_turn_through_the_verb(
        &h,
        "idle=1 timeout=60000 submit=none yield=0.2 hello",
        &crate::control::TurnIo {
            paste: &|_: &str| true,
            momentum: &momentum,
            ..crate::control::TurnIo::paste_only()
        },
    );
    hand.set(2_000);
    h.ctx.timeline.lock().unwrap().publish_agent(
        "busy",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        stamp(1),
    );
    let got = lines_of(&h.ctx.timeline);
    assert!(
        got.iter().any(|l| l.contains(" mark - human by=human ")),
        "the person's burst is said: {got:?}"
    );
    assert!(
        got.iter()
            .any(|l| l.contains(&format!(" open t1 turn par=r1 ledger={ledger} by=ctl "))),
        "{got:?}"
    );
}

#[test]
fn a_typed_turn_longer_than_the_attribution_window_is_still_the_exchanges() {
    let text = "a".repeat(60);
    for enter in [true, false] {
        let (h, hand) = claude_handle();
        let keys = hand.clone();
        let key = move |_| {
            keys.advance(100);
            true
        };
        let pressed = hand.clone();
        let press = move |_: &str| {
            pressed.advance(100);
            true
        };
        let args = if enter {
            format!(
                "idle=1 timeout=60000 typed=1 cadence=1 submit_verify=seq presses=1 \
                 submit_window=1 {text}"
            )
        } else {
            format!("idle=1 timeout=60000 typed=1 cadence=1 submit=none {text}")
        };
        let ledger = ctl_turn_through_the_verb(
            &h,
            &args,
            &crate::control::TurnIo {
                key: &key,
                press: &press,
                ..crate::control::TurnIo::paste_only()
            },
        );
        hand.advance(if enter { 110 } else { 5_100 });
        h.ctx.timeline.lock().unwrap().publish_agent(
            "busy",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(1),
        );
        let got = lines_of(&h.ctx.timeline);
        let opened = got
            .iter()
            .find(|l| l.contains(" open t1 turn "))
            .unwrap_or_else(|| panic!("a turn opened: {got:?}"));
        if enter {
            assert!(
                opened.contains(&format!(" open t1 turn par=r1 ledger={ledger} by=ctl ")),
                "{opened}"
            );
        } else {
            assert!(
                opened.contains(" open t1 turn par=r1 src=screen "),
                "no credit past the window: {opened}"
            );
        }
    }
}

#[test]
fn a_typed_exchange_with_no_enter_is_noted_at_its_first_key() {
    for exchange in [true, false] {
        let (h, hand) = claude_handle();
        let publish = |word: &'static str, seq: u64| {
            h.ctx.timeline.lock().unwrap().publish_agent(
                word,
                None,
                None,
                Some(aterm_phase::Program::Claude),
                stamp(seq),
            );
        };
        hand.set(100);
        publish("busy", 1);
        hand.set(200);
        publish("idle", 2);
        assert!(
            h.ctx.timeline.lock().unwrap().slog_wake().is_some(),
            "PRECONDITION: the turn is resting, its settle armed"
        );
        hand.set(300);
        let ledger = exchange.then(|| {
            let keys = hand.clone();
            let key = move |_| {
                keys.advance(100);
                true
            };
            ctl_turn_through_the_verb(
                &h,
                "idle=1 timeout=60000 typed=1 cadence=1 submit=none abc",
                &crate::control::TurnIo {
                    key: &key,
                    ..crate::control::TurnIo::paste_only()
                },
            )
        });
        hand.set(700);
        publish("busy", 3);
        let got = lines_of(&h.ctx.timeline);
        if let Some(ledger) = ledger {
            assert!(
                got.iter().any(|l| l.contains(" close t1 turn ")),
                "the first key closed the resting turn: {got:?}"
            );
            assert!(
                got.iter()
                    .any(|l| l.contains(&format!(" open t2 turn par=r1 ledger={ledger} by=ctl "))),
                "{got:?}"
            );
        } else {
            assert!(
                !got.iter()
                    .any(|l| l.contains(" close t1 turn ") || l.contains(" open t2 ")),
                "NEGATIVE CONTROL: one turn goes on: {got:?}"
            );
        }
    }
}

fn first_turn_after(
    typed: &dyn Fn(&crate::session_store::SessionHandle, &Hand) -> Option<u64>,
) -> (Vec<String>, Option<u64>) {
    let h = test_handle(0);
    let hand = hand_clock(&h.ctx.timeline);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, None, now);
        g.note_root_group(100);
    }
    blocks(&h.ctx.timeline, &hand, 10, &running(1, 5, "claude"));
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        hand.set(20);
        g.note_foreground_group(300);
        hand.set(30);
        assert!(g.set_program(300, Some("claude".into())));
    }
    let ledger = typed(&h, &hand);
    hand.set(270);
    swept_with(
        &mut h.ctx.timeline.lock().unwrap(),
        &running(1, 5, "claude"),
    );
    hand.set(275);
    h.ctx.timeline.lock().unwrap().publish_agent(
        "busy",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        stamp(1),
    );
    (lines_of(&h.ctx.timeline), ledger)
}

#[test]
fn input_while_the_name_waits_attributes_the_first_turn() {
    let turn = |got: &[String]| {
        got.iter()
            .find(|l| l.contains(" open t1 turn par=r2 "))
            .unwrap_or_else(|| panic!("the first turn, under the agent run: {got:?}"))
            .clone()
    };
    let at = |ms: u64| format!("t={}", WALL0 + ms);
    let exchange = |h: &crate::session_store::SessionHandle| {
        ctl_turn_through_the_verb(
            h,
            "idle=1 timeout=60000 submit=none hello",
            &crate::control::TurnIo {
                paste: &|_: &str| true,
                ..crate::control::TurnIo::paste_only()
            },
        )
    };

    let (got, _) = first_turn_after(&|h, hand| {
        hand.set(100);
        crate::app_input::note_person(&h.ctx, crate::human_input::Gesture::Other);
        None
    });
    let t1 = turn(&got);
    assert!(
        t1.contains(" par=r2 by=human ") && t1.ends_with(&at(275)),
        "a person's input: {got:?}"
    );

    let (got, ledger) = first_turn_after(&|h, hand| {
        hand.set(100);
        Some(exchange(h))
    });
    let ledger = ledger.expect("the exchange's ledger id");
    assert!(
        turn(&got).contains(&format!(" par=r2 ledger={ledger} by=ctl ")),
        "a ctl turn: {got:?}"
    );

    let (got, _) = first_turn_after(&|h, hand| {
        hand.set(100);
        let ledger = exchange(h);
        hand.set(150);
        crate::app_input::note_person(&h.ctx, crate::human_input::Gesture::Other);
        Some(ledger)
    });
    let t1 = turn(&got);
    assert!(
        t1.contains(" par=r2 by=human ") && !t1.contains(" ledger="),
        "keys after the exchange: {got:?}"
    );
    let (got, ledger) = first_turn_after(&|h, hand| {
        hand.set(100);
        crate::app_input::note_person(&h.ctx, crate::human_input::Gesture::Other);
        hand.set(150);
        Some(exchange(h))
    });
    let ledger = ledger.expect("the exchange's ledger id");
    assert!(
        turn(&got).contains(&format!(" par=r2 ledger={ledger} by=ctl ")),
        "the exchange after the keys: {got:?}"
    );

    let early = |by_ms: u64| {
        let h = test_handle(0);
        let hand = hand_clock(&h.ctx.timeline);
        {
            let mut g = h.ctx.timeline.lock().unwrap();
            let now = g.slog().now();
            g.slog_mut().open_session(None, None, now);
            g.note_root_group(100);
        }
        if by_ms < 10 {
            hand.set(by_ms);
            let mut g = h.ctx.timeline.lock().unwrap();
            let now = g.slog().now();
            g.slog_mut().ctl_input(
                Actor::Ctl,
                super::InputVerb::Send,
                None,
                super::InputAct::default(),
                now,
            );
        }
        blocks(&h.ctx.timeline, &hand, 10, &running(1, 5, "claude"));
        if by_ms >= 10 {
            hand.set(by_ms);
            let mut g = h.ctx.timeline.lock().unwrap();
            let now = g.slog().now();
            g.slog_mut().ctl_input(
                Actor::Ctl,
                super::InputVerb::Send,
                None,
                super::InputAct::default(),
                now,
            );
        }
        {
            let mut g = h.ctx.timeline.lock().unwrap();
            hand.set(20);
            g.note_foreground_group(300);
            hand.set(30);
            assert!(g.set_program(300, Some("claude".into())));
            hand.set(270);
            swept_with(&mut g, &running(1, 5, "claude"));
            hand.set(275);
            g.publish_agent(
                "busy",
                None,
                None,
                Some(aterm_phase::Program::Claude),
                stamp(1),
            );
        }
        lines_of(&h.ctx.timeline)
    };
    let got = early(15);
    assert!(
        turn(&got).contains(" par=r2 by=ctl "),
        "a prompt sent before the name, after the command began: {got:?}"
    );
    let got = early(3);
    assert!(
        !turn(&got).contains(" by="),
        "NEGATIVE CONTROL: control input before the command began: {got:?}"
    );

    let h = test_handle(0);
    let hand = hand_clock(&h.ctx.timeline);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, None, now);
        g.note_root_group(100);
    }
    hand.set(3);
    crate::app_input::note_person(&h.ctx, crate::human_input::Gesture::Other);
    blocks(&h.ctx.timeline, &hand, 10, &running(1, 5, "claude"));
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        hand.set(20);
        g.note_foreground_group(300);
        hand.set(30);
        assert!(g.set_program(300, Some("claude".into())));
        hand.set(270);
        swept_with(&mut g, &running(1, 5, "claude"));
        hand.set(275);
        g.publish_agent(
            "busy",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(1),
        );
    }
    let got = lines_of(&h.ctx.timeline);
    assert!(
        !turn(&got).contains(" by="),
        "NEGATIVE CONTROL: the shell's input is not the agent's: {got:?}"
    );
}

#[test]
fn a_command_from_before_this_process_measures_its_duration_on_the_wall() {
    let tl = Mutex::new(SessionTimeline::for_test());
    let hand = hand_clock(&tl);
    hand.set(10_000);
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().adopt(LogAdopt::default(), false, None, now);
        g.slog_mut().open_session(None, None, now);
    }
    let began = WALL0 - 3_600_000;
    let read = |end: BlockEnd| BlockRead {
        facts: vec![BlockFact {
            id: 7,
            started_ms: Some(began),
            end,
            detail: Some("claude".into()),
        }],
        ..BlockRead::default()
    };
    blocks(&tl, &hand, 10_000, &read(BlockEnd::Running));
    let end = BlockEnd::Done(crate::session_status::CompletedBlock {
        id: 7,
        exit_code: Some(0),
        started_ms: Some(began),
        ended_ms: Some(WALL0 + 20_000),
    });
    blocks(&tl, &hand, 20_500, &read(end));
    let got = lines_of(&tl);
    assert!(
        got.iter()
            .any(|l| l.ends_with(&format!("detail=claude src=shell conf=strong t={began}"))),
        "{got:?}"
    );
    assert!(
        got.iter().any(|l| l.contains(&format!(
            " close r1 cmd out=ok dur_ms={} dur=wall code=0 src=shell conf=exact t={}",
            WALL0 + 20_000 - began,
            WALL0 + 20_000
        ))),
        "{got:?}"
    );
    blocks(&tl, &hand, 21_000, &running(8, 20_900, "make"));
    blocks(
        &tl,
        &hand,
        22_000,
        &finished(8, Some(0), 20_900, 21_900, "make"),
    );
    assert!(
        lines_of(&tl)
            .iter()
            .any(|l| l.contains(" close r2 cmd out=ok dur_ms=1000 code=0 ")),
        "{:?}",
        lines_of(&tl)
    );
}

/// THE RUNS' TIME ORDER over the records `lines`: (1) a child's close `t=` and `dur_ms` never exceed its parent's; (2) no node closes before it opened; (3) a `cmd` run never opens before the `cmd` run logged before it (the shell runs one command at a time) closed; (4) no node opens before the node it hangs under (the session node aside: a command carried from a predecessor began before this log's session opened).
pub(super) fn time_order_holds(lines: &[String]) -> Result<(), String> {
    let field = |l: &str, k: &str| -> Option<u64> {
        l.split(' ')
            .find_map(|w| w.strip_prefix(k))
            .and_then(|v| v.parse().ok())
    };
    let mut parent: HashMap<String, String> = HashMap::new();
    let mut opened: HashMap<String, u64> = HashMap::new();
    let mut closed: HashMap<String, (u64, u64)> = HashMap::new();
    let mut cmds: Vec<String> = Vec::new();
    for l in lines {
        let w: Vec<&str> = l.split(' ').collect();
        let t = field(l, "t=").unwrap_or(0);
        match (w.get(1), w.get(2), w.get(3)) {
            (Some(&"open"), Some(node), kind) => {
                if let Some(p) = l.split(' ').find_map(|x| x.strip_prefix("par=")) {
                    if let Some(pt) = opened.get(p).filter(|_| p != "-")
                        && t < *pt
                    {
                        return Err(format!(
                            "{node} opened t={t}, before its parent {p} opened (t={pt})"
                        ));
                    }
                    parent.insert((*node).to_string(), p.to_string());
                }
                if kind == Some(&"cmd") {
                    if let Some(prev) = cmds.last()
                        && let Some((pt, _)) = closed.get(prev)
                        && t < *pt
                    {
                        return Err(format!(
                            "{node} opened t={t}, before {prev} closed (t={pt})"
                        ));
                    }
                    cmds.push((*node).to_string());
                }
                opened.insert((*node).to_string(), t);
            }
            (Some(&"close"), Some(node), _) => {
                if let Some(ot) = opened.get(*node)
                    && t < *ot
                {
                    return Err(format!("{node} closed t={t}, before it opened (t={ot})"));
                }
                let dur = field(l, "dur_ms=").unwrap_or(0);
                closed.insert((*node).to_string(), (t, dur));
            }
            _ => {}
        }
    }
    for (child, (t, dur)) in &closed {
        if let Some(p) = parent.get(child)
            && let Some((pt, pdur)) = closed.get(p)
            && (t > pt || dur > pdur)
        {
            return Err(format!(
                "{child} closed t={t} dur_ms={dur}, after its parent {p} (t={pt} dur_ms={pdur})"
            ));
        }
    }
    Ok(())
}

#[test]
fn a_program_never_closes_after_the_command_that_ran_it() {
    for d_first in [true, false] {
        let (tl, hand) = opened();
        blocks(&tl, &hand, 10, &running(1, 5, "less"));
        {
            let mut g = tl.lock().unwrap();
            hand.set(20);
            g.note_foreground_group(200);
            g.set_program(200, Some("less".into()));
        }
        hand.set(1_000);
        {
            let mut g = tl.lock().unwrap();
            let read = finished(1, Some(0), 5, 800, "less");
            if d_first {
                assert_eq!(g.sweep_slog(&read, false, 100, 100), Some(200));
            } else {
                g.note_foreground_group(100);
                let now = g.slog().now();
                g.slog_mut().shell_blocks(&read, None, now);
            }
        }
        let got = lines_of(&tl);
        time_order_holds(&got).unwrap_or_else(|e| panic!("d_first={d_first}: {e}: {got:?}"));
        let at_d = got.iter().any(|l| {
            l.contains(&format!(
                " close r2 prog out=unknown dur_ms=780 src=status conf=strong t={}",
                WALL0 + 800
            ))
        }) && got.iter().any(|l| {
            l.contains(&format!(
                " close r1 cmd out=ok dur_ms=795 code=0 src=shell conf=exact t={}",
                WALL0 + 800
            ))
        });
        assert_eq!(at_d, d_first, "d_first={d_first}: {got:?}");
        if !d_first {
            assert!(
                got.iter().any(|l| l.contains(&format!(
                    " close r1 cmd out=ok dur_ms=995 code=0 src=shell conf=exact t={}",
                    WALL0 + 1_000
                ))),
                "lifted to its child's close: {got:?}"
            );
        }
    }
    let mut log = Log::new(1);
    let cmd = log
        .open(
            aterm_slog::Open::new(Kind::Cmd, Src::Shell, Conf::Strong),
            at(0),
        )
        .unwrap();
    let prog = log
        .open(
            aterm_slog::Open::new(Kind::Prog, Src::Status, Conf::Strong).under(cmd),
            at(10),
        )
        .unwrap();
    let close = |out| aterm_slog::Close::new(aterm_slog::Out::Run(out), Src::Status, Conf::Strong);
    log.close(prog, close(aterm_slog::RunOut::Unknown), at(200))
        .unwrap();
    log.close(cmd, close(aterm_slog::RunOut::Ok), at(100))
        .unwrap();
    assert!(time_order_holds(&lines(&log)).is_err(), "{:?}", lines(&log));
}

fn cmd_of(got: &[String], blk: u64) -> String {
    got.iter()
        .find(|l| l.contains(" open ") && l.contains(&format!(" cmd par=- blk={blk} ")))
        .and_then(|l| l.split(' ').nth(2))
        .unwrap_or_else(|| panic!("no cmd run for block {blk}: {got:?}"))
        .to_string()
}

fn typeahead(pgid: i32, name: &str, named: u64) -> (Mutex<SessionTimeline>, Hand) {
    let (tl, hand) = opened();
    blocks(&tl, &hand, 10, &running(1, 5, "vim"));
    {
        let mut g = tl.lock().unwrap();
        hand.set(20);
        g.note_foreground_group(200);
        g.set_program(200, Some("vim".into()));
    }
    blocks(&tl, &hand, 1_000, &running(1, 5, "vim"));
    {
        let mut g = tl.lock().unwrap();
        hand.set(1_003);
        g.note_foreground_group(pgid);
        hand.set(named);
        g.set_program(pgid, Some(name.into()));
    }
    (tl, hand)
}

#[test]
fn a_program_named_after_the_d_keeps_the_runs_in_time_order() {
    let t = |ms: u64| format!("t={}", WALL0 + ms);
    let has = |got: &[String], what: &str, ms: u64| {
        got.iter().any(|l| l.contains(what) && l.ends_with(&t(ms)))
    };
    let vim_closed_at_the_move = |got: &[String]| {
        assert!(
            has(got, " close r2 prog out=unknown dur_ms=983 ", 1_003)
                && has(got, " close r1 cmd out=ok dur_ms=998 code=0 ", 1_003),
            "vim's command closes with its run, at the move: {got:?}"
        );
    };

    let (tl, hand) = typeahead(400, "sleep", 1_010);
    let read = BlockRead {
        facts: vec![
            fact(1, 5, done(1, Some(0), 5, 1_001), Some("vim")),
            fact(2, 1_002, BlockEnd::Running, Some("sleep")),
        ],
        ..BlockRead::default()
    };
    hand.set(1_250);
    assert_eq!(tl.lock().unwrap().sweep_slog(&read, false, 100, 400), None);
    hand.set(1_500);
    assert_eq!(
        tl.lock()
            .unwrap()
            .sweep_slog(&running(2, 1_002, "sleep"), false, 100, 400),
        None
    );
    hand.set(1_600);
    assert!(!tl.lock().unwrap().set_program(400, Some("sleep".into())));
    let got = lines_of(&tl);
    time_order_holds(&got).unwrap_or_else(|e| panic!("A: {e}: {got:?}"));
    vim_closed_at_the_move(&got);
    let blk2 = cmd_of(&got, 2);
    assert!(
        has(&got, " cmd par=- blk=2 detail=sleep ", 1_003),
        "A: block 2 opens as vim's command closed: {got:?}"
    );
    assert!(
        has(
            &got,
            &format!(" open r4 prog par={blk2} prog=sleep "),
            1_010
        ),
        "A: sleep's run hangs under its own command, at its name: {got:?}"
    );
    {
        let g = tl.lock().unwrap();
        let open = g.slog().log().open_nodes();
        assert!(
            open.iter().any(|e| e.kind == Kind::Prog
                && e.parent.to_string() == blk2
                && g.slog().log().is_open(e.node)),
            "A: sleep, still in front, has an open run under block 2: {got:?}"
        );
    }

    let claude = Some(aterm_phase::Program::Claude);
    let (tl, hand) = typeahead(300, "claude", 1_010);
    let read = BlockRead {
        facts: vec![
            fact(1, 5, done(1, Some(0), 5, 1_001), Some("vim")),
            fact(2, 1_002, BlockEnd::Running, Some("claude")),
        ],
        ..BlockRead::default()
    };
    hand.set(1_250);
    assert_eq!(tl.lock().unwrap().sweep_slog(&read, false, 100, 300), None);
    {
        let mut g = tl.lock().unwrap();
        hand.set(1_260);
        g.publish_agent("busy", None, None, claude, stamp(1));
    }
    hand.set(1_500);
    assert_eq!(
        tl.lock()
            .unwrap()
            .sweep_slog(&running(2, 1_002, "claude"), false, 100, 300),
        None
    );
    let got = lines_of(&tl);
    time_order_holds(&got).unwrap_or_else(|e| panic!("A2: {e}: {got:?}"));
    vim_closed_at_the_move(&got);
    let blk2 = cmd_of(&got, 2);
    assert!(
        has(
            &got,
            &format!(" open r4 agent par={blk2} prog=claude agent=claude "),
            1_010
        ) && has(&got, " open t1 turn par=r4 ", 1_260),
        "A2: {got:?}"
    );
    assert!(
        !got.iter()
            .any(|l| l.contains(" close r4 ") || l.contains(" close t1 ")),
        "A2: the agent in front and its turn stay open: {got:?}"
    );

    let (tl, hand) = typeahead(400, "ls", 1_010);
    let read = BlockRead {
        facts: vec![
            fact(1, 5, done(1, Some(0), 5, 1_001), Some("vim")),
            fact(2, 1_002, done(2, Some(0), 1_002, 1_020), Some("ls")),
        ],
        ..BlockRead::default()
    };
    hand.set(1_250);
    assert_eq!(
        tl.lock().unwrap().sweep_slog(&read, false, 100, 100),
        Some(400)
    );
    let got = lines_of(&tl);
    time_order_holds(&got).unwrap_or_else(|e| panic!("ls: {e}: {got:?}"));
    vim_closed_at_the_move(&got);
    let blk2 = cmd_of(&got, 2);
    assert!(
        has(&got, &format!(" open r4 prog par={blk2} prog=ls "), 1_010)
            && has(&got, " close r4 prog out=unknown dur_ms=10 ", 1_020)
            && has(&got, &format!(" close {blk2} cmd out=ok "), 1_020),
        "ls's run closes at ls's D, with its command: {got:?}"
    );

    let (tl, hand) = typeahead(400, "ls", 1_200);
    let read = BlockRead {
        facts: vec![
            fact(1, 5, done(1, Some(0), 5, 1_001), Some("make")),
            fact(2, 1_002, done(2, Some(0), 1_002, 1_020), Some("ls")),
            fact(3, 1_030, done(3, Some(1), 1_030, 1_040), Some("false")),
        ],
        ..BlockRead::default()
    };
    hand.set(1_250);
    assert_eq!(
        tl.lock().unwrap().sweep_slog(&read, false, 100, 100),
        Some(400)
    );
    let got = lines_of(&tl);
    time_order_holds(&got).unwrap_or_else(|e| panic!("B: {e}: {got:?}"));
    vim_closed_at_the_move(&got);
    let (blk2, blk3) = (cmd_of(&got, 2), cmd_of(&got, 3));
    assert!(
        has(&got, &format!(" close {blk2} cmd out=ok "), 1_020)
            && has(&got, " cmd par=- blk=3 detail=false ", 1_030)
            && has(&got, &format!(" close {blk3} cmd out=fail "), 1_040),
        "B: the read's commands keep their own stamps: {got:?}"
    );
    assert!(
        has(&got, " prog par=- prog=ls ", 1_200),
        "B: a name no running command took is the session's: {got:?}"
    );

    let mut log = Log::new(1);
    let run = |log: &mut Log, open: u64, close: u64| {
        let r = log
            .open(
                aterm_slog::Open::new(Kind::Cmd, Src::Shell, Conf::Strong),
                at(open),
            )
            .unwrap();
        log.close(
            r,
            aterm_slog::Close::new(
                aterm_slog::Out::Run(aterm_slog::RunOut::Ok),
                Src::Shell,
                Conf::Exact,
            ),
            at(close),
        )
        .unwrap();
    };
    run(&mut log, 100, 50);
    assert!(
        time_order_holds(&lines(&log)).is_err_and(|e| e.contains("before it opened")),
        "{:?}",
        lines(&log)
    );
    let mut log = Log::new(1);
    run(&mut log, 100, 200);
    run(&mut log, 150, 300);
    assert!(
        time_order_holds(&lines(&log)).is_err_and(|e| e.contains("before r1 closed")),
        "{:?}",
        lines(&log)
    );
}

fn cut_read(ids: std::ops::RangeInclusive<u64>, last: Option<(u64, u64, &str)>) -> BlockRead {
    let mut facts: Vec<BlockFact> = ids
        .map(|i| {
            let start = 100 + 10 * i;
            fact(i, start, done(i, Some(0), start, start + 5), Some("true"))
        })
        .collect();
    if let Some((blk, start, detail)) = last {
        facts.push(fact(blk, start, BlockEnd::Running, Some(detail)));
    }
    BlockRead {
        more: last.is_none(),
        facts,
        ..BlockRead::default()
    }
}

fn named_past_a_cut(pgid: i32, prog: &str, verdict: Option<&'static str>) -> Vec<String> {
    let (tl, _) = past_a_cut(pgid, prog, verdict);
    lines_of(&tl)
}

fn past_a_cut(
    pgid: i32,
    prog: &str,
    verdict: Option<&'static str>,
) -> (Mutex<SessionTimeline>, Hand) {
    let (tl, hand) = opened();
    hand.set(1_000);
    let _ = tl
        .lock()
        .unwrap()
        .sweep_slog(&cut_read(1..=32, None), false, 100, pgid);
    hand.set(1_005);
    assert!(tl.lock().unwrap().set_program(pgid, Some(prog.into())));
    hand.set(1_250);
    let _ = tl
        .lock()
        .unwrap()
        .sweep_slog(&cut_read(33..=64, None), false, 100, pgid);
    if let Some(word) = verdict {
        hand.set(1_260);
        tl.lock()
            .unwrap()
            .publish_agent(word, None, None, aterm_phase::program_of(prog), stamp(1));
    }
    hand.set(1_500);
    let _ =
        tl.lock()
            .unwrap()
            .sweep_slog(&cut_read(65..=69, Some((70, 800, prog))), false, 100, pgid);
    (tl, hand)
}

#[test]
fn a_name_past_a_cut_read_waits_for_the_read_that_reaches_its_command() {
    let t = |ms: u64| format!("t={}", WALL0 + ms);
    let got = named_past_a_cut(400, "sleep", None);
    time_order_holds(&got).unwrap_or_else(|e| panic!("sleep: {e}: {got:?}"));
    let blk70 = cmd_of(&got, 70);
    assert!(
        got.iter().any(
            |l| l.contains(&format!(" prog par={blk70} prog=sleep ")) && l.ends_with(&t(1_005))
        ),
        "sleep's run hangs under its own command: {got:?}"
    );
    assert!(
        got.iter()
            .filter(|l| l.contains(" cmd par="))
            .all(|l| l.contains(" cmd par=- ")),
        "every command is the session's: {got:?}"
    );

    let got = named_past_a_cut(300, "claude", Some("busy"));
    time_order_holds(&got).unwrap_or_else(|e| panic!("claude: {e}: {got:?}"));
    let agent = got
        .iter()
        .find(|l| l.contains(" agent par=") && l.contains(" prog=claude "))
        .and_then(|l| l.split(' ').nth(2))
        .unwrap_or_else(|| panic!("claude's agent run: {got:?}"))
        .to_string();
    assert!(
        got.iter()
            .filter(|l| l.contains(" cmd par="))
            .all(|l| l.contains(" cmd par=- ")),
        "no command hangs under the agent run that opened after it: {got:?}"
    );
    assert!(
        got.iter()
            .any(|l| l.contains(&format!(" open t1 turn par={agent} ")) && l.ends_with(&t(1_260))),
        "the agent's turn: {got:?}"
    );

    let mut log = Log::new(1);
    let run = log
        .open(
            aterm_slog::Open::new(Kind::Prog, Src::Status, Conf::Strong),
            at(1_005),
        )
        .unwrap();
    log.open(
        aterm_slog::Open::new(Kind::Cmd, Src::Shell, Conf::Strong).under(run),
        at(750),
    )
    .unwrap();
    assert!(
        time_order_holds(&lines(&log)).is_err_and(|e| e.contains("before its parent")),
        "{:?}",
        lines(&log)
    );
}

fn opened_node(got: &[String], what: &str) -> String {
    got.iter()
        .find(|l| l.contains(" open ") && l.contains(what))
        .and_then(|l| l.split(' ').nth(2))
        .unwrap_or_else(|| panic!("no open record with {what:?}: {got:?}"))
        .to_string()
}

fn nested_shell() -> (Mutex<SessionTimeline>, Hand) {
    let (tl, hand) = opened();
    {
        let mut g = tl.lock().unwrap();
        hand.set(20);
        assert!(g.note_foreground_group(400));
        hand.set(300);
        assert!(g.set_program(400, Some("bash".into())));
    }
    (tl, hand)
}

fn sweep_told(g: &mut SessionTimeline, read: &BlockRead, front: Option<i32>) {
    match front {
        Some(pgid) => {
            let _ = g.sweep_slog(read, false, 100, pgid);
        }
        None => {
            let now = g.slog().now();
            g.slog_mut().shell_blocks(read, None, now);
        }
    }
}

#[test]
fn an_agent_run_opened_past_a_cut_closes_at_its_commands_d() {
    let t = |ms: u64| format!("t={}", WALL0 + ms);
    for ls_done in [false, true] {
        let (tl, hand) = past_a_cut(300, "claude", Some("busy"));
        let (ls, front) = if ls_done {
            (done(71, Some(2), 2_010, 2_050), 100)
        } else {
            (BlockEnd::Running, 500)
        };
        let read = BlockRead {
            facts: vec![
                fact(70, 800, done(70, Some(0), 800, 2_000), Some("claude")),
                fact(71, 2_010, ls, Some("ls")),
            ],
            ..BlockRead::default()
        };
        hand.set(2_100);
        let _ = tl.lock().unwrap().sweep_slog(&read, false, 100, front);
        hand.set(2_300);
        let _ = tl.lock().unwrap().sweep_slog(
            &finished(71, Some(2), 2_010, 2_050, "ls"),
            false,
            100,
            100,
        );
        let got = lines_of(&tl);
        time_order_holds(&got).unwrap_or_else(|e| panic!("ls_done={ls_done}: {e}: {got:?}"));
        let agent = opened_node(&got, " agent par=- prog=claude ");
        assert!(
            got.iter().any(
                |l| l.contains(&format!(" close {agent} agent out=unknown "))
                    && l.ends_with(&t(2_000))
            ),
            "ls_done={ls_done}: the agent run closes at its command's D: {got:?}"
        );
        let ls = cmd_of(&got, 71);
        assert!(
            got.iter()
                .any(|l| l.contains(&format!(" close {ls} cmd out=fail "))
                    && l.contains(" code=2 ")
                    && l.ends_with(&t(2_050))),
            "ls_done={ls_done}: ls keeps its own D: {got:?}"
        );
        assert!(
            !got.iter().any(|l| l.contains(" cmd out=gone ")),
            "ls_done={ls_done}: {got:?}"
        );
        assert_eq!(
            tl.lock()
                .unwrap()
                .slog()
                .log()
                .state()
                .top_need()
                .map(|n| n.word),
            Some("failure"),
            "ls_done={ls_done}: {got:?}"
        );
    }

    let (tl, hand) = nested_shell();
    hand.set(350);
    sweep_told(&mut tl.lock().unwrap(), &BlockRead::default(), Some(400));
    hand.set(500);
    sweep_told(
        &mut tl.lock().unwrap(),
        &finished(1, Some(0), 400, 410, "ls"),
        Some(400),
    );
    let got = lines_of(&tl);
    time_order_holds(&got).unwrap_or_else(|e| panic!("nested: {e}: {got:?}"));
    let bash = opened_node(&got, " prog par=- prog=bash ");
    assert!(
        got.iter()
            .any(|l| l.contains(&format!(" cmd par={bash} blk=1 detail=ls ")))
            && !got.iter().any(|l| l.contains(&format!(" close {bash} "))),
        "ls hangs under bash, which outlives ls's D: {got:?}"
    );
}

#[test]
fn a_running_command_never_hangs_under_a_run_the_sweeps_move_closes() {
    let t = |ms: u64| format!("t={}", WALL0 + ms);
    let vim = |front: Option<i32>| -> Vec<String> {
        let (tl, hand) = nested_shell();
        hand.set(350);
        sweep_told(&mut tl.lock().unwrap(), &BlockRead::default(), Some(400));
        hand.set(1_010);
        {
            let mut g = tl.lock().unwrap();
            sweep_told(&mut g, &running(1, 1_000, "vim"), front);
            if front.is_none() {
                g.note_foreground_group(600);
            }
        }
        hand.set(2_000);
        sweep_told(
            &mut tl.lock().unwrap(),
            &finished(1, Some(3), 1_000, 1_900, "vim"),
            Some(400),
        );
        lines_of(&tl)
    };
    let kept_its_d = |got: &[String]| {
        got.iter()
            .any(|l| l.contains(" cmd out=fail dur_ms=900 code=3 ") && l.ends_with(&t(1_900)))
            && !got.iter().any(|l| l.contains(" cmd out=gone "))
    };

    let got = vim(Some(600));
    time_order_holds(&got).unwrap_or_else(|e| panic!("own group: {e}: {got:?}"));
    let _ = cmd_of(&got, 1);
    assert!(kept_its_d(&got), "own group: {got:?}");

    let got = vim(Some(400));
    time_order_holds(&got).unwrap_or_else(|e| panic!("shell's group: {e}: {got:?}"));
    let bash = opened_node(&got, " prog par=- prog=bash ");
    assert!(
        got.iter()
            .any(|l| l.contains(&format!(" cmd par={bash} blk=1 detail=vim ")))
            && kept_its_d(&got),
        "shell's group: {got:?}"
    );

    let got = vim(None);
    assert!(!kept_its_d(&got), "by time alone: {got:?}");
}

#[test]
fn a_nested_shells_name_read_with_its_first_command_hangs_above_it() {
    let t = |ms: u64| format!("t={}", WALL0 + ms);
    let nested = |front: Option<i32>| -> Vec<String> {
        let (tl, hand) = nested_shell();
        hand.set(500);
        sweep_told(
            &mut tl.lock().unwrap(),
            &finished(1, Some(0), 400, 410, "ls"),
            front,
        );
        hand.set(1_500);
        sweep_told(
            &mut tl.lock().unwrap(),
            &finished(2, Some(0), 1_000, 1_100, "make"),
            Some(400),
        );
        lines_of(&tl)
    };
    let got = nested(Some(400));
    time_order_holds(&got).unwrap_or_else(|e| panic!("nested: {e}: {got:?}"));
    let bash = opened_node(&got, " prog par=- prog=bash ");
    assert!(
        got.iter()
            .any(|l| l.contains(&format!(" open {bash} prog ")) && l.ends_with(&t(300))),
        "bash opens at its name: {got:?}"
    );
    assert!(
        got.iter()
            .any(|l| l.contains(&format!(" cmd par={bash} blk=1 detail=ls ")))
            && got
                .iter()
                .any(|l| l.contains(&format!(" cmd par={bash} blk=2 detail=make ")))
            && !got.iter().any(|l| l.contains(&format!(" close {bash} "))),
        "bash's commands hang under it, and it stays open: {got:?}"
    );

    for sleep_done in [false, true] {
        let (tl, hand) = opened();
        {
            let mut g = tl.lock().unwrap();
            hand.set(1_020);
            assert!(g.note_foreground_group(400));
            hand.set(1_025);
            assert!(g.set_program(400, Some("sleep".into())));
        }
        let (read, front) = if sleep_done {
            (finished(1, Some(0), 1_030, 1_040, "sleep"), 100)
        } else {
            (running(1, 1_030, "sleep"), 400)
        };
        hand.set(1_250);
        sweep_told(&mut tl.lock().unwrap(), &read, Some(front));
        let got = lines_of(&tl);
        time_order_holds(&got).unwrap_or_else(|e| panic!("sleep_done={sleep_done}: {e}: {got:?}"));
        let sleep = cmd_of(&got, 1);
        assert!(
            got.iter()
                .any(|l| l.contains(&format!(" prog par={sleep} prog=sleep "))
                    && l.ends_with(&t(1_030))),
            "sleep_done={sleep_done}: under its own command: {got:?}"
        );
    }

    let got = nested(None);
    let ls = cmd_of(&got, 1);
    assert!(
        got.iter()
            .any(|l| l.contains(&format!(" prog par={ls} prog=bash "))),
        "by time alone: {got:?}"
    );
}

#[test]
fn an_adoption_told_no_epoch_resets_a_predecessors_cursor() {
    for keeper in [true, false] {
        let tl = Mutex::new(SessionTimeline::for_test());
        let hand = hand_clock(&tl);
        hand.set(1_000);
        {
            let mut g = tl.lock().unwrap();
            let now = g.slog().now();
            g.slog_mut().adopt(LogAdopt::default(), keeper, None, now);
            g.slog_mut().open_session(None, None, now);
            g.note_root_group(100);
            for pgid in 200..210 {
                g.note_foreground_group(pgid);
                g.set_program(pgid, Some("less".into()));
            }
        }
        let g = tl.lock().unwrap();
        let log = g.slog().log();
        assert!(log.last_seq() > 5);
        assert_eq!(log.ep(), u32::try_from((WALL0 + 1_000) / 1000).unwrap());
        let reply = render_log(log, &query("since=5 ep=1"));
        let mut it = reply.lines().skip(1);
        assert!(
            it.next().is_some_and(|l| l.starts_with("gap why=reset ")),
            "keeper={keeper}: {reply}"
        );
        let why = if keeper { "crash" } else { "handoff" };
        assert!(
            it.next()
                .is_some_and(|l| l.starts_with(&format!("1 gap - gap why={why} "))),
            "{reply}"
        );
        let own = render_log(log, &query(&format!("since=5 ep={}", log.ep())));
        assert!(!own.contains("\ngap why="), "{own}");
    }
}

#[test]
fn a_failure_is_lifted_by_the_next_good_command_or_by_being_seen() {
    let need = |tl: &Mutex<SessionTimeline>| {
        tl.lock()
            .unwrap()
            .slog()
            .log()
            .state()
            .top_need()
            .map(|n| n.word)
    };
    let (tl, hand) = opened();
    blocks(&tl, &hand, 100, &finished(0, Some(1), 10, 20, "grep"));
    assert_eq!(need(&tl), Some("failure"), "NEGATIVE CONTROL: it stands");
    blocks(&tl, &hand, 200, &finished(1, Some(0), 110, 120, "true"));
    assert_eq!(need(&tl), None, "the next command ended ok");
    let brief = render_brief(
        tl.lock().unwrap().slog().log(),
        "s-x",
        Node::SESSION,
        None,
        600,
        WALL0 + 600_000,
    );
    assert!(!brief.contains("failure"), "{brief}");

    blocks(&tl, &hand, 300, &finished(2, Some(2), 210, 220, "make"));
    assert_eq!(need(&tl), Some("failure"));
    {
        let mut g = tl.lock().unwrap();
        hand.set(400);
        let now = g.slog().now();
        g.slog_mut().failure_seen(now);
    }
    assert_eq!(need(&tl), None, "seen");
    assert!(
        lines_of(&tl).iter().any(|l| l
            == &format!(
                "{} mark - seen by=human src=status conf=strong t={}",
                tl.lock().unwrap().slog().log().last_seq(),
                WALL0 + 400
            )),
        "{:?}",
        lines_of(&tl)
    );
}

#[test]
fn a_failure_the_person_sees_is_seen_in_the_log() {
    use std::time::{Duration, Instant};
    let need_and_seen = |app: &crate::App, session: u64| {
        let timeline = Arc::clone(&app.pool.get(session).expect("pooled").ctx.timeline);
        let g = timeline.lock().unwrap();
        let seen = lines(g.slog().log())
            .iter()
            .filter(|l| l.contains(" mark - seen by=human "))
            .count();
        (g.slog().log().state().top_need().map(|n| n.word), seen)
    };
    for looking in [true, false] {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        for ws in app.windows.values_mut() {
            ws.os_window_for_test = true;
            ws.focused = looking;
        }
        let front = app.front_terminal(wid).expect("a front terminal");
        let (session, term) = (front.session, Arc::clone(&front.term));
        crate::term_lock(&term).process(
            b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\make\r\n\x1b]133;C\x1b\\\
              \x1b]133;D;2\x1b\\\x1b]133;A\x1b\\\x1b]133;B\x1b\\",
        );
        let t0 = Instant::now();
        app.observe_session_statuses(t0);
        app.observe_session_statuses(t0 + Duration::from_secs(1));
        assert_eq!(
            app.session_status.status(session).map(|s| s.last_outcome),
            Some(crate::session_status::Outcome::Failure { exit_code: 2 }),
            "PRECONDITION: the sweep published the failure"
        );
        if looking {
            assert_eq!(
                need_and_seen(&app, session),
                (None, 1),
                "the sweep: seen as it published"
            );
            continue;
        }
        assert_eq!(
            need_and_seen(&app, session),
            (Some("failure"), 0),
            "NEGATIVE CONTROL: nobody looked"
        );
        for ws in app.windows.values_mut() {
            ws.focused = true;
        }
        app.note_session_seen(wid, session);
        assert_eq!(
            need_and_seen(&app, session),
            (None, 1),
            "note_session_seen: the person looked"
        );
    }
}

#[test]
fn a_carried_exit_the_person_saw_is_seen_in_the_log() {
    use std::time::{Duration, Instant};
    let need_and_seen = |app: &crate::App, session: u64| {
        let timeline = Arc::clone(&app.pool.get(session).expect("pooled").ctx.timeline);
        let g = timeline.lock().unwrap();
        let seen = lines(g.slog().log())
            .iter()
            .filter(|l| l.contains(" mark - seen by=human src=status "))
            .count();
        (g.slog().log().state().top_need().map(|n| n.word), seen)
    };
    for seen in [true, false] {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        for ws in app.windows.values_mut() {
            ws.os_window_for_test = true;
            ws.focused = false;
        }
        let front = app.front_terminal(wid).expect("a front terminal");
        let (session, term) = (front.session, Arc::clone(&front.term));
        crate::term_lock(&term).process(
            b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\make\r\n\x1b]133;C\x1b\\\
              \x1b]133;D;2\x1b\\\x1b]133;A\x1b\\\x1b]133;B\x1b\\",
        );
        let t0 = Instant::now();
        app.observe_session_statuses(t0);
        app.observe_session_statuses(t0 + Duration::from_secs(1));
        assert_eq!(
            need_and_seen(&app, session),
            (Some("failure"), 0),
            "PRECONDITION: the failed command's need, nobody looking"
        );
        let block = crate::session_status::completed_block(&crate::term_lock(&term));
        assert!(
            block.is_some_and(|b| b.exit_code == Some(2)),
            "PRECONDITION: the engine holds the failed command's block"
        );
        {
            let pooled = &mut app
                .pool
                .sessions
                .get_mut(&session)
                .expect("the pane")
                .session;
            let _ = pooled.child_exit.set(aterm_pty::ChildExit::Code(2));
            pooled
                .child_reaped
                .store(true, std::sync::atomic::Ordering::Release);
        }
        app.note_carried_exit(session, seen, block);
        if seen {
            assert_eq!(
                need_and_seen(&app, session),
                (None, 1),
                "the carried exit the person saw: `mark - seen` lifts the need"
            );
        } else {
            assert_eq!(
                need_and_seen(&app, session),
                (Some("failure"), 0),
                "NEGATIVE CONTROL: carried unseen, no `seen` and the need stands"
            );
        }
    }
}

#[test]
fn a_burst_of_blocks_is_read_oldest_first_and_none_is_lost() {
    use crate::session_status::{BLOCK_READ_MAX, block_read};
    let prompt = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    let commands = |term: &mut aterm_core::terminal::Terminal, n: usize| {
        for i in 0..n {
            term.process(prompt);
            term.process(format!("true {i}\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\").as_bytes());
        }
        term.process(prompt);
    };
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    commands(&mut term, 50);
    let (tl, hand) = opened();
    let first = block_read(&term, None, None, None);
    assert_eq!(first.facts.len(), BLOCK_READ_MAX);
    assert!(first.more, "the rest is owed");
    assert_eq!(first.facts[0].id, 0, "oldest first");
    blocks(&tl, &hand, 100, &first);
    let from = tl.lock().unwrap().slog().block_cursor();
    assert_eq!(from, Some(32));
    let rest = block_read(&term, from, None, None);
    assert_eq!(rest.facts.len(), 18);
    assert!(!rest.more);
    blocks(&tl, &hand, 200, &rest);
    let closes = lines_of(&tl)
        .iter()
        .filter(|l| l.contains(" close r") && l.contains(" cmd out=ok "))
        .count();
    assert_eq!(closes, 50, "every command is a run");
    assert_eq!(
        tl.lock().unwrap().slog().block_cursor(),
        Some(50),
        "the prompt in front (block 50) has run nothing yet: still owed"
    );

    term.process(b"true again\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
    let from = tl.lock().unwrap().slog().block_cursor();
    assert_eq!(from, Some(50));
    let again = block_read(&term, from, None, None);
    assert_eq!(again.facts.iter().map(|f| f.id).collect::<Vec<_>>(), [50]);
    blocks(&tl, &hand, 300, &again);
    term.process(prompt);

    let past = block_read(&term, None, Some(u64::MAX), None);
    assert!(past.facts.is_empty() && !past.more);
    assert_eq!(past.through, Some(50));

    let mut big = aterm_core::terminal::Terminal::new(24, 80);
    commands(&mut big, 1_005);
    let held = big.all_blocks().next().map(|b| b.id).expect("blocks");
    assert!(held > 0);
    let behind = block_read(&big, Some(0), None, None);
    assert_eq!(behind.dropped, held);
    let (tl, hand) = opened();
    blocks(&tl, &hand, 100, &behind);
    assert!(
        lines_of(&tl)
            .iter()
            .any(|l| l.starts_with("2 gap - gap why=evicted ")),
        "{:?}",
        lines_of(&tl)
    );
    assert_eq!(block_read(&big, Some(held), None, None).dropped, 0);
}

fn sweep_term(
    tl: &Mutex<SessionTimeline>,
    hand: &Hand,
    term: &aterm_core::terminal::Terminal,
    ms: u64,
) {
    let (from, since) = {
        let g = tl.lock().unwrap();
        (g.slog().block_cursor(), g.slog().opened_wall())
    };
    let read = crate::session_status::block_read(term, from, since, None);
    blocks(tl, hand, ms, &read);
}

fn evicted_gaps(tl: &Mutex<SessionTimeline>) -> usize {
    lines_of(tl)
        .iter()
        .filter(|l| l.contains(" gap - gap why=evicted "))
        .count()
}

#[test]
fn a_cleared_block_history_is_said_once_and_only_when_it_took_a_command() {
    let prompt = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    let cmd_opens = |tl: &Mutex<SessionTimeline>| {
        lines_of(tl)
            .iter()
            .filter(|l| l.contains(" cmd par=- blk="))
            .count()
    };
    for (case, gaps, first_held, cursor) in [
        ("clear", 0, 0, 2),
        ("clear seen running", 0, 0, 2),
        ("a clear past an unread command", 1, 2, 3),
        ("reset", 1, 2, 2),
        ("reset seen running", 0, 2, 2),
        ("reset seen running, a command after it", 0, 2, 3),
        ("cmd-k", 0, 0, 2),
        ("ris", 0, 2, 2),
    ] {
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        let (tl, hand) = opened();
        term.process(prompt);
        term.process(b"ls\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
        term.process(prompt);
        sweep_term(&tl, &hand, &term, 10);
        match case {
            "clear" => {
                term.process(b"clear\r\n\x1b]133;C\x1b\\\x1b[H\x1b[2J\x1b[3J\x1b]133;D;0\x1b\\")
            }
            "clear seen running" => {
                term.process(b"clear\r\n\x1b]133;C\x1b\\");
                sweep_term(&tl, &hand, &term, 50);
                term.process(b"\x1b[H\x1b[2J\x1b[3J\x1b]133;D;0\x1b\\");
            }
            "a clear past an unread command" => {
                term.process(b"true\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
                term.process(prompt);
                term.process(b"seq 30; clear\r\n\x1b]133;C\x1b\\");
                term.process(&b"n\r\n".repeat(30));
                term.process(b"\x1b[H\x1b[2J\x1b[3J\x1b]133;D;0\x1b\\");
            }
            "reset" => term.process(b"reset\r\n\x1b]133;C\x1b\\\x1bc\x1b]133;D;0\x1b\\"),
            "reset seen running" | "reset seen running, a command after it" => {
                term.process(b"reset\r\n\x1b]133;C\x1b\\");
                sweep_term(&tl, &hand, &term, 50);
                term.process(b"\x1bc\x1b]133;D;0\x1b\\");
                if case.ends_with("after it") {
                    term.process(prompt);
                    term.process(b"true\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
                }
            }
            "cmd-k" => term.clear_scrollback(),
            _ => term.process(b"\x1bc"),
        }
        term.process(prompt);
        assert_eq!(
            term.all_blocks().next().map(|b| b.id),
            Some(first_held),
            "{case}: PRECONDITION: what the clear left"
        );
        for i in 0..10 {
            sweep_term(&tl, &hand, &term, 100 + i * 250);
        }
        assert_eq!(evicted_gaps(&tl), gaps, "{case}: {:?}", lines_of(&tl));
        assert_eq!(
            tl.lock().unwrap().slog().block_cursor(),
            Some(cursor),
            "{case}: the next read starts at the first block still held"
        );
        let before = cmd_opens(&tl);
        term.process(b"true\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
        term.process(prompt);
        sweep_term(&tl, &hand, &term, 3_000);
        sweep_term(&tl, &hand, &term, 3_250);
        assert_eq!(
            cmd_opens(&tl),
            before + 1,
            "{case}: the new prompt's command is a run: {:?}",
            lines_of(&tl)
        );
        assert_eq!(evicted_gaps(&tl), gaps, "{case}: {:?}", lines_of(&tl));
    }

    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    term.process(prompt);
    term.process(b"ls\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
    term.process(prompt);
    term.process(b"\x1bc");
    term.process(prompt);
    let read = crate::session_status::block_read(&term, Some(1), None, None);
    assert_eq!(read.dropped, 1);
    let (tl, hand) = opened();
    blocks(&tl, &hand, 100, &read);
    assert_eq!(evicted_gaps(&tl), 1, "{:?}", lines_of(&tl));
    assert_eq!(tl.lock().unwrap().slog().block_cursor(), Some(2));
}

#[test]
fn an_idle_clear_after_a_seen_command_writes_no_gap() {
    let prompt = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    for clear in ["ed3", "cmd-k", "ris"] {
        for swept in [false, true] {
            for ran in [false, true] {
                let mut term = aterm_core::terminal::Terminal::new(24, 80);
                let (tl, hand) = opened();
                term.process(prompt);
                term.process(b"make\r\n\x1b]133;C\x1b\\");
                sweep_term(&tl, &hand, &term, 10);
                assert!(
                    lines_of(&tl)
                        .iter()
                        .any(|l| l.contains(" open r1 cmd par=- blk=0 ")),
                    "PRECONDITION: the sweep saw make running: {:?}",
                    lines_of(&tl)
                );
                term.process(b"\x1b]133;D;0\x1b\\");
                term.process(prompt);
                if ran {
                    term.process(b"ls\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
                    term.process(prompt);
                }
                match clear {
                    "ed3" => term.process(b"\x1b[3J"),
                    "cmd-k" => term.clear_scrollback(),
                    _ => term.process(b"\x1bc"),
                }
                if swept {
                    sweep_term(&tl, &hand, &term, 50);
                }
                term.process(prompt);
                for i in 0..4 {
                    sweep_term(&tl, &hand, &term, 100 + i * 250);
                }
                assert_eq!(
                    evicted_gaps(&tl),
                    usize::from(ran && clear == "ris"),
                    "{clear} swept={swept} ran={ran}: {:?}",
                    lines_of(&tl)
                );
            }
        }
    }
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    let (tl, hand) = opened();
    term.process(prompt);
    sweep_term(&tl, &hand, &term, 10);
    term.clear_scrollback();
    term.process(b"ls\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
    sweep_term(&tl, &hand, &term, 100);
    term.process(prompt);
    sweep_term(&tl, &hand, &term, 200);
    assert_eq!(evicted_gaps(&tl), 0, "{:?}", lines_of(&tl));
    assert!(
        lines_of(&tl)
            .iter()
            .any(|l| l.contains(" open r1 cmd par=- blk=0 ")),
        "ls ran on the prompt's block: {:?}",
        lines_of(&tl)
    );
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    let (tl, hand) = opened();
    let sweep = |term: &aterm_core::terminal::Terminal, front: i32, ms: u64| {
        hand.set(ms);
        let mut g = tl.lock().unwrap();
        let from = g.slog().block_cursor();
        let since = g.slog().opened_wall();
        let read = crate::session_status::block_read(term, from, since, None);
        let _ = g.sweep_slog(&read, false, 100, front);
    };
    {
        let mut g = tl.lock().unwrap();
        assert!(g.note_foreground_group(300));
        assert!(g.set_program(300, Some("ssh".into())));
    }
    sweep(&term, 300, 10);
    term.process(prompt);
    term.process(b"sleep 9\r\n\x1b]133;C\x1b\\");
    sweep(&term, 300, 20);
    sweep(&term, 100, 30);
    {
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().human_input(true, at);
    }
    term.process(prompt);
    term.process(b"\x1bc");
    term.process(prompt);
    sweep(&term, 100, 40);
    let got = lines_of(&tl);
    assert!(
        got.iter().any(|l| l.contains(" close r2 cmd out=gone ")),
        "PRECONDITION: ssh's close took sleep: {got:?}"
    );
    assert_eq!(evicted_gaps(&tl), 0, "{got:?}");
}

#[test]
fn the_cap_owes_a_look_only_for_a_block_that_may_yet_be_a_fact() {
    use crate::session_status::{BLOCK_READ_MAX, block_read};
    let prompt = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    for (n, running, more, gaps) in [
        (31, false, false, 0),
        (32, false, false, 0),
        (33, false, true, 1),
        (33, true, true, 1),
    ] {
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        for i in 0..n {
            term.process(prompt);
            term.process(format!("true {i}\r\n\x1b]133;C\x1b\\").as_bytes());
            if !running || i + 1 < n {
                term.process(b"\x1b]133;D;0\x1b\\");
            }
        }
        if !running {
            term.process(prompt);
        }
        let (tl, hand) = opened();
        let read = block_read(&term, None, None, None);
        assert_eq!(
            read.facts.len(),
            n.min(BLOCK_READ_MAX),
            "n={n} running={running}"
        );
        assert_eq!(read.more, more, "n={n} running={running}");
        blocks(&tl, &hand, 10, &read);
        term.process(b"\x1bc");
        term.process(prompt);
        sweep_term(&tl, &hand, &term, 100);
        sweep_term(&tl, &hand, &term, 200);
        assert_eq!(
            evicted_gaps(&tl),
            gaps,
            "n={n} running={running}: {:?}",
            lines_of(&tl)
        );
    }
}

struct Seeded(u64);

impl Seeded {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[derive(Clone, Copy)]
struct OracleCfg {
    seeds: (u64, u64),
    steps: usize,
    osc633: bool,
    nonce: bool,
    giant: bool,
    adopt: bool,
    mutate: fn(&mut BlockRead),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OraclePhase {
    None,
    Prompt,
    Typing,
    Exec,
    Done,
}

struct OracleCmd {
    blk: Option<u64>,
    done: bool,
    read: Option<usize>,
    gone_by: Option<usize>,
    epoch: usize,
}

#[derive(Default)]
struct OracleSeen {
    sweeps: usize,
    ms: u64,
    gaps_at: Vec<usize>,
    logged: HashSet<u64>,
    last_seq: u64,
    epoch: usize,
}

#[derive(Default, Debug)]
struct OracleTally {
    false_gaps: usize,
    missed_gaps: usize,
    unlogged: usize,
    examples: Vec<String>,
    losses: usize,
    refused_c: usize,
    gate_dropped: u64,
    giant_bursts: usize,
    store_evicted: usize,
    adoptions_new: usize,
    adoptions_old: usize,
}

const ORACLE_NONCE: [u8; 32] = [0xab; 32];

fn oracle_mark(body: &str, osc633: bool, signed: Option<bool>) -> Vec<u8> {
    let osc = if osc633 { "633" } else { "133" };
    let id = match signed {
        Some(ok) => {
            let byte = if ok { 0xab_u8 } else { 0xcd };
            format!(";id={}", format!("{byte:02x}").repeat(32))
        }
        None => String::new(),
    };
    format!("\x1b]{osc};{body}{id}\x1b\\").into_bytes()
}

fn oracle_sweep(
    tl: &Mutex<SessionTimeline>,
    hand: &Hand,
    term: &aterm_core::terminal::Terminal,
    mutate: fn(&mut BlockRead),
    seen: &mut OracleSeen,
    cmds: &mut [OracleCmd],
) {
    seen.ms += 250;
    let (from, since) = {
        let g = tl.lock().unwrap();
        (g.slog().block_cursor(), g.slog().opened_wall())
    };
    let mut read = crate::session_status::block_read(term, from, since, None);
    mutate(&mut read);
    blocks(tl, hand, seen.ms, &read);
    let g = tl.lock().unwrap();
    let after = seen.last_seq;
    for r in g.slog().log().window().filter(|r| r.seq > after) {
        seen.last_seq = r.seq;
        let l = aterm_slog::encode(r, Tier::Host).expect("a logged record fits");
        if l.contains(" gap - gap why=evicted ") {
            seen.gaps_at.push(seen.sweeps);
        }
        if l.contains(" cmd par=")
            && let Some(b) = l.split(' ').find_map(|w| w.strip_prefix("blk="))
        {
            seen.logged.insert(b.parse().expect("a block id"));
        }
    }
    let cursor = g.slog().block_cursor();
    for c in cmds.iter_mut() {
        if c.read.is_none()
            && c.gone_by.is_none()
            && let Some(b) = c.blk
            && (cursor.is_some_and(|at| at > b) || seen.logged.contains(&b))
        {
            c.read = Some(seen.epoch);
        }
    }
    seen.sweeps += 1;
}

fn oracle_gone(term: &aterm_core::terminal::Terminal, cmds: &mut [OracleCmd], sweeps: usize) {
    let held: HashSet<u64> = term.all_blocks().map(|b| b.id).collect();
    for c in cmds.iter_mut() {
        if c.gone_by.is_none() && c.blk.is_some_and(|b| !held.contains(&b)) {
            c.gone_by = Some(sweeps);
        }
    }
}

fn oracle_adopted(ms: u64) -> (Mutex<SessionTimeline>, Hand) {
    let tl = Mutex::new(SessionTimeline::for_test());
    let hand = hand_clock(&tl);
    hand.set(ms);
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().adopt(LogAdopt::default(), false, None, now);
        g.slog_mut().open_session(None, None, now);
        g.note_root_group(100);
    }
    (tl, hand)
}

fn lost_command_oracle(cfg: OracleCfg) -> OracleTally {
    use OraclePhase::{Done, Exec, None as Idle, Prompt, Typing};
    use aterm_core::terminal::{CheckpointMeta, ClockReading, Terminal};
    let mut tally = OracleTally::default();
    for seed in cfg.seeds.0..=cfg.seeds.1 {
        let mut rng = Seeded(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mono = ClockReading::now().monotonic;
        let feed = |term: &mut Terminal, bytes: &[u8], ms: u64| {
            term.process_at(
                bytes,
                ClockReading {
                    monotonic: mono,
                    wall_ms: Some(WALL0 + ms),
                },
            );
        };
        let mut term = Terminal::new(24, 80);
        let mut require = false;
        if cfg.nonce {
            term.authorize_shell_integration(ORACLE_NONCE);
        }
        let (mut tl, mut hand) = opened();
        let mut phase = Idle;
        let mut cmds: Vec<OracleCmd> = Vec::new();
        let mut seen = OracleSeen::default();
        let mut count = 0u64;
        let mut adopt_at: Vec<usize> = Vec::new();
        let mut blocks_made = 0usize;
        oracle_sweep(&tl, &hand, &term, cfg.mutate, &mut seen, &mut cmds);
        for _ in 0..cfg.steps {
            let ms = seen.ms;
            let osc633 = cfg.osc633 && rng.below(2) == 0;
            let signed = if cfg.nonce {
                match rng.below(10) {
                    0 => None,
                    1 => Some(false),
                    _ => Some(true),
                }
            } else {
                None
            };
            let ok = !require || signed == Some(true);
            match rng.below(100) {
                0..=14 => {
                    let mut b = oracle_mark("A", osc633, signed);
                    b.extend(b"$ ");
                    b.extend(oracle_mark("B", osc633, signed));
                    feed(&mut term, &b, ms);
                    if ok {
                        phase = Typing;
                        blocks_made += 1;
                    }
                }
                15..=19 => {
                    feed(&mut term, &oracle_mark("A", osc633, signed), ms);
                    if ok {
                        phase = Prompt;
                        blocks_made += 1;
                    }
                }
                20..=24 => {
                    feed(&mut term, &oracle_mark("B", osc633, signed), ms);
                    if ok && phase == Prompt {
                        phase = Typing;
                    }
                }
                25..=44 => {
                    let mut b = b"cmd\r\n".to_vec();
                    b.extend(oracle_mark("C", osc633, signed));
                    feed(&mut term, &b, ms);
                    if ok && phase == Typing {
                        let blk = term.current_block().map(|b| b.id);
                        count += 1;
                        cmds.push(OracleCmd {
                            blk,
                            done: false,
                            read: None,
                            gone_by: blk.is_none().then_some(seen.sweeps),
                            epoch: adopt_at.len(),
                        });
                        phase = Exec;
                    } else {
                        tally.refused_c += 1;
                    }
                }
                45..=56 => {
                    feed(&mut term, &oracle_mark("D;0", osc633, signed), ms);
                    if ok && phase == Exec {
                        phase = Done;
                        let held: HashSet<u64> = term.all_blocks().map(|b| b.id).collect();
                        if let Some(c) = cmds.last_mut() {
                            c.done = c.blk.is_some_and(|b| held.contains(&b));
                        }
                    }
                }
                57..=59 => {
                    if cfg.nonce {
                        require = !require;
                        term.set_require_shell_integration_nonce(require);
                    }
                }
                60..=63 => {
                    let giant = cfg.giant && rng.below(3) == 0;
                    if giant || blocks_made < 700 {
                        let n = if giant {
                            900 + rng.below(400)
                        } else {
                            20 + rng.below(40)
                        };
                        tally.giant_bursts += usize::from(giant);
                        let sig = cfg.nonce.then_some(true);
                        let mut prompt = oracle_mark("A", false, sig);
                        prompt.extend(b"$ ");
                        prompt.extend(oracle_mark("B", false, sig));
                        let mut one = prompt.clone();
                        one.extend(b"true\r\n");
                        one.extend(oracle_mark("C", false, sig));
                        let d = oracle_mark("D;0", false, sig);
                        for _ in 0..n {
                            feed(&mut term, &one, ms);
                            let blk = term.current_block().map(|b| b.id);
                            feed(&mut term, &d, ms);
                            count += 1;
                            cmds.push(OracleCmd {
                                blk,
                                done: true,
                                read: None,
                                gone_by: None,
                                epoch: adopt_at.len(),
                            });
                            blocks_made += 1;
                        }
                        feed(&mut term, &prompt, ms);
                        blocks_made += 1;
                        phase = Typing;
                        let held: HashSet<u64> = term.all_blocks().map(|b| b.id).collect();
                        tally.store_evicted += cmds
                            .iter()
                            .filter(|c| {
                                c.gone_by.is_none() && c.blk.is_some_and(|b| !held.contains(&b))
                            })
                            .count();
                    }
                }
                64..=67 => feed(&mut term, b"\x1b[3J", ms),
                68..=70 => term.clear_scrollback(),
                71..=73 => {
                    feed(&mut term, b"\x1bc", ms);
                    phase = Idle;
                }
                74 if cfg.adopt && adopt_at.len() < 3 => {
                    let cp = term.checkpoint();
                    let meta = CheckpointMeta::from_checkpoint(&cp);
                    let mut value = aterm_json::to_value(&meta).expect("the meta serializes");
                    let shell = value
                        .as_object_mut()
                        .and_then(|o| o.get_mut("shell"))
                        .and_then(|s| s.as_object_mut());
                    if rng.below(2) == 0
                        && let Some(shell) = shell
                    {
                        shell.remove("started_seq");
                        count = cp
                            .shell
                            .output_blocks
                            .iter()
                            .chain(&cp.shell.current_block)
                            .filter(|b| b.state >= 2)
                            .count() as u64;
                        tally.adoptions_old += 1;
                    } else {
                        tally.adoptions_new += 1;
                    }
                    let text = aterm_json::to_string(&value).expect("json");
                    let meta: CheckpointMeta =
                        aterm_json::from_str(&text).expect("this build reads it");
                    term = Terminal::from_checkpoint(
                        &meta.into_checkpoint(cp.grid.clone(), cp.alt_grid.clone()),
                    );
                    if cfg.nonce {
                        term.authorize_shell_integration(ORACLE_NONCE);
                        term.set_require_shell_integration_nonce(require);
                    }
                    assert_eq!(
                        term.started_command_seq(),
                        count,
                        "seed {seed}: the count crosses (an old predecessor's starts at its \
                         carried blocks' C's)"
                    );
                    (tl, hand) = oracle_adopted(seen.ms + 1);
                    seen.last_seq = 0;
                    adopt_at.push(seen.sweeps);
                    seen.epoch = adopt_at.len();
                    oracle_gone(&term, &mut cmds, seen.sweeps);
                    oracle_sweep(&tl, &hand, &term, cfg.mutate, &mut seen, &mut cmds);
                }
                _ => oracle_sweep(&tl, &hand, &term, cfg.mutate, &mut seen, &mut cmds),
            }
            oracle_gone(&term, &mut cmds, seen.sweeps);
        }
        for _ in 0..if cfg.giant { 90 } else { 40 } {
            oracle_sweep(&tl, &hand, &term, cfg.mutate, &mut seen, &mut cmds);
        }
        assert_eq!(
            term.started_command_seq(),
            count,
            "seed {seed}: PRECONDITION: the oracle knows every C the gate took"
        );
        tally.gate_dropped += term.shell_integration_dropped_count();
        let mut want: Vec<usize> = Vec::new();
        for c in &cmds {
            let next = adopt_at.get(c.epoch).copied().unwrap_or(usize::MAX);
            match (c.read, c.gone_by) {
                (Some(epoch), _) => {
                    if epoch == c.epoch
                        && c.done
                        && !c.blk.is_some_and(|b| seen.logged.contains(&b))
                    {
                        tally.unlogged += 1;
                    }
                }
                (None, Some(at)) if at >= next => {}
                (None, Some(at)) => want.push(at),
                (None, None) => panic!("seed {seed}: block {:?} is held but was never read", c.blk),
            }
        }
        tally.losses += want.len();
        want.sort_unstable();
        want.dedup();
        let got = &seen.gaps_at;
        assert!(
            got.windows(2).all(|w| w[0] < w[1]),
            "seed {seed}: one gap per sweep at most: {got:?}"
        );
        let false_gaps = got.iter().filter(|g| !want.contains(g)).count();
        let missed_gaps = want.iter().filter(|w| !got.contains(w)).count();
        tally.false_gaps += false_gaps;
        tally.missed_gaps += missed_gaps;
        if (false_gaps > 0 || missed_gaps > 0) && tally.examples.len() < 4 {
            tally.examples.push(format!(
                "seed {seed}: gaps at sweeps {got:?}, losses first said at {want:?}"
            ));
        }
    }
    tally
}

#[test]
fn the_block_read_says_every_lost_command_and_nothing_else() {
    let keep: fn(&mut BlockRead) = |_| {};
    let base = OracleCfg {
        seeds: (1, 100),
        steps: 60,
        osc633: false,
        nonce: false,
        giant: false,
        adopt: false,
        mutate: keep,
    };
    let mixes = [
        ("plain 133", base),
        (
            "633 and the nonce gate",
            OracleCfg {
                seeds: (1, 50),
                steps: 120,
                osc633: true,
                nonce: true,
                ..base
            },
        ),
        (
            "seamless updates",
            OracleCfg {
                seeds: (1, 50),
                steps: 120,
                osc633: true,
                nonce: true,
                adopt: true,
                ..base
            },
        ),
        (
            "the 1000-block store",
            OracleCfg {
                seeds: (1, 24),
                steps: 100,
                osc633: true,
                nonce: true,
                giant: true,
                adopt: true,
                ..base
            },
        ),
    ];
    for (name, cfg) in mixes {
        let t = lost_command_oracle(cfg);
        assert_eq!(
            (t.false_gaps, t.missed_gaps, t.unlogged),
            (0, 0, 0),
            "{name}: {t:?}"
        );
        assert!(
            t.losses > 0 && t.refused_c > 0,
            "{name}: covers losses and refused C's: {t:?}"
        );
        if cfg.nonce {
            assert!(t.gate_dropped > 0, "{name}: the gate refused marks: {t:?}");
        }
        if cfg.giant {
            assert!(
                t.giant_bursts > 0 && t.store_evicted > 0,
                "{name}: bursts past the store, which evicted unread commands: {t:?}"
            );
        }
        if cfg.adopt {
            assert!(
                t.adoptions_new > 0 && t.adoptions_old > 0,
                "{name}: both predecessors: {t:?}"
            );
        }
    }

    let no_count = lost_command_oracle(OracleCfg {
        seeds: (1, 25),
        mutate: |r| r.started = None,
        ..base
    });
    assert!(no_count.false_gaps > 0, "{no_count:?}");
    let blind = lost_command_oracle(OracleCfg {
        seeds: (1, 25),
        mutate: |r| {
            r.started = None;
            r.dropped = 0;
        },
        ..base
    });
    assert!(blind.missed_gaps > 0, "{blind:?}");
}

#[test]
fn log_json_is_one_object_per_line_its_gap_included() {
    let mut small = Log::with_limits(1, aterm_slog::Limits::new(3, 4096).unwrap());
    small
        .open_session(
            Kind::Spawn,
            Src::Aterm,
            Conf::Exact,
            Actor::Nobody,
            Fields::new(),
            at(0),
        )
        .unwrap();
    for i in 0..5 {
        small
            .mark(Mark::new(Kind::Human, Src::Input, Conf::Strong), at(i))
            .unwrap();
    }
    for (log, rest, why) in [
        (&small, "--json", "evicted"),
        (&sample_log(), "since=2 ep=2 --json", "reset"),
    ] {
        let reply = render_log(log, &query(rest));
        let mut it = reply.lines();
        let head = it.next().unwrap();
        let body: Vec<&str> = it.collect();
        assert!(
            head.starts_with(&format!("OK {} v=2 ", body.len())),
            "{reply}"
        );
        for line in &body {
            assert!(
                aterm_json::from_str::<aterm_json::Value>(line).is_ok(),
                "`{line}` in {reply}"
            );
        }
        let gap: aterm_json::Value = aterm_json::from_str(body[0]).unwrap();
        assert_eq!(gap.get("op").and_then(|v| v.as_str()), Some("gap"));
        assert_eq!(gap.get("why").and_then(|v| v.as_str()), Some(why));
        assert!(gap.get("lost").and_then(|v| v.as_u64()).is_some());
        assert!(gap.get("resync_to").and_then(|v| v.as_u64()).is_some());
    }
    let text = render_log(&small, &query(""));
    let line = text.lines().nth(1).unwrap();
    assert!(line.starts_with("gap why=evicted "));
    assert!(aterm_json::from_str::<aterm_json::Value>(line).is_err());
}

#[test]
fn log_takes_its_json_flag_and_its_node_anywhere() {
    let r1 = Node::parse("r1").unwrap();
    for rest in [
        "--json r1",
        "r1 --json",
        "json r1",
        "r1 json",
        "lvl=run r1 json",
    ] {
        let q = LogQuery::parse(rest).unwrap_or_else(|e| panic!("`{rest}`: {e}"));
        assert!(q.json && q.node == r1, "`{rest}`: {q:?}");
    }
    assert!(LogQuery::parse("json").is_ok_and(|q| q.json));
    for bad in ["r1 r2", "--json r1 t1"] {
        assert!(LogQuery::parse(bad).is_err(), "`{bad}`");
    }
    assert_eq!(BriefQuery::parse("budget=10 r1").unwrap().node, r1);
    assert!(BriefQuery::parse("budget=10 all").unwrap().all);
    assert!(BriefQuery::parse("all r1").is_err());
}

#[test]
fn brief_reports_only_the_gap_its_read_crossed() {
    let log = sample_log();
    let now = WALL0 + 60_000;
    let r1 = Node::parse("r1").unwrap();
    for (node, cursor) in [
        (r1, None),
        (Node::SESSION, Some(Cursor::new(3, 2))),
        (Node::SESSION, Some(Cursor::new(3, 4))),
    ] {
        let b = render_brief(&log, "s-x", node, cursor, 600, now);
        assert!(!b.contains("gap "), "{node} {cursor:?}: {b}");
    }
    for cursor in [Cursor::new(9, 2), Cursor::new(3, 999)] {
        let b = render_brief(&log, "s-x", Node::SESSION, Some(cursor), 600, now);
        assert!(
            b.contains("\ngap why=reset lost=0 resync_to=0\nmore="),
            "{cursor:?}: {b}"
        );
    }
    let mut small = Log::with_limits(1, aterm_slog::Limits::new(3, 4096).unwrap());
    small
        .open_session(
            Kind::Spawn,
            Src::Aterm,
            Conf::Exact,
            Actor::Nobody,
            Fields::new(),
            at(0),
        )
        .unwrap();
    for i in 0..5 {
        small
            .mark(Mark::new(Kind::Human, Src::Input, Conf::Strong), at(i))
            .unwrap();
    }
    let behind = render_brief(
        &small,
        "s-x",
        Node::SESSION,
        Some(Cursor::new(1, 1)),
        600,
        now,
    );
    assert!(
        behind.contains("\ngap why=evicted lost=4 resync_to=5\n"),
        "{behind}"
    );
    let whole = render_brief(&small, "s-x", Node::SESSION, None, 600, now);
    assert!(
        whole.contains("\ngap why=evicted lost=4 resync_to=0\n"),
        "{whole}"
    );

    let a = test_handle(1);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(a.clone());
    let on = crate::control::cmd_brief(&a.ctx, &store, "since=1 ep=1");
    assert!(on.starts_with("OK ") && !on.contains("\ngap "), "{on}");
    let stale = crate::control::cmd_brief(&a.ctx, &store, "since=1 ep=9");
    assert!(stale.contains("\ngap why=reset "), "{stale}");
}

#[test]
fn brief_all_keeps_within_its_budget() {
    let store = crate::session_store::new_store();
    for id in 1..=40 {
        store.write().unwrap().register(test_handle(id));
    }
    let any = store.read().unwrap().snapshot()[0].clone();
    let used_of = |reply: &str| -> usize {
        reply
            .lines()
            .next()
            .and_then(|h| h.split(' ').find_map(|w| w.strip_prefix("used=")))
            .and_then(|v| v.parse().ok())
            .expect("used=")
    };
    for (rest, budget) in [("all", 1000), ("all budget=100", 100)] {
        let reply = crate::control::cmd_brief(&any.ctx, &store, rest);
        let n: usize = reply
            .split(' ')
            .nth(1)
            .and_then(|v| v.parse().ok())
            .unwrap();
        assert_eq!(n, reply.lines().count() - 1, "{reply}");
        assert!(used_of(&reply) <= budget, "{rest}: {reply}");
        assert_eq!(used_of(&reply), aterm_slog::tokens(reply.len()), "{reply}");
        let kept = reply
            .lines()
            .filter(|l| l.starts_with("brief sid="))
            .count();
        assert!(kept >= 1, "{rest}: {reply}");
        if kept < 40 {
            assert!(
                reply.ends_with(&format!("\nmore=sessions:{}\n", 40 - kept)),
                "{reply}"
            );
        } else {
            assert!(!reply.contains("more=sessions"), "{reply}");
        }
        if budget == 100 {
            assert!(kept < 40, "forty sessions do not fit 100 tokens: {reply}");
        }
    }
    let none = crate::control::cmd_brief(&any.ctx, &store, "all budget=1");
    assert!(none.ends_with("\nmore=sessions:40\n"), "{none}");
    assert!(!none.contains("brief sid="), "{none}");

    let two = crate::session_store::new_store();
    for id in 1..=2 {
        two.write().unwrap().register(test_handle(id));
    }
    let any = two.read().unwrap().snapshot()[0].clone();
    let reply = crate::control::cmd_brief(&any.ctx, &two, "all");
    assert!(!reply.contains("more=sessions"), "{reply}");
    assert_eq!(
        reply
            .lines()
            .filter(|l| l.starts_with("brief sid="))
            .count(),
        2
    );
}

fn brief_all_parts() -> Vec<BriefPart> {
    (0..5u64)
        .map(|i| {
            let mut log = Log::new(1);
            log.open_session(
                Kind::Spawn,
                Src::Aterm,
                Conf::Exact,
                Actor::Nobody,
                Fields::new(),
                at(0),
            )
            .unwrap();
            for j in 0..3 * i + 1 {
                log.mark(
                    Mark::new(Kind::Human, Src::Input, Conf::Strong),
                    at(10 * j + i),
                )
                .unwrap();
            }
            if let Some(word) = [None, Some("escalated"), None, Some("failure"), None][i as usize] {
                log.mark(
                    Mark::new(Kind::Need, Src::Meta, Conf::Strong)
                        .fields(Fields::new().word(Key::Word, word).unwrap()),
                    at(500 + i),
                )
                .unwrap();
            }
            BriefPart {
                sid: format!("s-{i}"),
                copy: BriefCopy::copied_from(&log, Node::SESSION, None),
                now_wall: WALL0 + 60_000,
            }
        })
        .collect()
}

fn brief_all_two_pass(parts: &[BriefPart], budget: usize) -> String {
    let one = |p: &BriefPart, budget: usize| {
        let c = &p.copy;
        let recs = c.recs.iter().map(|r| &**r);
        aterm_slog::brief(&p.sid, recs, c.gap, &c.state, &c.links, budget, p.now_wall)
    };
    let n = parts.len();
    let fixed = aterm_slog::tokens(
        format!("OK {n:05} brief all sessions={n} v=2 budget={budget} used={budget:05}\n").len()
            + format!("more=sessions:{n}\n").len(),
    );
    let avail = budget.saturating_sub(fixed);
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (parts[i].rank(), i));
    let floor: Vec<usize> = parts.iter().map(|p| one(p, 0).used).collect();
    let kept = (1..=n)
        .rev()
        .find(|&k| order[..k].iter().all(|&i| floor[i] <= avail / k))
        .unwrap_or(0);
    let share = avail.checked_div(kept).unwrap_or(0);
    let mut keep = order[..kept].to_vec();
    keep.sort_unstable();
    let mut body = String::new();
    for &i in &keep {
        let text = one(&parts[i], share).text;
        let (ok, tail) = text.split_once(' ').unwrap();
        assert_eq!(ok, "OK", "{text}");
        body.push_str(tail.split_once(' ').unwrap().1);
        body.push('\n');
    }
    if kept < n {
        body.push_str(&format!("more=sessions:{}\n", n - kept));
    }
    let lines = body.lines().count();
    let mut used = 0;
    for _ in 0..4 {
        let head = format!("OK {lines} brief all sessions={n} v=2 budget={budget} used={used}\n");
        let next = aterm_slog::tokens(head.len() + body.len());
        if next == used {
            break;
        }
        used = next;
    }
    format!("OK {lines} brief all sessions={n} v=2 budget={budget} used={used}\n{body}")
}

#[test]
fn brief_all_matches_the_two_pass_reference_and_scans_once() {
    let parts = brief_all_parts();
    let scans = aterm_slog::brief::scans_on_this_thread;
    let mut kept_seen = std::collections::BTreeSet::new();
    for budget in (0..=600).step_by(3) {
        let before = scans();
        let reply = render_brief_all(&parts, budget);
        assert_eq!(scans() - before, 5, "budget {budget}: {reply}");
        let before = scans();
        let reference = brief_all_two_pass(&parts, budget);
        let reference_scans = scans() - before;
        assert_eq!(reply, reference, "budget {budget}");
        let kept = reply
            .lines()
            .filter(|l| l.starts_with("brief sid="))
            .count();
        assert_eq!(reference_scans, 5 + kept as u64, "budget {budget}: {reply}");
        kept_seen.insert(kept);
    }
    for kept in [0, 1, 3, 5] {
        assert!(
            kept_seen.contains(&kept),
            "no budget kept {kept}: {kept_seen:?}"
        );
    }
}

#[test]
fn log_and_brief_encode_off_the_timeline_lock() {
    let (a, b) = (test_handle(1), test_handle(2));
    let store = crate::session_store::new_store();
    store.write().unwrap().register(a.clone());
    store.write().unwrap().register(b.clone());
    let ctxs = [Arc::clone(&a.ctx), Arc::clone(&b.ctx)];
    let held: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let probe_held = Arc::clone(&held);
    ENCODE_PROBE.with(|p| {
        *p.borrow_mut() = Some(Box::new(move || {
            let locked = ctxs.iter().any(|ctx| ctx.timeline.try_lock().is_err());
            probe_held.lock().unwrap().push(locked);
        }));
    });
    let replies = [
        crate::control::cmd_log(&a.ctx, ""),
        crate::control::cmd_log_json(&a.ctx, ""),
        crate::control::cmd_brief(&a.ctx, &store, ""),
        crate::control::cmd_brief(&a.ctx, &store, "all"),
    ];
    let probes = std::mem::take(&mut *held.lock().unwrap());
    for reply in &replies {
        assert!(reply.starts_with("OK "), "{reply}");
    }
    assert!(
        probes.len() >= replies.len(),
        "every verb encoded: {probes:?}"
    );
    assert!(
        probes.iter().all(|locked| !locked),
        "an encoding ran under a timeline lock: {probes:?}"
    );

    let q = query("--json");
    let under = {
        let g = a.ctx.timeline.lock().unwrap();
        render_log_copy(&LogCopy::copied_from(g.slog().log(), &q), &q)
    };
    let probes = std::mem::take(&mut *held.lock().unwrap());
    ENCODE_PROBE.with(|p| *p.borrow_mut() = None);
    assert_eq!(probes, vec![true]);
    assert_eq!(
        under,
        render_log(a.ctx.timeline.lock().unwrap().slog().log(), &q)
    );
}

#[test]
fn log_copy_shares_records_with_the_ring() {
    let mut log = Log::with_limits(1, aterm_slog::Limits::new(4, 1 << 16).unwrap());
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Nobody,
        Fields::new(),
        at(0),
    )
    .unwrap();
    for i in 1..4 {
        log.mark(Mark::new(Kind::Human, Src::Input, Conf::Strong), at(i))
            .unwrap();
    }
    let q = query("");
    let copy = LogCopy::copied_from(&log, &q);
    let brief = BriefCopy::copied_from(&log, Node::SESSION, None);
    let reply = render_log_copy(&copy, &q);
    assert_eq!(reply, render_log(&log, &q));
    assert_eq!((copy.recs.len(), brief.recs.len()), (4, 4));
    for r in copy.recs.iter().chain(&brief.recs) {
        assert_eq!(Arc::strong_count(r), 3, "the ring and two copies: {r:?}");
    }

    for i in 4..8 {
        log.mark(Mark::new(Kind::Human, Src::Input, Conf::Strong), at(i))
            .unwrap();
    }
    let live: Vec<u64> = log
        .read_shared(Cursor::new(log.ep(), 0), usize::MAX)
        .recs
        .iter()
        .map(|r| r.seq)
        .collect();
    assert!(
        (2..=4).all(|seq| !live.contains(&seq)),
        "the copied marks are evicted: {live:?}"
    );
    drop(brief);
    for r in &copy.recs {
        let held = if live.contains(&r.seq) { 2 } else { 1 };
        assert_eq!(
            Arc::strong_count(r),
            held,
            "the copy, and a ring that keeps it: {r:?}"
        );
    }
    assert_eq!(render_log_copy(&copy, &q), reply);
}

fn successor_clock(timeline: &Mutex<SessionTimeline>, wall_ms: u64) -> Hand {
    let ms = Arc::new(AtomicU64::new(0));
    let read = Arc::clone(&ms);
    timeline
        .lock()
        .unwrap()
        .slog_mut()
        .set_clock(Clock::reading(move || {
            let m = read.load(Ordering::SeqCst);
            Stamp::new(WALL0 + wall_ms + m, 50 + m)
        }));
    Hand(ms)
}

/// A SEAMLESS UPDATE at the session log's seams, as the handoff runs them: the manifest's draw (`SessionHandoff::from_store`: `log_ids`, `log_seq`), the worker's take of the log (`slog_carry_source`, its readers fenced from then on) and its cut, the sidecar's JSON round trip, then the successor's adoption (`spawn::mark_handoff_gap`) and registration, its clock a new process's at `wall_ms`.
pub(super) fn hand_over(
    producer: &crate::session_store::SessionHandle,
    store: &SessionStore,
    wall_ms: u64,
    carry: bool,
) -> (crate::session_store::SessionHandle, SessionStore, Hand) {
    let manifest = SessionHandoff::from_store(store);
    let read = SessionHandoff::from_toml(&manifest.to_toml().unwrap()).unwrap();
    let rec = read
        .sessions
        .iter()
        .find(|r| r.local_id == producer.local_id)
        .expect("the record")
        .clone();
    let src = producer.ctx.timeline.lock().unwrap().slog_carry_source();
    let out = src
        .encode(aterm_slog::CarryBudget::SESSION)
        .expect("carried");
    let wire: Vec<String> =
        aterm_json::from_slice(&aterm_json::to_vec(&out.lines).unwrap()).unwrap();
    let successor = test_handle(producer.local_id);
    let hand = successor_clock(&successor.ctx.timeline, wall_ms);
    crate::spawn::mark_handoff_gap(
        &successor.ctx.timeline,
        &successor.ctx.cast,
        &successor.ctx.temporal,
        Some(7),
        rec.timeline_id,
        LogAdopt {
            ids: rec.log_ids.as_deref().and_then(LogCarry::parse),
            seq: rec.log_seq,
            lines: carry.then_some(wire),
            keeper_ep: None,
            mirror: None,
        },
        false,
    );
    let mut next = SessionStore::default();
    next.register(successor.clone());
    (successor, next, hand)
}

fn producer() -> (crate::session_store::SessionHandle, SessionStore, Hand) {
    let h = test_handle(1);
    let hand = hand_clock(&h.ctx.timeline);
    let mut store = SessionStore::default();
    store.register(h.clone());
    h.ctx.timeline.lock().unwrap().note_root_group(100);
    (h, store, hand)
}

fn publish(g: &mut SessionTimeline, word: &'static str, seq: u64) {
    g.publish_agent(
        word,
        None,
        None,
        Some(aterm_phase::Program::Claude),
        stamp(seq),
    );
}

fn successors_first_sweep(g: &mut SessionTimeline, program: &str) {
    let _ = g.sweep_slog(&BlockRead::default(), false, 100, 300);
    g.set_program(300, Some(program.into()));
    swept(g);
}

#[test]
fn an_agent_mid_turn_stays_in_its_turn_across_an_update() {
    for carry in [true, false] {
        let (p, store, hand) = producer();
        {
            let mut g = p.ctx.timeline.lock().unwrap();
            hand.set(10);
            assert!(g.note_foreground_group(300));
            assert!(g.set_program(300, Some("claude".into())));
            swept(&mut g);
            hand.set(1_000);
            publish(&mut g, "busy", 1);
        }
        let (s, _store2, hand2) = hand_over(&p, &store, 2_000, carry);
        {
            let mut g = s.ctx.timeline.lock().unwrap();
            successors_first_sweep(&mut g, "claude");
            hand2.set(100);
            publish(&mut g, "busy", 2);
            hand2.set(3_000);
            publish(&mut g, "idle", 3);
            hand2.set(4_100);
            let _ = g.slog_tick();
        }
        let got = lines_of(&s.ctx.timeline);
        let opens: Vec<&String> = got.iter().filter(|l| l.contains(" open t")).collect();
        let closes: Vec<&String> = got.iter().filter(|l| l.contains(" close t")).collect();
        if carry {
            assert!(
                got.iter()
                    .any(|l| l.contains(" mark - handoff carried=")
                        && l.contains(" shed=0 epoch=2 ")),
                "{got:?}"
            );
            assert!(!got.iter().any(|l| l.contains("why=handoff")), "{got:?}");
            assert_eq!(opens.len(), 1, "one turn: {got:?}");
            assert!(opens[0].contains(" open t1 turn "), "{got:?}");
            assert_eq!(closes.len(), 1, "{got:?}");
            assert!(
                closes[0].contains(" close t1 turn out=done ") && closes[0].contains(" dur=wall "),
                "the predecessor's turn closes here, on the wall clock: {got:?}"
            );
            assert!(
                !got.iter().any(|l| l.contains(" close r1 ")),
                "the agent's run went on: {got:?}"
            );
        } else {
            assert!(got[0].contains(" gap - gap why=handoff "), "{got:?}");
            assert!(
                opens.iter().all(|l| !l.contains(" open t1 ")),
                "NEGATIVE CONTROL: another turn, t1 never closing here: {got:?}"
            );
            assert!(!closes.iter().any(|l| l.contains(" close t1 ")), "{got:?}");
        }
    }
}

/// A MAIL ITS BRIDGE OFFERS AGAIN AFTER A SEAMLESS UPDATE IS MARKED ONCE
/// (6G-4's latch finding, found live by `lab seamless`). The predecessor
/// marks a task mail's delivery. The update carries the log but not the
/// inbox ring, so the successor's bridge offers the unread mail again as
/// the ring's refill. The successor takes the row back, and its log holds
/// one `mail dir=in` for it, whether the log was carried or started over.
/// So the fold counts one mail in, one open and one waiting; `brief` says
/// `mail in 1`; and the successor's next ctl turn links the mail.
#[test]
fn a_mail_its_bridge_offers_again_after_an_update_is_marked_once() {
    for carry in [true, false] {
        let (p, _, hand) = producer();
        let store = crate::session_store::new_store();
        store.write().unwrap().register(p.clone());
        {
            let mut g = p.ctx.timeline.lock().unwrap();
            hand.set(10);
            assert!(g.note_foreground_group(300));
            assert!(g.set_program(300, Some("claude".into())));
            swept(&mut g);
            publish(&mut g, "idle", 1);
        }
        hand.set(100);
        let mail = |sid: &str| format!("{sid} off=9 from=h-a kind=task trust=agent text=x");
        let first = crate::fabric::cmd_deliver(&store, &mail(p.sid.as_str()));
        assert!(first.starts_with("OK "), "{first}");
        let manifest = SessionHandoff::from_store(&store.read().unwrap());
        let rec = manifest
            .sessions
            .iter()
            .find(|r| r.local_id == p.local_id)
            .expect("the record")
            .clone();
        assert_eq!(rec.fabric.marked, [9], "the delivery's mark is carried");
        let (s, next, hand2) = hand_over(&p, &store.read().unwrap(), 2_000, carry);
        crate::spawn::seed_adopted_fabric(
            &s.ctx.fabric,
            &s.ctx.timeline,
            &rec.topics,
            &rec.hold_rows(),
            &rec.fabric,
        );
        let next: crate::session_store::Store = Arc::new(std::sync::RwLock::new(next));
        {
            let mut g = s.ctx.timeline.lock().unwrap();
            successors_first_sweep(&mut g, "claude");
        }
        hand2.set(100);
        let refill = crate::fabric::cmd_deliver(&next, &mail(s.sid.as_str()));
        assert!(refill.starts_with("OK "), "the refill is taken: {refill}");
        assert!(
            crate::fabric::cmd_inbox(&s.ctx, "--peek").contains(" off=9 "),
            "carry {carry}: the successor's inbox holds the mail again"
        );
        let brief = crate::control::cmd_brief(&s.ctx, &next, "");
        assert!(brief.contains("mail in 1 "), "carry {carry}:\n{brief}");
        let mut g = s.ctx.timeline.lock().unwrap();
        {
            let log = g.slog().log();
            let marks = log
                .window()
                .filter(|r| {
                    r.k == Kind::Mail
                        && r.f.get_word(Key::Dir) == Some("in")
                        && r.f.get_u(Key::Off) == Some(9)
                })
                .count();
            assert_eq!(marks, 1, "carry {carry}: {:?}", lines(log));
            let st = log.state();
            assert_eq!(
                (
                    st.totals.mail_in,
                    st.mail_open,
                    log.fold().mail_waiting().len()
                ),
                (1, 1, 1),
                "carry {carry}: one mail in, open and waiting"
            );
        }
        hand2.set(200);
        let now = g.slog().now();
        let _ = g.slog_mut().ctl_turn(Actor::Ctl, 1, None, now);
        hand2.set(300);
        publish(&mut g, "busy", 2);
        let log = g.slog().log();
        let turn = log
            .window()
            .rfind(|r| r.op == aterm_slog::Op::Open && r.k == Kind::Turn)
            .map(|r| r.node)
            .expect("the ctl turn");
        assert_eq!(
            log.fold().mail_link(turn).map(|l| l.off),
            Some(9),
            "carry {carry}: the turn links the one waiting mail"
        );
    }
}

#[test]
fn a_command_running_across_an_update_closes_with_its_code_and_wall_duration() {
    for carry in [true, false] {
        let (p, store, hand) = producer();
        blocks(&p.ctx.timeline, &hand, 200, &running(1, 100, "sleep"));
        let (s, _store2, hand2) = hand_over(&p, &store, 5_000, carry);
        hand2.set(25_100);
        {
            let mut g = s.ctx.timeline.lock().unwrap();
            swept_with(&mut g, &finished(1, Some(0), 100, 30_100, "sleep"));
        }
        let got = lines_of(&s.ctx.timeline);
        let cmds: Vec<&String> = got.iter().filter(|l| l.contains(" cmd ")).collect();
        if carry {
            assert_eq!(cmds.len(), 2, "the carried open and the close: {got:?}");
            assert!(cmds[0].contains(" open r1 cmd "), "{got:?}");
            assert!(
                cmds[1].contains(" close r1 cmd out=ok dur_ms=30000 dur=wall code=0 "),
                "its real code and duration: {got:?}"
            );
        } else {
            assert!(
                got.iter().any(|l| l.contains(" open r")
                    && l.contains(" cmd ")
                    && !l.contains(" open r1 ")),
                "NEGATIVE CONTROL: a run again, for the same block: {got:?}"
            );
        }
    }
}

#[test]
fn a_turn_resting_across_an_update_settles_after_it() {
    for carry in [true, false] {
        let (p, store, hand) = producer();
        {
            let mut g = p.ctx.timeline.lock().unwrap();
            assert!(g.note_foreground_group(300));
            assert!(g.set_program(300, Some("claude".into())));
            swept(&mut g);
            hand.set(1_000);
            publish(&mut g, "busy", 1);
            hand.set(4_000);
            publish(&mut g, "idle", 2);
            assert!(g.slog_wake().is_some(), "PRECONDITION: the settle is armed");
        }
        let (s, _store2, hand2) = hand_over(&p, &store, 4_300, carry);
        let mut g = s.ctx.timeline.lock().unwrap();
        successors_first_sweep(&mut g, "claude");
        if !carry {
            assert_eq!(g.slog_wake(), None, "NEGATIVE CONTROL: nothing to settle");
            continue;
        }
        let wake = g.slog_wake().expect("the carried settle's wake");
        assert_eq!(wake, 50 + 700, "due when it would have been: 700 ms on");
        hand2.set(699);
        assert!(g.slog_tick().is_some(), "not yet");
        hand2.set(700);
        assert_eq!(g.slog_tick(), None, "spent");
        drop(g);
        let got = lines_of(&s.ctx.timeline);
        let close = got
            .iter()
            .find(|l| l.contains(" close t1 turn "))
            .unwrap_or_else(|| panic!("the turn settled: {got:?}"));
        assert!(
            close.contains(" out=done ") && close.ends_with(&format!("t={}", WALL0 + 4_000)),
            "dated when the idle first held: {close}"
        );
    }
}

#[test]
fn the_successors_resolver_naming_the_same_program_keeps_its_run() {
    let (p, store, hand) = producer();
    {
        let mut g = p.ctx.timeline.lock().unwrap();
        hand.set(10);
        assert!(g.note_foreground_group(300));
        assert!(g.set_program(300, Some("node".into())));
        swept(&mut g);
        hand.set(20);
        publish(&mut g, "busy", 1);
        hand.set(30);
        assert!(g.set_program(300, Some("claude".into())));
        swept(&mut g);
    }
    let before = lines_of(&p.ctx.timeline);
    assert!(
        before.iter().any(|l| l.contains(" amend r2 agent ")),
        "{before:?}"
    );
    for (name, keeps) in [("claude", true), ("vim", false)] {
        let (s, _store2, _hand2) = hand_over(&p, &store, 1_000, true);
        {
            let mut g = s.ctx.timeline.lock().unwrap();
            successors_first_sweep(&mut g, name);
        }
        let got = lines_of(&s.ctx.timeline);
        let closed = got
            .iter()
            .any(|l| l.contains(" close r1 ") || l.contains(" close t1 "));
        if keeps {
            assert!(!closed, "the same name keeps the run and its turn: {got:?}");
        } else {
            assert!(
                got.iter()
                    .any(|l| l.contains(" close r1 prog out=superseded ")),
                "NEGATIVE CONTROL: another name supersedes: {got:?}"
            );
        }
        p.ctx.timeline.lock().unwrap().slog_mut().lift_read_fence();
    }
}

#[test]
fn a_long_verdict_detail_never_costs_the_carry() {
    let (p, store, hand) = producer();
    blocks(&p.ctx.timeline, &hand, 200, &running(1, 100, "sleep"));
    {
        let mut g = p.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        let long = format!("approve: {}", "\u{e9}x".repeat(550));
        g.slog_mut()
            .program_named(300, Some(&"c".repeat(400)), "prompt", Some(&long), now);
    }
    let src = p.ctx.timeline.lock().unwrap().slog_carry_source();
    let out = src
        .encode(aterm_slog::CarryBudget::SESSION)
        .expect("carried");
    let longest = out.lines.iter().map(String::len).max().unwrap_or(0);
    assert!(
        longest <= aterm_slog::CARRY_MAX_LINE,
        "a line of {longest} bytes"
    );
    assert!(
        out.lines.iter().any(|l| l.starts_with("host pending ")),
        "the waiting name crossed, cut: {:?}",
        out.lines
    );
    p.ctx.timeline.lock().unwrap().slog_mut().lift_read_fence();
    let (s, _store2, hand2) = hand_over(&p, &store, 5_000, true);
    let got = lines_of(&s.ctx.timeline);
    assert!(
        got.iter().any(|l| l.contains(" mark - handoff carried=")),
        "carried: {got:?}"
    );
    hand2.set(25_100);
    {
        let mut g = s.ctx.timeline.lock().unwrap();
        swept_with(&mut g, &finished(1, Some(0), 100, 30_100, "sleep"));
    }
    let got = lines_of(&s.ctx.timeline);
    assert!(
        got.iter()
            .any(|l| l.contains(" close r1 cmd out=ok dur_ms=30000 dur=wall code=0 ")),
        "the running command closed with its own code: {got:?}"
    );

    let mut lines = out.lines.clone();
    let i = lines
        .iter()
        .position(|l| l.starts_with("host "))
        .expect("a host line");
    lines[i] = format!(
        "host pending detail={}",
        "x".repeat(aterm_slog::CARRY_MAX_LINE)
    );
    let a = aterm_slog::CarryAdopt {
        ep: 2,
        next_seq: out.last_seq + 100,
        ids: aterm_slog::IdCounters {
            run: 9,
            turn: 9,
            step: 9,
        },
        limits: aterm_slog::Limits::SESSION,
        at: at(6_000),
    };
    assert!(aterm_slog::Log::from_carry(&out.lines, a).is_ok());
    assert!(aterm_slog::Log::from_carry(&lines, a).is_err());
}

#[test]
fn a_carried_block_cursor_meets_the_engine_the_successor_adopted() {
    use aterm_core::terminal::Terminal;
    let prompt = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    let ran = |term: &mut Terminal, cmd: &str, code: i32| {
        term.process(prompt);
        term.process(format!("{cmd}\r\n\x1b]133;C\x1b\\\x1b]133;D;{code}\x1b\\").as_bytes());
    };
    let sweep = |tl: &Mutex<SessionTimeline>, term: &Mutex<Terminal>| {
        let mut g = tl.lock().unwrap();
        let (from, since) = (g.slog().block_cursor(), g.slog().opened_wall());
        let read = crate::session_status::block_read(&crate::term_lock(term), from, since, None);
        let now = g.slog().now();
        g.slog_mut().shell_blocks(&read, None, now);
    };
    for (fit, blank) in [(true, true), (false, true), (true, false)] {
        let (p, store, hand) = producer();
        let old = Mutex::new(Terminal::new(24, 80));
        {
            let mut t = crate::term_lock(&old);
            for c in ["ls", "pwd", "date"] {
                ran(&mut t, c, 0);
            }
            t.process(prompt);
            t.process(b"sleep 30\r\n\x1b]133;C\x1b\\");
        }
        hand.set(100);
        sweep(&p.ctx.timeline, &old);
        assert_eq!(
            p.ctx.timeline.lock().unwrap().slog().block_cursor(),
            Some(3),
            "PRECONDITION: the sleep runs in block 3"
        );
        let (s, _store2, hand2) = hand_over(&p, &store, 5_000, true);
        let engine = Mutex::new(Terminal::new(24, 80));
        if !blank {
            let cp = crate::term_lock(&old).checkpoint();
            crate::term_lock(&engine).restore_checkpoint(&cp);
        }
        if fit {
            crate::spawn::fit_adopted_log(&engine, &s.ctx.timeline);
        }
        hand2.set(100);
        {
            let mut t = crate::term_lock(&engine);
            if !blank {
                // The sleep's own D, then the next prompt.
                t.process(b"\x1b]133;D;0\x1b\\");
            }
            ran(&mut t, "make", 2);
            ran(&mut t, "cargo", 1);
            t.process(prompt);
        }
        hand2.set(200);
        sweep(&s.ctx.timeline, &engine);
        let got = lines_of(&s.ctx.timeline);
        let fails = got
            .iter()
            .filter(|l| l.contains(" close r") && l.contains(" cmd out=fail "))
            .count();
        let gaps = got
            .iter()
            .filter(|l| l.contains(" gap why=evicted"))
            .count();
        match (fit, blank) {
            (true, true) => {
                assert!(
                    got.iter().any(|l| l.contains(" close r4 cmd out=unknown ")),
                    "the carried sleep's block is gone: {got:?}"
                );
                assert_eq!(gaps, 1, "{got:?}");
                assert_eq!(fails, 2, "both failures logged: {got:?}");
                assert!(
                    got.iter()
                        .any(|l| l.contains(" cmd out=fail ") && l.contains(" code=2 "))
                );
                assert_eq!(
                    s.ctx
                        .timeline
                        .lock()
                        .unwrap()
                        .slog()
                        .log()
                        .state()
                        .top_need()
                        .map(|n| n.word),
                    Some("failure"),
                    "{got:?}"
                );
            }
            (false, true) => {
                assert_eq!(fails, 0, "NEGATIVE CONTROL: unfitted, passed over: {got:?}");
                assert_eq!(gaps, 0, "and said nowhere: {got:?}");
            }
            _ => {
                assert!(
                    got.iter().any(|l| l.contains(" close r4 cmd out=ok ")),
                    "a cursor its engine holds is kept: {got:?}"
                );
                assert_eq!(gaps, 0, "nothing went: {got:?}");
                assert_eq!(fails, 2, "{got:?}");
            }
        }
    }
}

#[test]
fn a_host_state_that_does_not_reconcile_closes_what_nobody_holds() {
    for mangle in ["both", "cmd", "state"] {
        let (p, store, hand) = producer();
        let seen = BlockRead {
            through: Some(1),
            started: Some(1),
            started_held: 1,
            ..running(1, 100, "sleep")
        };
        blocks(&p.ctx.timeline, &hand, 200, &seen);
        let manifest = SessionHandoff::from_store(&store);
        let rec = manifest.sessions[0].clone();
        let src = p.ctx.timeline.lock().unwrap().slog_carry_source();
        let lines = src
            .encode(aterm_slog::CarryBudget::SESSION)
            .expect("carried")
            .lines;
        let mangled: Vec<String> = lines
            .iter()
            .map(|l| {
                if l.starts_with("host cmd ") && mangle != "state" {
                    "host cmd blk=zz".to_string()
                } else if l.starts_with("host state ") && mangle != "cmd" {
                    "host state v=1 opened=zz".to_string()
                } else {
                    l.clone()
                }
            })
            .collect();
        assert_ne!(
            mangled, lines,
            "{mangle}: PRECONDITION: the lines were there"
        );
        let successor = test_handle(1);
        let hand2 = successor_clock(&successor.ctx.timeline, 5_000);
        crate::spawn::mark_handoff_gap(
            &successor.ctx.timeline,
            &successor.ctx.cast,
            &successor.ctx.temporal,
            None,
            rec.timeline_id,
            LogAdopt {
                ids: rec.log_ids.as_deref().and_then(LogCarry::parse),
                seq: rec.log_seq,
                lines: Some(mangled),
                keeper_ep: None,
                mirror: None,
            },
            false,
        );
        let got = lines_of(&successor.ctx.timeline);
        assert_eq!(
            got.iter()
                .any(|l| l.contains(" close r1 cmd out=gone ") && l.contains(" by=aterm ")),
            mangle != "state",
            "{mangle}: the command nobody holds closes at the adoption: {got:?}"
        );
        let (read, logged): (BlockRead, &[&str]) = match mangle {
            "both" => (
                finished(2, Some(1), 5_200, 5_600, "make"),
                &[" cmd out=fail ", " code=1 "],
            ),
            "cmd" => (
                BlockRead {
                    from: Some(2),
                    through: Some(2),
                    dropped: 1,
                    started: Some(1),
                    ..BlockRead::default()
                },
                &[],
            ),
            _ => (
                BlockRead {
                    from: Some(1),
                    through: Some(1),
                    dropped: 1,
                    started: Some(1),
                    ..BlockRead::default()
                },
                &[" close r1 cmd out=unknown "],
            ),
        };
        hand2.set(1_000);
        {
            let mut g = successor.ctx.timeline.lock().unwrap();
            assert_eq!(g.slog().block_cursor(), read.from, "{mangle}: PRECONDITION");
            swept_with(&mut g, &read);
        }
        let got = lines_of(&successor.ctx.timeline);
        assert!(
            logged.is_empty() || got.iter().any(|l| logged.iter().all(|w| l.contains(w))),
            "{mangle}: {logged:?}: {got:?}"
        );
        assert_eq!(
            evicted_gaps(&successor.ctx.timeline),
            0,
            "{mangle}: {got:?}"
        );
    }
}

#[test]
fn a_reader_during_the_overlap_sees_only_what_was_carried() {
    let (p, _store, hand) = producer();
    blocks(&p.ctx.timeline, &hand, 200, &running(1, 100, "sleep"));
    let fenced = p.ctx.timeline.lock().unwrap().slog().log().last_seq();
    let _src = p.ctx.timeline.lock().unwrap().slog_carry_source();
    hand.set(300);
    blocks(
        &p.ctx.timeline,
        &hand,
        300,
        &finished(1, Some(0), 100, 290, "sleep"),
    );
    let reply = crate::control::cmd_log(&p.ctx, "");
    assert!(
        reply.starts_with(&format!("OK {fenced} v=2 as_of={fenced} ")),
        "{reply}"
    );
    assert!(
        !reply.contains(" close r1 "),
        "the overlap's close is not shown: {reply}"
    );
    let brief = crate::session_log::render_brief(
        p.ctx.timeline.lock().unwrap().slog().log(),
        "s-1",
        Node::SESSION,
        None,
        600,
        WALL0 + 400,
    );
    assert!(!brief.contains("sleep ok"), "{brief}");
    p.ctx.timeline.lock().unwrap().slog_mut().lift_read_fence();
    let reply = crate::control::cmd_log(&p.ctx, "");
    assert!(reply.contains(" close r1 cmd out=ok "), "lifted: {reply}");
}

fn recovered(ms: u64, tag_ep: Option<u32>) -> SessionLog {
    let mut log = SessionLog::for_test();
    let at = Stamp::new(WALL0 + ms, 5);
    log.adopt(
        LogAdopt {
            keeper_ep: tag_ep,
            mirror: None,
            ..LogAdopt::default()
        },
        true,
        None,
        at,
    );
    log
}

#[test]
fn two_crash_adoptions_in_one_second_take_growing_epochs() {
    let first = recovered(100, None);
    let e1 = first.log().ep();
    assert_eq!(
        u64::from(e1),
        (WALL0 + 100) / 1000,
        "the seconds, told nothing"
    );
    let second = recovered(400, Some(e1));
    assert_eq!(
        second.log().ep(),
        e1 + 1,
        "above the tag's, in the same second"
    );
    let third = recovered(700, Some(second.log().ep()));
    assert_eq!(third.log().ep(), e1 + 2);
    let later = recovered(10_000_000, Some(3));
    assert_eq!(u64::from(later.log().ep()), (WALL0 + 10_000_000) / 1000);
    assert_eq!(
        recovered(400, None).log().ep(),
        e1,
        "NEGATIVE CONTROL: no tag, the same second, the same epoch"
    );
    let s = crate::stub_session(5);
    {
        let mut g = s.ctx.timeline.lock().unwrap();
        g.slog_mut().adopt(
            LogAdopt {
                ids: LogCarry::parse("6 1 1 1"),
                seq: 9,
                lines: None,
                keeper_ep: None,
                mirror: None,
            },
            false,
            None,
            Stamp::new(WALL0, 5),
        );
    }
    assert_eq!(
        crate::keeper_link::registration_tag(&s, 1, None).slog_ep,
        Some(7)
    );
}

#[test]
fn a_crash_adoption_resets_a_cursor_of_the_same_second() {
    let first = recovered(100, None);
    let cursor = Cursor::new(first.log().ep(), 3);
    for (tag, reset) in [(Some(first.log().ep()), true), (None, false)] {
        let mut second = recovered(500, tag);
        let now = second.now();
        for _ in 0..6 {
            second.human_input(true, now);
        }
        let read = second.log().read(cursor, usize::MAX);
        if reset {
            assert_eq!(read.gap.map(|g| g.why), Some(aterm_slog::GapWhy::Reset));
        } else {
            assert_eq!(
                read.gap, None,
                "NEGATIVE CONTROL: read on into another process's records"
            );
        }
    }
}

fn app_with_logs(n: u64, marks: usize) -> crate::App {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    for _ in 0..n {
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    }
    for s in app.pool.iter() {
        let mut g = s.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, None, now);
        for _ in 0..marks {
            g.slog_mut().human_input(true, now);
        }
    }
    app
}

fn total_footprint(app: &crate::App) -> usize {
    app.pool
        .iter()
        .map(|s| s.ctx.timeline.lock().unwrap().slog().log().footprint())
        .sum()
}

#[test]
fn the_instance_sweep_brings_total_log_memory_under_its_cap() {
    let mut app = app_with_logs(6, 1500);
    let cap = total_footprint(&app) / 3;
    app.session_status.slog_memory.cap = cap;
    assert!(
        total_footprint(&app) > cap,
        "NEGATIVE CONTROL: unswept, over"
    );
    let t0 = std::time::Instant::now();
    for pass in 0..6 {
        app.sweep_session_log_memory(t0 + SLOG_SWEEP_EVERY * pass);
    }
    let total = total_footprint(&app);
    assert!(
        total <= cap + cap / 8,
        "under the cap (its slack): {total} > {cap}"
    );
    for s in app.pool.iter() {
        let g = s.ctx.timeline.lock().unwrap();
        let log = g.slog().log();
        assert!(log.lost() > 0, "every log gave some up");
        assert!(
            log.window()
                .next()
                .is_some_and(|r| r.op == aterm_slog::Op::Open),
            "the session's open stays"
        );
    }
    let lost: u64 = app
        .pool
        .iter()
        .map(|s| s.ctx.timeline.lock().unwrap().slog().log().lost())
        .sum();
    app.session_status.slog_memory.cap = 1;
    app.sweep_session_log_memory(t0 + SLOG_SWEEP_EVERY * 5 + std::time::Duration::from_millis(1));
    let after: u64 = app
        .pool
        .iter()
        .map(|s| s.ctx.timeline.lock().unwrap().slog().log().lost())
        .sum();
    assert_eq!(after, lost, "at most once a second");
}

#[test]
fn far_over_the_cap_one_sweep_pass_holds_the_desk() {
    let mut app = app_with_logs(23, 1500);
    let n = app.pool.iter().count();
    assert!(n > 2 * SLOG_SWEEP_MAX_SHRINK, "PRECONDITION: {n} logs");
    let cap = total_footprint(&app) / 3;
    app.session_status.slog_memory.cap = cap;
    let t0 = std::time::Instant::now();
    app.sweep_session_log_memory(t0);
    let lowered = |app: &crate::App| {
        app.session_status
            .slog_memory
            .known
            .values()
            .filter(|c| c.lowered)
            .count()
    };
    assert_eq!(lowered(&app), n, "every log, in one pass");
    let total = total_footprint(&app);
    assert!(total <= cap + cap / 8, "{total} > {cap}");
    app.session_status.slog_memory.cap = 1;
    app.sweep_session_log_memory(t0 + SLOG_SWEEP_EVERY);
    let floors: Vec<Option<usize>> = app
        .session_status
        .slog_memory
        .known
        .values()
        .map(|c| c.budget)
        .collect();
    let lost: u64 = app
        .pool
        .iter()
        .map(|s| s.ctx.timeline.lock().unwrap().slog().log().lost())
        .sum();
    app.sweep_session_log_memory(t0 + SLOG_SWEEP_EVERY * 2);
    let again: Vec<Option<usize>> = app
        .session_status
        .slog_memory
        .known
        .values()
        .map(|c| c.budget)
        .collect();
    assert_eq!(again, floors, "at its floor, nothing set again");
    let lost_again: u64 = app
        .pool
        .iter()
        .map(|s| s.ctx.timeline.lock().unwrap().slog().log().lost())
        .sum();
    assert_eq!(lost_again, lost);

    let mut app = app_with_logs(23, 1500);
    let total = total_footprint(&app);
    app.session_status.slog_memory.cap = total - total / 20;
    app.sweep_session_log_memory(std::time::Instant::now());
    assert_eq!(lowered(&app), SLOG_SWEEP_MAX_SHRINK);

    let mut app = app_with_logs(1, 1500);
    let mut root = None;
    {
        let mut timeline = app.pool.get(0).unwrap().ctx.timeline.lock().unwrap();
        let log = &mut timeline.slog_mut().log;
        let mut parent = Node::SESSION;
        for n in 0..60 {
            let fields = Fields::new()
                .tok(Key::Prog, &"x".repeat(40))
                .and_then(|f| f.tok(Key::Detail, &"x".repeat(40)))
                .and_then(|f| f.text(Key::Cwd, &"/very/long/path/".repeat(15)))
                .unwrap();
            parent = log
                .open(
                    aterm_slog::Open::new(Kind::Prog, Src::Status, Conf::Strong)
                        .under(parent)
                        .fields(fields),
                    at(n),
                )
                .expect("open path fits");
            if n == 0 {
                root = Some(parent);
            }
        }
    }
    let floors: Vec<(u64, usize)> = app
        .pool
        .iter()
        .map(|session| {
            let timeline = session.ctx.timeline.lock().unwrap();
            let log = timeline.slog().log();
            (
                session.id,
                log.kept_within(log.pinned_bytes() + aterm_slog::MAX_RECORD_BYTES)
                    .footprint(),
            )
        })
        .collect();
    let cap = floors.iter().map(|(_, floor)| floor).sum::<usize>() + 16 * 1024;
    assert!(
        floors.iter().find(|(id, _)| *id == 0).unwrap().1 > cap / 2,
        "the open path exceeds an equal share: cap={cap}, floors={floors:?}"
    );
    assert!(
        total_footprint(&app) > cap,
        "unswept detail exceeds the cap"
    );
    app.session_status.slog_memory.cap = cap;
    for pass in 0..12 {
        app.sweep_session_log_memory(t0 + SLOG_SWEEP_EVERY * pass);
    }
    let total = total_footprint(&app);
    assert!(
        total <= cap + cap / 8,
        "redistribute the open path's excess from evictable detail: {total} > {cap}; floors={floors:?}"
    );

    {
        let mut timeline = app.pool.get(0).unwrap().ctx.timeline.lock().unwrap();
        let log = &mut timeline.slog_mut().log;
        let old_budget = log.budget_bytes();
        log.close(
            root.unwrap(),
            aterm_slog::Close::new(
                aterm_slog::Out::Run(aterm_slog::RunOut::Ok),
                Src::Status,
                Conf::Strong,
            ),
            at(100),
        )
        .unwrap();
        assert!(log.pinned_bytes() + aterm_slog::MAX_RECORD_BYTES < old_budget);
    }
    let cap = app
        .pool
        .iter()
        .map(|session| {
            let timeline = session.ctx.timeline.lock().unwrap();
            let log = timeline.slog().log();
            log.kept_within(log.pinned_bytes() + aterm_slog::MAX_RECORD_BYTES)
                .footprint()
        })
        .sum::<usize>()
        + 16 * 1024;
    app.session_status.slog_memory.cap = cap;
    for pass in 12..24 {
        app.sweep_session_log_memory(t0 + SLOG_SWEEP_EVERY * pass);
    }
    assert!(
        total_footprint(&app) <= cap + cap / 8,
        "a former minimum is reassessed after its pinned state changes"
    );
}

#[test]
fn the_sweep_skips_a_timeline_it_cannot_take_and_never_waits() {
    let mut app = app_with_logs(3, 1500);
    app.session_status.slog_memory.cap = total_footprint(&app) / 3;
    let held_tl = Arc::clone(&app.pool.get(0).expect("session 0").ctx.timeline);
    let before = held_tl.lock().unwrap().slog().log().lost();
    let guard = held_tl.lock().unwrap();
    app.sweep_session_log_memory(std::time::Instant::now());
    drop(guard);
    assert_eq!(
        held_tl.lock().unwrap().slog().log().lost(),
        before,
        "the held one was not touched"
    );
    assert!(
        app.pool.iter().filter(|s| s.id != 0).any(|s| s
            .ctx
            .timeline
            .lock()
            .unwrap()
            .slog()
            .log()
            .lost()
            > 0),
        "the others were lowered"
    );
}

#[test]
fn budgets_rise_again_as_sessions_close() {
    let mut app = app_with_logs(4, 1500);
    app.session_status.slog_memory.cap = total_footprint(&app) / 3;
    let t0 = std::time::Instant::now();
    app.sweep_session_log_memory(t0);
    let id = app.pool.iter().map(|s| s.id).max().expect("a session");
    let budget = |app: &crate::App| {
        app.pool
            .get(id)
            .unwrap()
            .ctx
            .timeline
            .lock()
            .unwrap()
            .slog()
            .log()
            .budget_bytes()
    };
    assert!(
        budget(&app) < aterm_slog::MAX_BYTES,
        "PRECONDITION: lowered"
    );
    app.sweep_session_log_memory(t0 + SLOG_SWEEP_EVERY);
    assert!(
        budget(&app) < aterm_slog::MAX_BYTES,
        "CONTROL: still over, still lowered"
    );
    let others: Vec<u64> = app.pool.iter().map(|s| s.id).filter(|i| *i != id).collect();
    for other in others {
        app.pool.sessions.remove(&other);
    }
    app.sweep_session_log_memory(t0 + SLOG_SWEEP_EVERY * 2);
    assert_eq!(budget(&app), aterm_slog::MAX_BYTES, "its bound back");
}

#[test]
fn the_sweep_arms_no_deadline_on_an_idle_machine() {
    let mut app = app_with_logs(2, 50);
    let before: Vec<u64> = app
        .pool
        .iter()
        .map(|s| s.ctx.timeline.lock().unwrap().slog().log().lost())
        .collect();
    app.sweep_session_log_memory(std::time::Instant::now());
    assert!(app.next_session_log_wake().is_none(), "no deadline owed");
    let after: Vec<u64> = app
        .pool
        .iter()
        .map(|s| s.ctx.timeline.lock().unwrap().slog().log().lost())
        .collect();
    assert_eq!(after, before, "under the cap, nothing lowered");
}

fn closed_log(marks: usize) -> Log {
    let (tl, hand) = opened();
    let mut g = tl.lock().unwrap();
    for i in 0..marks {
        hand.set(10 + i as u64);
        let now = g.slog().now();
        g.slog_mut().human_input(true, now);
    }
    let now = g.slog().now();
    g.slog_mut().close_session(
        aterm_slog::SessionOut::UiClose,
        Actor::Human,
        "-",
        None,
        now,
    );
    g.slog().log().clone()
}

/// Close `n`'s slot (roster seq `n + 1`, local `n`) linked into `index`, then `log`'s part attached as `keep` says: how many skeletons the two freed.
fn link_closed(index: &mut ClosedLogs, log: &Log, n: u64, keep: ClosedKeep) -> usize {
    let sid = aterm_session::SessionId::new(format!("s-closed-{n}"));
    let linked = index.link(ClosedExit {
        seq: n + 1,
        t_ms: n,
        sid: sid.clone(),
        local_id: n,
        reason: ExitReason::UiClose,
        exit_code: None,
        actor: ExitActor::Human,
    });
    let kept = ClosedLog::kept_from(
        log,
        sid,
        Default::default(),
        0,
        aterm_session::LaunchNonce::generate(),
        keep,
    );
    linked.len() + index.keep_closed(n + 1, kept).len()
}

#[test]
fn the_index_keeps_512_summaries_and_the_newest_32_skeletons() {
    let log = closed_log(40);
    let mut index = ClosedLogs::default();
    let mut freed = 0;
    for n in 0..=CLOSED_INDEX_CAP as u64 {
        freed += link_closed(&mut index, &log, n, ClosedKeep::Skeleton);
    }
    let newest = CLOSED_INDEX_CAP as u64;
    assert_eq!(index.counts(), (CLOSED_INDEX_CAP, CLOSED_SKELETONS));
    assert!(index.by_local(0).is_none(), "the 513th-newest is gone");
    let held = |n: u64| {
        index
            .by_local(n)
            .expect("held")
            .source()
            .into_log()
            .expect("reads back")
    };
    assert_eq!(
        held(newest).len(),
        log.len(),
        "a small log's skeleton is whole"
    );
    assert_eq!(held(newest + 1 - CLOSED_SKELETONS as u64).len(), log.len());
    let summary = held(newest - CLOSED_SKELETONS as u64);
    assert_eq!(summary.len(), 2, "the 33rd-newest answers from its summary");
    assert_eq!(summary.lost(), log.lost() + log.len() as u64 - 2);
    assert_eq!(summary.last_seq(), log.last_seq());
    assert_eq!(
        index
            .by_sid(&aterm_session::SessionId::new("s-closed-1"))
            .map(|e| e.sid().as_str().to_string()),
        Some("s-closed-1".to_string())
    );
    assert_eq!(freed, CLOSED_INDEX_CAP + 1 - CLOSED_SKELETONS);
    assert_eq!(index.charge(), index.recharge());
}

#[test]
fn a_closed_summary_is_at_most_a_kilobyte_and_answers_log_and_brief() {
    let log = closed_log(3_000);
    assert!(log.lost() > 0, "PRECONDITION: a full ring");
    let mut index = ClosedLogs::default();
    for n in 0..=CLOSED_SKELETONS as u64 {
        let _ = link_closed(&mut index, &log, n, ClosedKeep::Skeleton);
    }
    let oldest = index.by_local(0).expect("held");
    let ClosedSource::Summary(text) = oldest.source() else {
        panic!("the 33rd-newest keeps only its summary");
    };
    assert!(
        text.len() <= aterm_slog::CLOSED_SUMMARY_BYTES,
        "{} B",
        text.len()
    );
    assert!(
        oldest.footprint() <= aterm_slog::CLOSED_SUMMARY_BYTES + 256,
        "{} B charged",
        oldest.footprint()
    );
    let back = oldest.source().into_log().expect("reads back");
    let q = query("");
    let (live, kept) = (render_log(&log, &q), render_log(&back, &q));
    let header = |r: &str| {
        r.lines()
            .next()
            .unwrap_or("")
            .split(' ')
            .skip(2)
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert_eq!(header(&kept), header(&live), "as_of= and ep=: {kept}");
    assert!(kept.contains(" close - "), "{kept}");
    let brief = render_brief(
        &back,
        "s-closed-0",
        Node::SESSION,
        None,
        600,
        WALL0 + 9_000_000,
    );
    assert!(
        brief.contains("ui-close") || brief.contains("closed"),
        "{brief}"
    );
    let newest = index.by_local(CLOSED_SKELETONS as u64).expect("held");
    assert!(matches!(newest.source(), ClosedSource::Skeleton(_)));
    assert!(newest.footprint() > 16 * aterm_slog::CLOSED_SUMMARY_BYTES);
}

#[test]
fn a_closed_session_is_linked_into_the_index_and_charged_to_the_cap() {
    let mut app = app_with_logs(1, 50);
    let local = app.next_session_id - 1;
    assert_eq!(app.closed_log_charge(), 0, "NEGATIVE CONTROL: none closed");
    assert!(
        app.store
            .read()
            .unwrap()
            .closed_logs()
            .by_local(local)
            .is_none()
    );
    app.retire_session_registration(local);
    let log = app
        .store
        .read()
        .unwrap()
        .closed_logs()
        .by_local(local)
        .expect("linked")
        .source()
        .into_log()
        .expect("reads back");
    assert_eq!(
        log.window().last().map(|r| r.op),
        Some(aterm_slog::Op::Close)
    );
    assert_eq!(log.window().last().map(|r| r.node), Some(Node::SESSION));
    let charge = app.closed_log_charge();
    assert!(charge >= log.footprint(), "{charge}");
    assert_eq!(charge, app.store.read().unwrap().closed_logs().charge());
}

#[test]
fn a_window_close_builds_only_what_the_closed_index_keeps() {
    use crate::session_log::ClosedKeep;
    let mut app = app_with_logs(39, 20);
    let wid = crate::WindowId(0);
    let other = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
    let last = app.next_session_id;
    app.push_stub_tab(other, crate::stub_session(last));
    {
        let ctx = std::sync::Arc::clone(&app.pool.get(last).expect("pooled").ctx);
        let mut g = ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, None, now);
    }
    let held = |h: &crate::session_store::SessionHandle| {
        !h.ctx.timeline.lock().unwrap().slog().log().is_empty()
    };
    assert!(
        app.store.read().unwrap().by_local(last).is_some_and(held),
        "PRECONDITION: the last window's tab is registered, its log open"
    );
    assert_eq!(app.window_terminal_sessions(wid).len(), 40);
    let built_before = crate::session_log::SKELETONS_BUILT.with(std::cell::Cell::get);
    let _ = app.close_window_logical(wid);
    let built_now = crate::session_log::SKELETONS_BUILT.with(std::cell::Cell::get);
    assert_eq!(
        built_now - built_before,
        CLOSED_SKELETONS,
        "32 built, not 40"
    );
    let counts = app.store.read().unwrap().closed_logs().counts();
    assert_eq!(counts, (40, CLOSED_SKELETONS), "{counts:?}");
    let built = (0..40u64)
        .filter(|id| {
            app.store
                .read()
                .unwrap()
                .closed_logs()
                .by_local(*id)
                .is_some_and(|e| matches!(e.source(), ClosedSource::Skeleton(_)))
        })
        .count();
    assert_eq!(built, CLOSED_SKELETONS, "the newest 32 hold theirs");
    let _ = app.close_window_logical(other);
    assert!(
        app.store.read().unwrap().by_local(last).is_none(),
        "the close retired it"
    );
    assert_eq!(
        crate::session_log::SKELETONS_BUILT.with(std::cell::Cell::get),
        built_now
    );
    assert!(
        app.store
            .read()
            .unwrap()
            .closed_logs()
            .by_local(last)
            .is_some_and(|e| matches!(e.source(), ClosedSource::Summary(_))),
        "the app's last close links its summary"
    );
    // Its slot is the newest, so the 33rd-newest's skeleton goes.
    assert_eq!(
        app.store.read().unwrap().closed_logs().counts(),
        (41, CLOSED_SKELETONS - 1),
        "the app's last close builds no skeleton"
    );
    for of in [1, 2, CLOSED_SKELETONS] {
        assert!((0..of).all(|i| ClosedKeep::in_batch(i, of) == ClosedKeep::Skeleton));
    }
    assert_eq!(ClosedKeep::in_batch(0, 40), ClosedKeep::Summary);
    assert_eq!(ClosedKeep::in_batch(8, 40), ClosedKeep::Skeleton);
}

#[test]
fn a_window_close_counts_its_closes_not_its_views() {
    let skeletons = |app: &crate::App| {
        (0..app.next_session_id)
            .filter(|id| {
                app.store
                    .read()
                    .unwrap()
                    .closed_logs()
                    .by_local(*id)
                    .is_some_and(|e| matches!(e.source(), ClosedSource::Skeleton(_)))
            })
            .count()
    };
    let mut app = app_with_logs(32, 20);
    let wid = crate::WindowId(0);
    let shared = app.next_session_id - 1;
    app.frontmost_window = Some(wid);
    let viewer = app
        .open_active_session_in_new_window_logical()
        .expect("the newest tab shown in a second window");
    assert_eq!(app.pool.views(shared), Some(2));
    assert_eq!(app.window_terminal_sessions(wid).len(), 33);
    let _ = app.close_window_logical(wid);
    assert_eq!(
        app.store.read().unwrap().closed_logs().counts(),
        (32, CLOSED_SKELETONS),
        "32 closes, each with its skeleton"
    );
    assert_eq!(skeletons(&app), CLOSED_SKELETONS);
    assert!(app.windows.contains_key(&viewer));
    assert!(app.store.read().unwrap().by_local(shared).is_some());
    assert!(
        app.store
            .read()
            .unwrap()
            .closed_logs()
            .by_local(shared)
            .is_none()
    );

    let mut app = app_with_logs(31, 20);
    let _living = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
    let twice = app.next_session_id - 2;
    app.frontmost_window = Some(wid);
    app.open_active_session_in_new_window_logical()
        .expect("shown in a second window");
    app.migrate_active_tab_to_next_window()
        .expect("moved back into the first");
    assert_eq!(app.pool.views(twice), Some(2));
    let views = app.window_terminal_sessions(wid);
    assert_eq!(views.len(), 33);
    assert_eq!(views.iter().filter(|id| **id == twice).count(), 2);
    let _ = app.close_window_logical(wid);
    assert_eq!(
        app.store.read().unwrap().closed_logs().counts(),
        (32, CLOSED_SKELETONS),
        "32 closes, each with its skeleton"
    );
    assert_eq!(skeletons(&app), CLOSED_SKELETONS);
}

#[test]
fn a_close_batchs_plan_ranks_the_detaches_that_close() {
    use crate::session_log::ClosedKeep;
    let mut app = app_with_logs(3, 0);
    app.frontmost_window = Some(crate::WindowId(0));
    app.pool.attach(1);
    app.pool.attach(2);
    let closes = app.pool.drops_of(&[0, 1, 2, 3, 2, 9]);
    assert_eq!(closes, vec![true, false, false, true, true, false]);
    assert_eq!((app.pool.views(1), app.pool.views(2)), (Some(2), Some(2)));
    let skel = Some(ClosedKeep::Skeleton);
    assert_eq!(
        ClosedKeep::plan(&closes),
        vec![skel, None, None, skel, skel, None]
    );
    let mut closes = vec![true; 32];
    closes.push(false);
    let plan = ClosedKeep::plan(&closes);
    assert_eq!(plan.iter().filter(|k| **k == skel).count(), 32);
    assert_eq!(plan[32], None);
    let plan = ClosedKeep::plan(&[true; 34]);
    let summary = Some(ClosedKeep::Summary);
    assert_eq!(&plan[..3], &[summary, summary, skel]);
    assert!(
        plan.iter()
            .enumerate()
            .all(|(i, k)| *k == Some(ClosedKeep::in_batch(i, 34)))
    );
}

#[test]
fn the_typed_line_carries_the_inputs_author_and_cause() {
    let (p, store, hand) = producer();
    hand.set(1_000);
    {
        let mut g = p.ctx.timeline.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().ctl_turn(Actor::Ctl, 4, ShortId::new("12"), at);
        g.slog_mut().ctl_input(
            Actor::Harness,
            super::InputVerb::Send,
            ShortId::new("@5"),
            super::InputAct::default(),
            at,
        );
        let host = g.slog().host_lines(at);
        let typed = host
            .iter()
            .find(|l| l.starts_with("typed "))
            .expect("a typed line");
        assert!(
            typed.contains(" input_by=harness input_cause=@5") && typed.contains(" ctl_cause=12"),
            "{typed}"
        );
    }
    let (s, _store2, _hand2) = hand_over(&p, &store, 1_500, true);
    let g = s.ctx.timeline.lock().unwrap();
    let typed = g.slog().typed;
    assert_eq!(
        typed
            .input
            .map(|(_, by, cause, _)| (by, cause.map(|c| c.to_string()))),
        Some((Actor::Harness, Some("@5".to_string())))
    );
    assert_eq!(
        typed
            .ctl
            .map(|(_, by, ledger, cause, _)| (by, ledger, cause.map(|c| c.to_string()))),
        Some((Actor::Ctl, 4, Some("12".to_string())))
    );
}

#[test]
fn a_turn_a_delivery_route_refuses_keeps_attribution_without_an_input_mark() {
    let (h, hand) = claude_handle();
    let store = crate::session_store::new_store();
    let calls = std::cell::Cell::new(0usize);
    hand.set(10);
    assert!(
        lines_of(&h.ctx.timeline)
            .iter()
            .all(|line| !line.contains(" mark - input ")),
        "PRECONDITION: the log starts without an input mark"
    );
    let paste = |text: &str| {
        calls.set(calls.get() + 1);
        assert_eq!(text, "hello");
        let id = {
            let lease = h.ctx.turn_lease.lock().unwrap();
            let Some(crate::Lease::Turn { id, .. }) = lease.as_ref() else {
                panic!("PRECONDITION: the actual turn owns its input lease");
            };
            *id
        };
        h.ctx.timeline.lock().unwrap().publish_agent(
            "busy",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(1),
        );
        let attributed = lines_of(&h.ctx.timeline);
        assert!(
            attributed
                .iter()
                .any(|line| line.contains(&format!(" open t1 turn par=r1 ledger={id} by=ctl "))),
            "PRECONDITION: attribution precedes even a refusing delivery callback: {attributed:?}"
        );
        false
    };
    let reply = crate::control::cmd_turn(
        &h.term,
        &store,
        0,
        "idle=1 timeout=60000 submit=none hello",
        &h.ctx,
        &crate::control::TurnIo {
            paste: &paste,
            ..crate::control::TurnIo::paste_only()
        },
    );
    assert_eq!(reply, "ERR paste delivery failed\n");
    assert_eq!(calls.get(), 1, "the real refusing route was called once");
    assert!(h.ctx.turn_lease.lock().unwrap().is_none());
    let marks: Vec<_> = lines_of(&h.ctx.timeline)
        .into_iter()
        .filter(|line| line.contains(" mark - input "))
        .collect();
    assert!(
        marks.is_empty(),
        "CTL_TURN_MARKED_UNACCEPTED_INPUT: a callback that accepted no bytes left {marks:?}"
    );
}

#[test]
fn the_always_present_host_lines_fit_the_carry_at_their_worst() {
    use crate::session_timeline::{MetaField, SUPERVISOR_FIELD};
    use aterm_slog::{CARRY_MAX_HOST_BYTES, CARRY_MAX_LINE, ShortId, TurnTracker};
    let (tl, _hand) = opened();
    let mut g = tl.lock().unwrap();
    let s = g.slog_mut();
    let max = u64::MAX;
    let now = Stamp::new(max, max);
    let run = Node::parse("r4294967295").expect("a node");
    let id40 = "a".repeat(ShortId::MAX);
    let sid = ShortId::new(&id40).expect("an id");
    s.origin = Kind::Restore;
    s.opened_wall = Some(max);
    s.root_pgid = i32::MIN;
    s.last_blk = Some(max);
    s.c_snap = Some(CSnap {
        base: max,
        cursor: Some(max),
    });
    s.cmd_end = Some(now);
    s.arrivals = max;
    let fp = "f".repeat(16);
    let meta: Vec<String> = MetaField::ALL
        .iter()
        .map(|f| f.wire_name())
        .chain([SUPERVISOR_FIELD])
        .map(|f| format!("{f}.{fp}"))
        .collect();
    let tokens: HashMap<&str, String> = [
        ("obs_int", "degraded".to_string()),
        ("obs_ctx", "10".to_string()),
        ("obs_goal", "1".to_string()),
        ("obs_prog", "indeterminate.10".to_string()),
        ("cwd_fp", fp.clone()),
        ("title_fp", fp.clone()),
        ("obs_esc", "1".to_string()),
        ("meta_fp", meta.join(",")),
    ]
    .into_iter()
    .collect();
    s.observed.read_carry(|k| tokens.get(k).cloned());
    let said = s.observed.carry_tokens();
    for (k, v) in &tokens {
        assert!(said.contains(&format!(" {k}={v}")), "{k} lost: {said}");
    }
    s.cmd = Some(CmdRun {
        blk: max,
        node: run,
        wall_from: Some(max),
        child_end: Some(now),
    });
    s.fg = Some(FgRun {
        pgid: i32::MIN,
        node: run,
        name_fp: max,
    });
    let detail = "%E9".repeat(16);
    let worst = format!(
        "tracker run=r4294967295 turn=t4294967295 word=wall:usage-session interrupted=1 \
         step=p4294967295 step_under=1 step_what=wall step_wall=usage-session step_detail={detail} \
         settle=wall:usage-session settle_wall={max} \
         ledger={max} ledger_by={id40} ledger_wall={max} ledger_cause={id40} \
         input_by={id40} input_wall={max} input_cause={id40} \
         tx_fp={max:016x} tx_off={max} tx_id={max:x}:{max:x} tx_v={id40} tx_vt=t4294967295 \
         tx_ve={max} tx_vp={max} tx_ref=usage-session tx_st=partial tx_ds=1 tx_dr={max} tx_du=1"
    );
    let who = [
        aterm_phase::Program::Claude,
        aterm_phase::Program::Codex,
        aterm_phase::Program::Agy,
        aterm_phase::Program::Muse,
        aterm_phase::Program::Gemini,
        aterm_phase::Program::Generic,
    ]
    .into_iter()
    .max_by_key(|p| p.name().len())
    .expect("a program");
    s.agent = Some(AgentRun {
        pgid: i32::MIN,
        node: run,
        who,
        by_screen: true,
        tracker: TurnTracker::from_carry_line(&worst, now).expect("the worst line reads"),
    });
    s.typed = Typed {
        input: Some((max, Actor::sid(sid), Some(sid), now)),
        ctl: Some((max, Actor::sid(sid), max, Some(sid), now)),
    };
    let always = |s: &SessionLog| {
        let mut lines = s.host_lines(now);
        if lines.first().map(String::as_str) != Some(carry::JOURNAL_HOST_LINE) {
            lines.insert(0, carry::JOURNAL_HOST_LINE.to_string());
        }
        lines
    };
    let lines = always(s);
    let tags = ["journal", "state", "cmd", "fg", "agent", "tracker", "typed"];
    assert_eq!(
        lines
            .iter()
            .map(|l| l.split(' ').next().unwrap_or(""))
            .collect::<Vec<_>>(),
        tags,
        "{lines:#?}"
    );
    for l in &lines {
        assert!(
            "host ".len() + l.len() <= CARRY_MAX_LINE - 5,
            "{} bytes: {l}",
            l.len()
        );
    }
    let bytes: usize = lines.iter().map(|l| l.len() + 1).sum();
    assert!(bytes <= CARRY_MAX_HOST_BYTES, "{bytes} bytes: {lines:#?}");
    let wide = |n: usize| "%".repeat(n);
    s.pending = (0..PENDING_NAMES_MAX)
        .map(|_| PendingName {
            pgid: i32::MIN,
            name: Some(wide(carry::PENDING_NAME_MAX)),
            word: wide(carry::PENDING_WORD_MAX),
            detail: Some(wide(carry::PENDING_DETAIL_MAX)),
            at: now,
            seq: max,
        })
        .collect();
    let host = always(s);
    let out = s
        .log
        .carry(aterm_slog::CarryBudget::SESSION, &host)
        .expect("carries");
    let crossed: Vec<&str> = out
        .lines
        .iter()
        .filter_map(|l| l.strip_prefix("host "))
        .collect();
    for tag in tags {
        assert!(
            crossed.iter().any(|l| l.split(' ').next() == Some(tag)),
            "{tag} dropped beside the waiting names: {crossed:#?}"
        );
    }
    let pending = crossed.iter().filter(|l| l.starts_with("pending ")).count();
    assert!(
        pending >= 1,
        "no waiting name crosses beside {bytes} bytes of always-present lines"
    );
    eprintln!(
        "M4: {bytes} B of always-present host lines at their worst; {pending} of \
         {PENDING_NAMES_MAX} widest pending lines fit beside them"
    );
    assert_eq!(
        host.iter()
            .take(tags.len())
            .map(|l| l.split(' ').next().unwrap_or(""))
            .collect::<Vec<_>>(),
        tags,
        "the waiting names come after every always-present line: {host:#?}"
    );
}

fn process(leaf_ep: Option<u32>) -> Arc<Epochs> {
    let epochs = Arc::new(Epochs::new());
    epochs.note_leaves(&crate::restore::RestoreManifest::one_terminal_for_test(
        leaf_ep,
    ));
    epochs
}

fn spawned_in(epochs: &Arc<Epochs>, ms: u64, marks: u64) -> SessionLog {
    let mut log = SessionLog::starting(Arc::clone(epochs), at(ms));
    log.set_clock(Clock::reading(move || at(ms)));
    log.open_session(None, None, at(ms));
    for _ in 0..marks {
        log.human_input(true, at(ms));
    }
    log
}

fn adopted_in(epochs: &Arc<Epochs>, ms: u64) -> SessionLog {
    let mut log = SessionLog::starting(Arc::clone(epochs), at(ms));
    log.set_clock(Clock::reading(move || at(ms)));
    log.adopt(LogAdopt::default(), false, None, at(ms));
    log.open_session(None, None, at(ms));
    log
}

#[test]
fn a_predecessors_cursor_resets_on_a_relaunch_under_the_same_sid() {
    let first = spawned_in(&process(None), 0, 6);
    let cursor = Cursor::new(first.log().ep(), first.log().last_seq());
    let relaunch = spawned_in(&process(None), 1_000, 10);
    assert!(relaunch.log().last_seq() > cursor.seq, "PRECONDITION");
    let read = relaunch.log().read(cursor, usize::MAX);
    assert_eq!(
        read.gap.map(|g| g.why),
        Some(aterm_slog::GapWhy::Reset),
        "the predecessor's cursor read on with no gap: first seq {:?}",
        read.recs.first().map(|r| r.seq)
    );
    assert!(
        relaunch.log().ep() > first.log().ep(),
        "a later process's epoch is larger: {} then {}",
        first.log().ep(),
        relaunch.log().ep()
    );
}

#[test]
fn a_cursor_of_another_session_resets() {
    let epochs = process(None);
    let a = spawned_in(&epochs, 0, 6);
    let b = spawned_in(&epochs, 0, 10);
    let reply = render_log(b.log(), &query(&format!("since=4 ep={}", a.log().ep())));
    let mut lines = reply.lines().skip(1);
    assert!(
        lines
            .next()
            .is_some_and(|l| l.starts_with("gap why=reset ")),
        "session A's cursor (ep={} since=4) read on in session B: {reply}",
        a.log().ep()
    );
    assert!(
        lines
            .next()
            .is_some_and(|l| l.starts_with("1 open - spawn ")),
        "{reply}"
    );
    assert_ne!(a.log().ep(), b.log().ep(), "one second, two epochs");
}

#[test]
fn a_restarted_sids_epoch_grows_and_the_bus_fold_starts_over() {
    let first = spawned_in(&process(None), 0, 20);
    let successor = adopted_in(&process(None), 5_000);
    let (e1, e2) = (first.log().ep(), successor.log().ep());
    let mut bus = aterm_slog::Fold::new(e1);
    for r in first.log().window() {
        bus.apply(r);
    }
    let old_head = bus.state().as_of;
    assert!(old_head > successor.log().last_seq(), "PRECONDITION");
    for r in successor.log().window() {
        bus.apply(r);
    }
    assert_eq!(
        bus.state().as_of,
        successor.log().last_seq(),
        "the fold took the successor's records from its first: epochs {e1} then {e2}"
    );
    assert_eq!(bus.state().ep, e2, "and its space");
    assert!(e2 > e1, "monotone: {e1} then {e2}");
}

#[test]
fn two_restarts_in_one_second_take_distinct_epochs_through_the_leaf() {
    let first = adopted_in(&process(None), 100).log().ep();
    let second = adopted_in(&process(Some(first)), 400).log().ep();
    assert!(second > first, "{first} then {second}");
    assert_eq!(
        adopted_in(&process(None), 400).log().ep(),
        first,
        "NEGATIVE CONTROL: no leaf, the same second, the same epoch"
    );
}

#[test]
fn an_adoption_after_a_chain_ahead_of_the_clock_repeats_no_epoch() {
    let mut held = vec![spawned_in(&process(None), 0, 1).log().ep()];
    for i in 1..=5u64 {
        let pred = *held.last().unwrap();
        let mut next = SessionLog::starting(process(None), at(100 * i));
        next.adopt(
            LogAdopt {
                ids: Some(LogCarry {
                    ep: pred,
                    ids: aterm_slog::IdCounters::default(),
                }),
                seq: 10 * i,
                ..LogAdopt::default()
            },
            false,
            None,
            at(100 * i),
        );
        assert_eq!(next.log().ep(), pred + 1, "a carried successor's");
        held.push(next.log().ep());
    }
    let wall = u32::try_from((WALL0 + 1_000) / 1000).unwrap();
    assert!(
        *held.last().unwrap() > wall + 2,
        "PRECONDITION: the chain runs ahead of the clock: {held:?} at {wall}"
    );
    let leaf = held.last().copied();
    let adopted = adopted_in(&process(leaf), 1_000).log().ep();
    assert!(
        held.iter().all(|&e| adopted > e),
        "{adopted} repeats or precedes one of {held:?}"
    );
    let blind = adopted_in(&process(None), 1_000).log().ep();
    assert!(
        held.contains(&blind),
        "NEGATIVE CONTROL: without the leaf, {blind} is one of {held:?}"
    );
}

#[test]
fn the_test_constructor_reads_ep_1_and_the_process_allocator_never_does() {
    assert_eq!(SessionLog::for_test().log().ep(), FIRST_EPOCH);
    let secs = u32::try_from(at(0).wall_ms / 1000).unwrap();
    let started = SessionLog::starting(Arc::new(Epochs::new()), at(0))
        .log()
        .ep();
    assert!(
        started >= secs && started > FIRST_EPOCH,
        "{secs}: {started}"
    );
    let a = SessionLog::default().log().ep();
    let b = SessionLog::default().log().ep();
    assert!(a > FIRST_EPOCH && b > a, "{a}, {b}");
}

#[test]
fn a_told_epoch_at_the_top_saturates_nothing() {
    let top = crate::restore::RestoreManifest::one_terminal_for_test(Some(u32::MAX));
    let wire = top.to_toml().expect("serialize");
    assert!(!wire.contains("log_ep"), "a capture writes none: {wire}");
    let seven = crate::restore::RestoreManifest::one_terminal_for_test(Some(7))
        .to_toml()
        .expect("serialize");
    let planted = seven.replace("log_ep = 7\n", &format!("log_ep = {}\n", u32::MAX));
    assert_ne!(
        planted, seven,
        "PRECONDITION: the planted epoch is in the wire"
    );
    let parsed = crate::restore::RestoreManifest::from_toml(&planted).expect("parse");
    assert_eq!(
        parsed.max_log_ep(),
        None,
        "the leaf's epoch is dropped at the parse"
    );
    let epochs = Arc::new(Epochs::new());
    epochs.note_leaves(&top);
    epochs.note(u32::MAX);
    epochs.note(EPOCH_CEILING + 1);
    assert_eq!(LogCarry::parse(&format!("{} 1 2 3", u32::MAX)), None);
    assert_eq!(
        LogCarry::parse(&format!("{EPOCH_CEILING} 1 2 3")).map(|c| c.ep),
        Some(EPOCH_CEILING),
        "the ceiling itself is an epoch"
    );
    let mut eps = vec![
        spawned_in(&epochs, 0, 1).log().ep(),
        spawned_in(&epochs, 0, 1).log().ep(),
        adopted_in(&epochs, 0).log().ep(),
    ];
    let mut crashed = SessionLog::starting(Arc::clone(&epochs), at(0));
    crashed.adopt(
        LogAdopt {
            keeper_ep: Some(u32::MAX),
            ..LogAdopt::default()
        },
        true,
        None,
        at(0),
    );
    eps.push(crashed.log().ep());
    assert!(
        eps.windows(2).all(|w| w[0] < w[1]) && eps.iter().all(|&e| e <= EPOCH_CEILING),
        "one process issued {eps:?}"
    );
}
