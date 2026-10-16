// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The session log's host side: what each slice-1 producer site writes (on an
//! injected clock), the settle-wake law, the handoff carry and its version
//! skew, the lock discipline, and the `log`/`brief` reply shapes.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use aterm_slog::{Actor, Conf, Fields, Key, Kind, Log, Mark, Node, Src, Stamp, Tier};

use super::*;
use crate::session_store::{ExitActor, ExitReason, SessionHandoff, SessionStore, test_handle};
use crate::session_timeline::{AgentStamp, SessionTimeline};

/// Wall time of the test epoch, so `t=` reads like a real one.
const WALL0: u64 = 1_760_000_000_000;

/// A hand-driven clock: `ms` into the test, on both clocks.
#[derive(Clone)]
struct Hand(Arc<AtomicU64>);

impl Hand {
    fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }

    /// Move the hand on by `ms`.
    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

/// The stamp `ms` into the test.
fn at(ms: u64) -> Stamp {
    Stamp::new(WALL0 + ms, ms)
}

/// Put `timeline`'s session log on a hand clock at 0.
fn hand_clock(timeline: &Mutex<SessionTimeline>) -> Hand {
    hand_clock_from(timeline, 0)
}

/// [`hand_clock`] counting on from `base` ms on both clocks: a base of the
/// host's monotonic now puts the hand on the event loop's own scale.
fn hand_clock_from(timeline: &Mutex<SessionTimeline>, base: u64) -> Hand {
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

/// Every live record of `log`, at the host tier (what `log` prints by
/// default).
fn lines(log: &Log) -> Vec<String> {
    log.window()
        .map(|r| aterm_slog::encode(r, Tier::Host).expect("a logged record fits"))
        .collect()
}

/// The records of `timeline`'s log.
fn lines_of(timeline: &Mutex<SessionTimeline>) -> Vec<String> {
    lines(timeline.lock().unwrap().slog().log())
}

/// A screen stamp for a published verdict.
fn stamp(seq: u64) -> AgentStamp {
    AgentStamp {
        generation: crate::control::ScreenGen { epoch: 0, seq },
        fp: 0x2a,
    }
}

/// A timeline whose session is open (spawned), its clock on the hand.
fn opened() -> (Mutex<SessionTimeline>, Hand) {
    let tl = Mutex::new(SessionTimeline::default());
    let hand = hand_clock(&tl);
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, now);
        g.note_root_group(100);
    }
    (tl, hand)
}

/// `timeline` with `claude` in front (group 300) under the root shell (100),
/// its agent run open (the sweep the name wakes opened it): the record count
/// before the agent's first verdict.
fn with_claude(tl: &Mutex<SessionTimeline>) {
    let mut g = tl.lock().unwrap();
    assert!(g.note_foreground_group(300));
    assert!(g.set_program(300, Some("claude".into())));
    swept(&mut g);
}

/// THE SWEEP A NAME WAKES ([`SessionTimeline::sweep_slog`]) with the group in
/// front unchanged and `read` from the blocks: the names the resolver gave
/// since the last sweep open under the command each ran in. A session with a
/// command open passes its running block, so the read does not take that
/// block for gone.
fn swept_with(g: &mut SessionTimeline, read: &BlockRead) {
    let pgid = g.agent().program_pgid;
    let _ = g.sweep_slog(read, false, 100, pgid);
}

/// [`swept_with`] with no block to report (no command held).
fn swept(g: &mut SessionTimeline) {
    swept_with(g, &BlockRead::default());
}

// ---- (1) register and deregister_local_as -----------------------------

/// THE SESSION NODE: the registry's first registration opens it (`spawn`,
/// `parent=` the spawning session, `epoch=1`), a replace writes nothing, and
/// the deregister closes it with the exit ledger's reason, actor and code.
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
            "1 open - spawn parent=s-0123456789abcdef0123 epoch=1 src=aterm conf=exact t={WALL0}"
        )]
    );
    // NEGATIVE CONTROL: a replace (a relabel) is not a birth.
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
    // The fold says the session exited, with that need.
    let tl = timeline.lock().unwrap();
    let st = tl.slog().log().state();
    assert!(st.closed.is_some());
    assert_eq!(st.top_need().map(|n| n.word), Some("exited"));
}

/// The attribution words are the exit ledger's (`ExitActor`, `ExitReason`).
#[test]
fn exit_actors_and_reasons_map_onto_the_logs_words() {
    assert_eq!(exit_actor(&ExitActor::Human), Actor::Human);
    assert_eq!(exit_actor(&ExitActor::Ctl), Actor::Ctl);
    assert_eq!(exit_actor(&ExitActor::Bridge), Actor::Bridge);
    assert_eq!(exit_actor(&ExitActor::Unknown), Actor::Nobody);
    assert_eq!(exit_actor(&ExitActor::Sid("s-1".into())).word(), "s-1");
    for reason in ExitReason::ALL {
        assert_eq!(
            aterm_slog::Out::Session(exit_out(reason)).word(),
            reason.as_str(),
            "{reason:?}"
        );
    }
}

/// A cold restore's new shell opens `restore`; an adopted one opens `adopt`
/// after its gap; a shell the PTY keeper held opens after `gap why=crash`.
#[test]
fn a_restored_adopted_or_kept_session_opens_as_what_it_is() {
    let restored = test_handle(1);
    let _hand = hand_clock(&restored.ctx.timeline);
    restored
        .ctx
        .timeline
        .lock()
        .unwrap()
        .slog_mut()
        .note_restored();
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
        None,
        true,
    );
    store.register(kept.clone());
    assert_eq!(
        lines_of(&kept.ctx.timeline),
        vec![
            format!("1 gap - gap why=crash by=aterm src=aterm conf=exact t={WALL0}"),
            format!(
                "2 open - adopt epoch={} src=aterm conf=exact t={WALL0}",
                WALL0 / 1000
            ),
        ]
    );
}

// ---- the seamless carry ------------------------------------------------

/// THE CARRY ACROSS A SEAMLESS UPDATE: the record's `log_ids` names the epoch
/// and the counters plus the reserve; the successor's log starts at the next
/// epoch with `gap why=handoff` and issues ids above them, so no node id is
/// issued twice under the sid. A producer that outruns the reserve refuses its
/// Commit. NEGATIVE CONTROL: without the carry the successor issues `r1` again.
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
        Some(carry),
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
    assert_eq!(
        after[..2],
        [
            format!("1 gap - gap why=handoff epoch=2 by=aterm src=aterm conf=exact t={WALL0}"),
            format!("2 open - adopt epoch=2 src=aterm conf=exact t={WALL0}"),
        ]
    );
    let first_run = 4 + reserve;
    assert!(
        after[2].starts_with(&format!("3 open r{first_run} prog par=- prog=vim ")),
        "{after:?}"
    );
    assert_eq!(successor_tl.lock().unwrap().slog().log().ep(), 2);

    // The producer goes on to its reserve: the Commit still stands…
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

    // NEGATIVE CONTROL: no carry, and `r1` is issued again.
    let bare = test_handle(1);
    let _hand3 = hand_clock(&bare.ctx.timeline);
    crate::spawn::mark_handoff_gap(
        &bare.ctx.timeline,
        &bare.ctx.cast,
        &bare.ctx.temporal,
        None,
        0,
        None,
        false,
    );
    {
        let mut g = bare.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, now);
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

/// VERSION SKEW: `log_ids` is an additive key. A manifest without it (an older
/// producer) reads, and carries nothing; a manifest with keys this build does
/// not know (a newer producer) reads; a value of the wrong type or shape
/// carries nothing and never costs the manifest.
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
    // The row round-trips.
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

// ---- (2) the status sweep's block read -----------------------------------

fn fact(blk: u64, start: u64, end: BlockEnd, detail: Option<&str>) -> BlockFact {
    BlockFact {
        id: blk,
        started_ms: Some(WALL0 + start),
        end,
        detail: detail.map(str::to_string),
    }
}

fn running(blk: u64, start: u64, detail: &str) -> BlockRead {
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

fn finished(blk: u64, code: Option<i32>, start: u64, end: u64, detail: &str) -> BlockRead {
    BlockRead {
        facts: vec![fact(blk, start, done(blk, code, start, end), Some(detail))],
        ..BlockRead::default()
    }
}

fn blocks(tl: &Mutex<SessionTimeline>, hand: &Hand, ms: u64, read: &BlockRead) {
    hand.set(ms);
    let mut g = tl.lock().unwrap();
    let now = g.slog().now();
    g.slog_mut().shell_blocks(read, None, now);
}

/// OSC 133 C opens a `cmd` run dated at its own stamp (`blk=`, `detail=` the
/// program, never the line); D closes it with `out=`, `code=` and `dur_ms`
/// from the stamps however late the sweep came. A block the sweep only saw
/// finished gets both records, `dur=wall`, block 0 included. Re-reading a
/// reported block writes nothing (NEGATIVE CONTROL against a sweep that logs
/// on every pass).
#[test]
fn shell_blocks_open_and_close_cmd_runs_from_their_stamps() {
    let (tl, hand) = opened();
    // `true`: seen running at 150 ms (it began at 100), finished at 130.
    blocks(&tl, &hand, 150, &running(0, 100, "true"));
    assert_eq!(tl.lock().unwrap().slog().block_cursor(), Some(0));
    blocks(&tl, &hand, 400, &finished(0, Some(0), 100, 130, "true"));
    assert_eq!(tl.lock().unwrap().slog().block_cursor(), Some(1));
    // `false`: never seen running.
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
    // A failed run is a need until it is seen.
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

/// Every block between two sweeps is a run: the D of a run the sweep saw
/// start, and two whole commands, all read in one pass (`sleep 2` ending and
/// `true; false` typed before the next sweep), in block order.
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

/// A block the prompt closed with no D (an interrupted or replaced command)
/// ends `out=unknown` at the prompt's stamp; a block that finished before this
/// process opened the session is the predecessor's and is not logged, while
/// one still running from before is (it runs here).
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
    // A run whose block left the terminal (a reset) ends too.
    blocks(&tl, &hand, 400, &running(2, 390, "vim"));
    blocks(&tl, &hand, 500, &BlockRead::default());
    assert!(lines_of(&tl)[4].starts_with("5 close r2 cmd out=unknown "));

    // Adopted at 10 s: the block that finished at 2 s is the predecessor's.
    let adopted = Mutex::new(SessionTimeline::default());
    let hand = hand_clock(&adopted);
    hand.set(10_000);
    {
        let mut g = adopted.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().adopt(None, false, now);
        g.slog_mut().open_session(None, now);
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

/// `completed_block` carries the block's C and D stamps, and `block_read`
/// reports, from the cursor on, the running block, each finished one (stamps,
/// code, its program) and none that ran nothing. NEGATIVE CONTROL: past the
/// cursor, an older block is not reported again.
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
}

// ---- (3) note_foreground_group and set_program ---------------------------

/// The program in front is a run: `prog` for `less` (under the `cmd` run that
/// started it), closed `out=unknown` when the group leaves; `agent` for an
/// agent's name, with a tracker. The root shell in front is the session at its
/// prompt, never a run (NEGATIVE CONTROL: the same name in ANOTHER group, a
/// nested shell, is one).
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
    // The same group naming another program (an exec) supersedes its run.
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

// ---- (4) settle_agent_word -----------------------------------------------

/// Every move of the published verdict writes `mark agent word= rev= gen= fp=`
/// on the agent run, and drives the tracker: `busy` opens a turn, a box opens
/// its approval step, `idle` arms the settle and the tick at the settle closes
/// the turn `out=done`, dated when the idle first held.
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

/// A screen that names an agent no program name did (Claude Code run as
/// `node`) opens an `agent` run under the `prog` run, and closes it when the
/// screen stops naming one.
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

// ---- (5) note_person -----------------------------------------------------

/// A person's input: `mark - human by=human` once per burst (the bytes are
/// never recorded), and the next turn is theirs (`by=human`).
#[test]
fn a_persons_input_marks_its_burst_and_attributes_the_turn() {
    let h = test_handle(1);
    let hand = hand_clock(&h.ctx.timeline);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, now);
        g.note_root_group(100);
    }
    with_claude(&h.ctx.timeline);
    hand.set(100);
    crate::app_input::note_person(&h.ctx);
    hand.set(200);
    crate::app_input::note_person(&h.ctx);
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
    assert_eq!(
        *humans[0],
        format!(
            "3 mark - human by=human src=input conf=strong t={}",
            WALL0 + 100
        )
    );
    assert!(
        got.iter()
            .any(|l| l.contains(" open t1 turn par=r1 by=human ")),
        "{got:?}"
    );
}

// ---- (6) cmd_turn ----------------------------------------------------------

/// A `ctl turn` exchange: the turn it opens carries `ledger=` (the id `history`
/// records) and `by=ctl`. Through the real verb, which takes the timeline's
/// lock with no other held.
#[test]
fn a_ctl_turn_labels_the_turn_it_opens_with_its_ledger_id() {
    let h = test_handle(0);
    let hand = hand_clock(&h.ctx.timeline);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, now);
        g.note_root_group(100);
    }
    with_claude(&h.ctx.timeline);
    let store = crate::session_store::new_store();
    let registry = crate::subscribe::new_registry();
    hand.set(10);
    let reply = crate::control::cmd_turn(
        &h.term,
        &store,
        0,
        "idle=1 timeout=60000 submit=none hello",
        &registry,
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

/// THE CTL TURN ARMS NOTHING: at rest it closes the resting turn at once, busy
/// it leaves no settle, so the control thread never owes the loop a wake it
/// cannot hand over.
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
    assert_eq!(g.slog_mut().ctl_turn(Actor::Ctl, 1, now), None, "busy");
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
    assert_eq!(g.slog_mut().ctl_turn(Actor::Ctl, 2, now), None, "at rest");
    assert_eq!(g.slog_wake(), None, "…and the resting turn closed");
}

// ---- the settle wake and the event loop ------------------------------------

/// THE TICK LAW. An idle session arms nothing: the loop's board stays empty
/// and the session log folds no deadline. A resting turn arms one wake at the
/// settle; the loop's tick before it changes nothing, at it closes the turn,
/// and the board is empty again — no past instant is ever re-armed.
/// NEGATIVE CONTROL: with no tick the turn stays open past its settle (the
/// tick is what closes it, not a later verdict).
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

    // A turn comes to rest: the producer hands the board its wake, as the
    // sweep does. The log's clock is a hand on the loop's own scale.
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
    // An early tick: the board keeps the same wake, the turn stays open.
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

    // At the settle: the log's hand reaches it, and the board's entry is made
    // due by hand too, as the loop's clock would.
    hand.set(aterm_slog::SETTLE_MS);
    app.session_status.slog_wakes.note(0, Some(0));
    app.tick_session_logs();
    assert!(!turn_open(&timeline), "the tick closed the turn");
    assert!(app.session_status.slog_wakes.is_empty(), "nothing re-armed");
    assert!(app.next_session_log_wake().is_none());

    // NEGATIVE CONTROL: a second resting turn the loop never ticks stays open.
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

/// The board holds one entry per session, its soonest; a session whose
/// tracker went away is dropped at its tick.
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

// ---- the lock discipline ---------------------------------------------------

/// THE TIMELINE STAYS A STRICT LEAF: every producer runs with the timeline's
/// lock and takes no other. Another thread holds the session's store, turn
/// ledger and meta locks (CONTROL: they really are held, `try_*` fails) while
/// a worker runs every producer site through its production entry point; one
/// that reached for any of them would block, and the one-minute hang detector
/// would fail the test. `cmd_turn`'s producer is placed after the ledger and
/// lease guards drop, and the census proves the graph gained no edge.
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
            let _ = g.slog_mut().ctl_turn(Actor::Ctl, 1, now);
            g.slog_mut().failure_seen(now);
            let _ = g.handoff_log_ids();
        }
        crate::app_input::note_person(&ctx);
        crate::app_input::note_person(&ctx);
        ran_tx.send(()).unwrap();
    });
    ran_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("a producer waited on a lock other than the timeline's");
    worker.join().unwrap();
    done_tx.send(()).unwrap();
    holder.join().unwrap();
}

// ---- the `log` verb --------------------------------------------------------

/// A session log with a run, a cwd mark (host tier) and a title mark (content
/// tier).
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

/// The reply's shape: `OK <n> v=2 as_of= ep= oldest=`, then the records; the
/// count is the lines that follow.
#[test]
fn log_answers_its_header_then_one_record_per_line() {
    let log = sample_log();
    let reply = render_log(&log, &query(""));
    let mut it = reply.lines();
    assert_eq!(it.next(), Some("OK 4 v=2 as_of=4 ep=3 oldest=1"));
    let body: Vec<&str> = it.collect();
    assert_eq!(body.len(), 4);
    assert!(body[1].starts_with("2 open r1 cmd par=- blk=1 src=shell conf=strong t="));
    // The subtree, the level, the kinds, the class and n.
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
    // Usage.
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

/// THE TIER RULE: the default reply stops at the host tier (the cwd shows, the
/// title does not); `tier=content` adds the content tier. NEGATIVE CONTROL: the
/// title is in the log, so the default's silence is the filter, not absence.
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

/// THE CURSOR: `since=<seq> ep=<e>` reads on past `seq`; a reader behind the
/// ring gets `gap why=evicted` with the exact count; a reader of another epoch
/// (or ahead of the head) gets `gap why=reset` and the epoch's records.
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

    // A small ring that has dropped its oldest records.
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
            "OK 4 v=2 as_of=6 ep=1 oldest=4 lost=3\ngap why=evicted lost=2 resync_to=3\n4 mark"
        ),
        "{behind}"
    );
}

/// The verb reads the target's own timeline: `cmd_log` over a ctx.
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

// ---- the `brief` verb ------------------------------------------------------

/// `brief`: the engine's brief of the target, `OK <n> brief sid=…` with `n`
/// the lines that follow, within its budget; `brief all` covers every session
/// under one header, each without its own `OK`.
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

/// Nothing a producer writes is text that could be typed: no record holds a
/// command line, and no producer is handed the session's sink (the log is
/// read only by `log` and `brief`).
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

// ---- review round: the producers ---------------------------------------------

/// THE RENAME CONFIRMS: a group named as an interpreter (`node`) whose screen
/// already named Claude Code, renamed `claude` mid-turn, keeps its agent run
/// and the turn in progress: the run is amended `prog=claude` and lives with
/// its group (the screen's `-` no longer closes it; the group leaving does).
/// Nothing closes, no turn is split in two. NEGATIVE CONTROL: the same group
/// renamed to another program (an exec) supersedes its run, the agent run under
/// it closes with it, and no closed node is amended.
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

    // NEGATIVE CONTROL: an exec to another program.
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

/// A `ctl turn` through the real verb, on the hand clock: `io` is its routes,
/// `args` its arguments. Returns the ledger id it recorded.
fn ctl_turn_through_the_verb(
    h: &crate::session_store::SessionHandle,
    args: &str,
    io: &crate::control::TurnIo<'_>,
) -> u64 {
    let store = crate::session_store::new_store();
    let registry = crate::subscribe::new_registry();
    let reply = crate::control::cmd_turn(&h.term, &store, 0, args, &registry, &h.ctx, io);
    assert!(reply.starts_with("OK "), "{reply}");
    let ledger = h.ctx.turns.lock().unwrap().since(None).last().map(|r| r.id);
    ledger.expect("the exchange is on the ledger")
}

/// A session with Claude in front, its log on a hand clock at 0.
fn claude_handle() -> (crate::session_store::SessionHandle, Hand) {
    let h = test_handle(0);
    let hand = hand_clock(&h.ctx.timeline);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, now);
        g.note_root_group(100);
    }
    with_claude(&h.ctx.timeline);
    (h, hand)
}

/// THE EXCHANGE IS NOTED WHEN ITS BYTES GO IN: a person typing during a
/// `turn yield=` (the hands the yield waits on) does not take the turn the
/// exchange then opens: it carries `by=ctl` and its `ledger=`, the person's
/// burst its own `mark - human`.
#[test]
fn a_persons_keys_during_the_yield_do_not_take_the_exchanges_turn() {
    let (h, hand) = claude_handle();
    let (ctx, during) = (Arc::clone(&h.ctx), hand.clone());
    let momentum = move || {
        during.set(1_000);
        crate::app_input::note_person(&ctx);
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

/// A TYPED TURN LONGER THAN THE 5 s WINDOW still opens as the exchange's: the
/// exchange is noted at its first key and again at its Enter, its last byte.
/// NEGATIVE CONTROL: the same keys with no Enter, and the agent busy more
/// than 5 s after the exchange was last noted, open a turn no one is credited
/// with — the window runs from the exchange's noted input, not without bound.
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
        // 60 keys at 100 ms: 6 s of typing, the Enter at 6.1 s.
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

/// A TYPED EXCHANGE WITH NO ENTER IS NOTED AT ITS FIRST KEY: under
/// `submit=none` no Enter is noted, so the first key is the exchange's one
/// note. A turn resting as the typing starts closes there (the exchange starts
/// a new turn), and the turn the agent then opens is the exchange's, `ledger=`
/// and `by=ctl`. NEGATIVE CONTROL: the same agent busy again with no exchange
/// between goes on in its one turn, so the second turn is the first key's
/// doing.
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
        // Busy again before the resting turn's settle came due (200 + 1000).
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

/// The agent's first turn after its name waited for a block read: block 1
/// (`claude`, C at 5) read running at 10, group 300 in front at 20, the
/// resolver naming it `claude` at 30, then `typed` (on the hand clock, with
/// the handle) before the sweep at 270 places the name and the verdict
/// `busy` at 275 opens the first turn. Returns the log, and the ledger id
/// `typed` returned.
fn first_turn_after(
    typed: &dyn Fn(&crate::session_store::SessionHandle, &Hand) -> Option<u64>,
) -> (Vec<String>, Option<u64>) {
    let h = test_handle(0);
    let hand = hand_clock(&h.ctx.timeline);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, now);
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

/// WHAT REACHED THE SESSION WHILE THE NAME WAITED IS ITS RUN'S: a person's
/// input, and a `ctl turn` through the real verb, that come after the
/// resolver named the agent but before the sweep's block read opened its
/// run, attribute the run's first turn (`by=human`; `ledger=` and `by=ctl`),
/// as they would had the run opened at the name; in their arrival order (a
/// person's keys after the exchange drop its ledger, the exchange after the
/// keys keeps it). NEGATIVE CONTROL: input before the name (the Enter that
/// started it, at the shell) is not the agent's: the turn is no one's.
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
        crate::app_input::note_person(&h.ctx);
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
        crate::app_input::note_person(&h.ctx);
        Some(ledger)
    });
    let t1 = turn(&got);
    assert!(
        t1.contains(" par=r2 by=human ") && !t1.contains(" ledger="),
        "keys after the exchange: {got:?}"
    );
    let (got, ledger) = first_turn_after(&|h, hand| {
        hand.set(100);
        crate::app_input::note_person(&h.ctx);
        hand.set(150);
        Some(exchange(h))
    });
    let ledger = ledger.expect("the exchange's ledger id");
    assert!(
        turn(&got).contains(&format!(" par=r2 ledger={ledger} by=ctl ")),
        "the exchange after the keys: {got:?}"
    );

    // NEGATIVE CONTROL: the keys came before the name.
    let h = test_handle(0);
    let hand = hand_clock(&h.ctx.timeline);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, now);
        g.note_root_group(100);
    }
    blocks(&h.ctx.timeline, &hand, 10, &running(1, 5, "claude"));
    hand.set(15);
    crate::app_input::note_person(&h.ctx);
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

/// A COMMAND FROM BEFORE THIS PROCESS (claude started from zsh, carried across
/// a seamless update) measures its duration on the wall clock: its C predates
/// the successor's monotonic clock, so its close says `dur=wall` and a
/// `dur_ms` that matches its own open and close `t=`. NEGATIVE CONTROL: a
/// command started here measures on the monotonic clock, with no `dur=wall`.
#[test]
fn a_command_from_before_this_process_measures_its_duration_on_the_wall() {
    let tl = Mutex::new(SessionTimeline::default());
    let hand = hand_clock(&tl);
    hand.set(10_000);
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().adopt(None, false, now);
        g.slog_mut().open_session(None, now);
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
    // NEGATIVE CONTROL: begun here.
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

/// THE RUNS' TIME ORDER over the records `lines`: (1) a child's close `t=`
/// and `dur_ms` never exceed its parent's; (2) no node closes before it
/// opened; (3) a `cmd` run never opens before the `cmd` run logged before it
/// (the shell runs one command at a time) closed; (4) no node opens before
/// the node it hangs under (the session node aside: a command carried from
/// a predecessor began before this log's session opened). `Err` names the
/// first record that breaks one.
fn time_order_holds(lines: &[String]) -> Result<(), String> {
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

/// THE NESTING LAW, THROUGH THE SWEEP'S OWN STEP: a program never closes after
/// the command that ran it. [`SessionTimeline::sweep_slog`] applies the
/// shell's D before the foreground move, so the program in front closes at
/// the D, with its command. NEGATIVE CONTROL (the other order, the move first,
/// as a sweep that saw the group leave before it read the D): the program
/// closes at the sweep's instant and the command's close is lifted to it, so
/// the law still holds, but nothing closes at the D; the stamps are the
/// order's. And the checker refuses a log whose child closes later.
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
        // The sweep at 1000: the shell's D at 800, the group back at the shell.
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
    // NEGATIVE CONTROL: the checker.
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

/// The node of the `cmd` run of block `blk` in `got`, and the open record of
/// the run whose `prog=` is `prog`.
fn cmd_of(got: &[String], blk: u64) -> String {
    got.iter()
        .find(|l| l.contains(" open ") && l.contains(&format!(" cmd par=- blk={blk} ")))
        .and_then(|l| l.split(' ').nth(2))
        .unwrap_or_else(|| panic!("no cmd run for block {blk}: {got:?}"))
        .to_string()
}

/// The setup of THE RACE WITH TYPEAHEAD: block 1 (`vim`, C at 5) read
/// running at 10 and again at 1000, its program named at 20; vim exits, the
/// shell's D (1001), the next prompt and the typed-ahead command's C (1002)
/// land after the block read of the sweep at 1000, whose foreground read
/// sees the new group `pgid`, stamped 1003. The resolver then names that
/// group `name` at `named` ms: the log still holds vim's command open.
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

/// THE RACE WITH TYPEAHEAD, THE PARENT CHOSEN AFTER THE BLOCKS ARE READ: a
/// name the resolver gives waits for the next block read
/// ([`SessionLog::program_named`]), which hangs it under the command it ran
/// in ([`SessionLog::shell_blocks`]), never under the command the log still
/// held when the name came. Every one of the three time-order laws holds
/// ([`time_order_holds`]) in each case:
///
/// - A typed-ahead program still running (`sleep`, block 2): its `prog` run
///   opens under block 2's command at its own instant and stays open while
///   it is in front; the next sweep and the same name again change nothing.
/// - The same for an agent (`claude`): its `agent` run under block 2's
///   command, and the turn its verdict opens, stay open.
/// - A typed-ahead `ls` that finished before the sweep (1002..1020), named at
///   1010: its run hangs under ls's own command and closes at ls's D.
/// - A name that comes after every command the read holds ended (`ls` at
///   1200; ls ran 1002..1020, `false` 1030..1040): no command was running
///   it, so it is the session's, and the read's commands keep their stamps.
///
/// In each, vim's command closes at the foreground move that closed vim's run
/// before the D was read (1003; the nesting law), not at the name's instant.
/// NEGATIVE CONTROLS: the checker refuses a close before its open and a
/// command that opens before the previous one closed.
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

    // A: the typed-ahead program is still running.
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

    // A2: the same for an agent, its turn included.
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

    // The typed-ahead `ls` finished before the sweep: under its own command.
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

    // B: the name comes after every command of the read ended.
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

    // NEGATIVE CONTROLS: the checker.
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

/// A cut read's facts: blocks `ids`, each `true` from `100 + 10 * id` ms to
/// 5 ms later, then (`last`) one more still running from `start`.
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

/// A NAME PAST A CUT READ WAITS FOR THE READ THAT REACHES ITS COMMAND: 69
/// `true`s and then `prog` (block 70, running from 800) typed between two
/// sweeps; the sweep at 1000 reads blocks 1-32 (cut at its cap) and sees
/// group `pgid` in front, which the resolver names `prog` at 1005; the sweep
/// at 1250 reads 33-64 (cut again) and the verdict `verdict` (if any)
/// publishes at 1260, as the sweep publishes after its read; the sweep at
/// 1500 reads 65-70. Returns the log.
fn named_past_a_cut(pgid: i32, prog: &str, verdict: Option<&'static str>) -> Vec<String> {
    let (tl, _) = past_a_cut(pgid, prog, verdict);
    lines_of(&tl)
}

/// [`named_past_a_cut`]'s timeline and clock, for a test that goes on.
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

/// A NAME GIVEN PAST A CUT READ: the command it ran under may lie past the
/// cut, so the read that stops at its cap leaves it waiting, and the read
/// that reaches the present places it by the usual rule (the first command
/// not ended by the name's instant): `sleep`'s `prog` run opens under block
/// 70's command, at its name (1005), after that command opened (800). A
/// command never hangs under a run that opened after it began (law 4 of
/// [`time_order_holds`]): an agent whose verdict moves before the read
/// reaches its command (`claude`, `busy` at 1260) has its run opened then,
/// at the session (its turn tracker needs the run at once), and the
/// commands the next read opens (blocks 65-70, from 750) hang beside it,
/// never under it; its turn is its own. NEGATIVE CONTROL: the checker
/// refuses a node that opens before its parent.
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

    // NEGATIVE CONTROL: the checker.
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

/// The node of the first record in `got` that opens and contains `what`.
fn opened_node(got: &[String], what: &str) -> String {
    got.iter()
        .find(|l| l.contains(" open ") && l.contains(what))
        .and_then(|l| l.split(' ').nth(2))
        .unwrap_or_else(|| panic!("no open record with {what:?}: {got:?}"))
        .to_string()
}

/// A timeline whose session runs a NESTED SHELL under an outer shell that
/// writes no OSC 133: group 400 in front from 20, named `bash` at 300; the
/// name waits for the next sweep's block read.
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

/// The sweep's session-log step over `read` with `front` in front, or (for
/// `None`) the blocks told no group in front: placement by time alone, as
/// before the sweep told the blocks its group.
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

/// AN AGENT RUN OPENED PAST A CUT CLOSES AT ITS COMMAND'S D. Past a cut read
/// the log opens `claude`'s run at the session as its verdict moves
/// ([`past_a_cut`]), and the read that reaches its command (block 70, from
/// 800) cannot hang it there again. Claude exits (D at 2000) and the
/// person's `ls` (C 2010, its own group 500) is read in the same sweep, at
/// 2100: the D closes the agent run and its turn at 2000, as it closes the
/// runs under a command ([`SessionLog::end_front_beside`]), so `ls` hangs at
/// the session, not under a run that did not run it, and keeps its own D:
/// `out=fail code=2` at 2050 when the next sweep reads it, and the failure
/// need. Before, `ls` hung under the agent run, and the sweep's foreground
/// move closed it `out=gone` with that run, its code never logged. The same
/// when `ls` had finished too by that sweep (the root shell back in front).
/// Every law of [`time_order_holds`] holds. NEGATIVE CONTROL: a command's end
/// never closes the run it hangs under, which ran it: a nested shell's `ls`
/// hangs under `bash`, which stays open past ls's D.
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

    // NEGATIVE CONTROL: the run a command hangs under ran it.
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

/// A RUNNING COMMAND NEVER HANGS UNDER A RUN THE SWEEP'S OWN MOVE CLOSES. A
/// nested shell (`bash`, group 400, its name placed by an empty sweep at
/// 350) runs `vim` in vim's own group (600), and the sweep at 1010 reads
/// vim's C (1000) with group 600 in front. The foreground move that sweep
/// applies next closes bash's run, and a command under it would close with
/// it, `out=gone`, its D never read (the cursor has passed its block). So
/// vim hangs at the session ([`SessionLog::open_cmd`]), and its D (1900,
/// code 3) closes it `out=fail code=3`. CONTROL: with bash's group still in
/// front at that read (vim run in the shell's own group), vim hangs under
/// bash and keeps its D the same. NEGATIVE CONTROL: a read told no group in
/// front places by time alone, as before, and loses vim's D to the move's
/// cascade, which the assertion here refuses.
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

    // CONTROL: the shell's own group holds the front.
    let got = vim(Some(400));
    time_order_holds(&got).unwrap_or_else(|e| panic!("shell's group: {e}: {got:?}"));
    let bash = opened_node(&got, " prog par=- prog=bash ");
    assert!(
        got.iter()
            .any(|l| l.contains(&format!(" cmd par={bash} blk=1 detail=vim ")))
            && kept_its_d(&got),
        "shell's group: {got:?}"
    );

    // NEGATIVE CONTROL: by time alone.
    let got = vim(None);
    assert!(!kept_its_d(&got), "by time alone: {got:?}");
}

/// A NESTED SHELL'S NAME, READ WITH ITS FIRST COMMAND, HANGS ABOVE IT. The
/// outer shell writes no OSC 133; `bash` ([`nested_shell`]) writes the
/// blocks, and its `ls` (C 400, D 410) is read at 500, in the same sweep that
/// places bash's name, with bash's group still in front. A program in front
/// after a command's D did not run as that command (the shell takes the
/// terminal back before it writes the D), so bash's run opens at its name,
/// at the session, and `ls` hangs under it
/// ([`SessionLog::open_names_before`]); `make` (1000-1100, read at 1500)
/// hangs under it too, and bash stays open. Before, bash hung under `ls`,
/// closed at ls's D, and `make` landed at the session. All four laws hold.
/// CONTROL, A NAME WHOSE COMMAND'S C WAS READ LATE (the PTY reader behind
/// the sweep that saw its group): `sleep` named at 1025, its C stamped
/// 1030, read running with its group in front, or finished with the shell
/// back in front, is its command's, as before (opened as that command
/// opened). NEGATIVE CONTROL: told no group in front, the read places bash
/// as before, under `ls`.
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

    // CONTROL: a C read late is its own command's.
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

    // NEGATIVE CONTROL: by time alone.
    let got = nested(None);
    let ls = cmd_of(&got, 1);
    assert!(
        got.iter()
            .any(|l| l.contains(&format!(" prog par={ls} prog=bash "))),
        "by time alone: {got:?}"
    );
}

/// AN ADOPTION TOLD NO EPOCH TAKES ONE NO PREDECESSOR HAD: a reader holding
/// the spawned predecessor's `since=5 ep=1` is reset (`gap why=reset`) by the
/// successor's log once it has passed seq 5, for the PTY keeper's crash
/// adoption and an uncarried one alike, so it sees the `gap why=crash` /
/// `why=handoff` and the `open - adopt`. CONTROL: a cursor of the successor's
/// own epoch reads on with no gap.
#[test]
fn an_adoption_told_no_epoch_resets_a_predecessors_cursor() {
    for keeper in [true, false] {
        let tl = Mutex::new(SessionTimeline::default());
        let hand = hand_clock(&tl);
        hand.set(1_000);
        {
            let mut g = tl.lock().unwrap();
            let now = g.slog().now();
            g.slog_mut().adopt(None, keeper, now);
            g.slog_mut().open_session(None, now);
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

/// THE FAILURE NEED ENDS: a command that failed is a need until the next
/// command ends (`ok` lifts it, as `status outcome=` moves on) or the person
/// sees it (`FailureMark` → `mark - seen by=human`). NEGATIVE CONTROL: with
/// neither, it stands.
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

/// `FailureMark`'S SEEN IS THE LOG'S, AT ITS PRODUCERS: a failure the person
/// is looking at as it publishes (the status sweep's `seen`), and one the
/// person comes to look at later (`App::note_session_seen`, a window gaining
/// focus), each write `mark - seen by=human` after the run's close, and
/// `need=failure` is gone from the fold the brief reads. NEGATIVE CONTROL:
/// while nobody looks, the need stands and no `seen` is written.
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

/// A BURST OF BLOCKS IS READ OLDEST FIRST, NONE LOST: 50 commands finished
/// between two sweeps are read 32 at a time from the cursor, the read owing
/// the rest (`more`), and every one becomes a run. An adopted session's
/// history (finished before the log opened) is passed over without spending
/// the cap. Blocks the terminal's history dropped before any read reached them
/// are said with `gap why=evicted`; NEGATIVE CONTROL: a cursor at the first
/// block held misses none.
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

    // The prompt in front is never passed over: the command typed at it is
    // read (NEGATIVE CONTROL against a cursor that skipped the block a
    // command has yet to run in).
    term.process(b"true again\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
    let from = tl.lock().unwrap().slog().block_cursor();
    assert_eq!(from, Some(50));
    let again = block_read(&term, from, None, None);
    assert_eq!(again.facts.iter().map(|f| f.id).collect::<Vec<_>>(), [50]);
    blocks(&tl, &hand, 300, &again);
    term.process(prompt);

    // An adopted session's history spends nothing, and the prompt in front
    // is still owed.
    let past = block_read(&term, None, Some(u64::MAX), None);
    assert!(past.facts.is_empty() && !past.more);
    assert_eq!(past.through, Some(50));

    // The terminal keeps its newest 1000 blocks: a cursor behind them missed some.
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

/// One status sweep's block read and write, on the hand clock at `ms`: the
/// read from the log's own cursor, as the sweep takes it.
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

/// The `gap why=evicted` records of `timeline`'s log.
fn evicted_gaps(tl: &Mutex<SessionTimeline>) -> usize {
    lines_of(tl)
        .iter()
        .filter(|l| l.contains(" gap - gap why=evicted "))
        .count()
}

/// A CLEARED BLOCK HISTORY IS SAID ONCE, AND ONLY WHEN IT TOOK A COMMAND:
/// ED 3 (`clear`), Cmd-K (`clear_scrollback`) and RIS drop the terminal's
/// blocks while its block ids count on. A `clear` or a `reset` (RIS, which
/// also swallows the shell's D) that ran and finished between two sweeps
/// took its own block unread: one `gap why=evicted`, then ten more sweeps at
/// the new prompt add none (the cursor moved past the drop, to the first
/// block still held). One the sweep saw running was read (it closes
/// `out=unknown`), and a Cmd-K or RIS at an idle prompt took only a prompt
/// that ran nothing (no C landed off the held blocks): no gap. In every case
/// the command run at the new prompt is logged. NEGATIVE CONTROL: a log with
/// no count from a read before at its cursor cannot tell, and counts the
/// same Cmd-K's drop as a loss.
#[test]
fn a_cleared_block_history_is_said_once_and_only_when_it_took_a_command() {
    let prompt = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    let cmd_opens = |tl: &Mutex<SessionTimeline>| {
        lines_of(tl)
            .iter()
            .filter(|l| l.contains(" cmd par=- blk="))
            .count()
    };
    for (case, gaps) in [
        ("clear", 1),
        ("clear seen running", 0),
        ("reset", 1),
        ("reset seen running", 0),
        ("cmd-k", 0),
        ("ris", 0),
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
            "reset" => term.process(b"reset\r\n\x1b]133;C\x1b\\\x1bc\x1b]133;D;0\x1b\\"),
            "reset seen running" => {
                term.process(b"reset\r\n\x1b]133;C\x1b\\");
                sweep_term(&tl, &hand, &term, 50);
                term.process(b"\x1bc\x1b]133;D;0\x1b\\");
            }
            "cmd-k" => term.clear_scrollback(),
            _ => term.process(b"\x1bc"),
        }
        term.process(prompt);
        assert!(
            term.all_blocks().next().is_some_and(|b| b.id >= 2),
            "{case}: PRECONDITION: the history before the new prompt is gone"
        );
        for i in 0..10 {
            sweep_term(&tl, &hand, &term, 100 + i * 250);
        }
        assert_eq!(evicted_gaps(&tl), gaps, "{case}: {:?}", lines_of(&tl));
        assert_eq!(
            tl.lock().unwrap().slog().block_cursor(),
            Some(2),
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

    // NEGATIVE CONTROL: no count from a read before, nothing to go by.
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    term.process(prompt);
    term.process(b"ls\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
    term.process(prompt);
    term.clear_scrollback();
    term.process(prompt);
    let read = crate::session_status::block_read(&term, Some(1), None, None);
    assert_eq!(read.dropped, 1);
    let (tl, hand) = opened();
    blocks(&tl, &hand, 100, &read);
    assert_eq!(evicted_gaps(&tl), 1, "{:?}", lines_of(&tl));
    assert_eq!(tl.lock().unwrap().slog().block_cursor(), Some(2));
}

/// A CLEAR AT AN IDLE PROMPT AFTER A D THE SWEEP HAS NOT READ LOSES NOTHING:
/// the sweep saw `make` running (its `cmd` run open); it then ends (D), a
/// prompt follows, the history is cleared there (ED 3, Cmd-K or RIS), with
/// or without a sweep between the clear and the next prompt, and the next
/// prompt is an empty Enter (A with no D). `make`'s own C was the open run's
/// (it closes `out=unknown`) and no other C landed: no gap, in all six. A
/// command run at the cleared prompt before the clear IS a loss: one gap
/// (CONTROL), and so is one run after a clear that left no block to read it
/// from, before the next sweep (the C with no block at all).
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
                    usize::from(ran),
                    "{clear} swept={swept} ran={ran}: {:?}",
                    lines_of(&tl)
                );
            }
        }
    }
    // A C after a clear took the prompt it was typed at: no block holds it.
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    let (tl, hand) = opened();
    term.process(prompt);
    sweep_term(&tl, &hand, &term, 10);
    term.clear_scrollback();
    term.process(b"ls\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
    sweep_term(&tl, &hand, &term, 100);
    term.process(prompt);
    sweep_term(&tl, &hand, &term, 200);
    assert_eq!(evicted_gaps(&tl), 1, "{:?}", lines_of(&tl));
}

/// THE CAP OWES A LOOK ONLY FOR A BLOCK THAT MAY YET BE A FACT: `n` commands
/// and the prompt in front, read once. At `n` = 31 and 32 the read takes
/// every command and what is left past the cap is the idle prompt, which can
/// never be a fact: no look owed (`more` false), and a Cmd-K at that prompt
/// before the next sweep loses nothing. NEGATIVE CONTROL: at 33 the 33rd
/// command is past the cap, owed (`more`), and the same Cmd-K takes it
/// unread: one gap.
#[test]
fn the_cap_owes_a_look_only_for_a_block_that_may_yet_be_a_fact() {
    use crate::session_status::{BLOCK_READ_MAX, block_read};
    let prompt = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    for (n, more, gaps) in [(31, false, 0), (32, false, 0), (33, true, 1)] {
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        for i in 0..n {
            term.process(prompt);
            term.process(format!("true {i}\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\").as_bytes());
        }
        term.process(prompt);
        let (tl, hand) = opened();
        let read = block_read(&term, None, None, None);
        assert_eq!(read.facts.len(), n.min(BLOCK_READ_MAX), "n={n}");
        assert_eq!(read.more, more, "n={n}");
        blocks(&tl, &hand, 10, &read);
        term.clear_scrollback();
        term.process(prompt);
        sweep_term(&tl, &hand, &term, 100);
        sweep_term(&tl, &hand, &term, 200);
        assert_eq!(evicted_gaps(&tl), gaps, "n={n}: {:?}", lines_of(&tl));
    }
}

/// A seeded xorshift64* stream: the oracle test's only source of choice.
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

/// What a run of the block-read oracle mixes in ([`lost_command_oracle`]).
#[derive(Clone, Copy)]
struct OracleCfg {
    /// The seeds, first to last.
    seeds: (u64, u64),
    steps: usize,
    /// Marks as OSC 633 half the time (as OSC 133 otherwise).
    osc633: bool,
    /// The nonce gate: signed, unsigned and wrong-`id=` marks, the gate
    /// switched on and off.
    nonce: bool,
    /// Bursts of 900 to 1300 commands between two sweeps: past the
    /// terminal's 1000-block store, which evicts blocks no read reached.
    giant: bool,
    /// Seamless updates: the checkpoint's meta crosses as JSON (half the
    /// time as an older predecessor's, with no `started_seq`), the
    /// successor's log starts over and sweeps at once.
    adopt: bool,
    /// A NEGATIVE CONTROL's change to every read before the log takes it.
    mutate: fn(&mut BlockRead),
}

/// The oracle's model of the integration gate's prompt phase: a C is a
/// command only after an A and its B.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OraclePhase {
    None,
    Prompt,
    Typing,
    Exec,
    Done,
}

/// A command the shell started (a C the gate took): its block (none if a
/// clear took the prompt it was typed at), whether its D landed on that
/// block while held, the epoch a read covered it in while held, the first
/// sweep after its block went, and the epoch it ran in.
struct OracleCmd {
    blk: Option<u64>,
    done: bool,
    read: Option<usize>,
    gone_by: Option<usize>,
    epoch: usize,
}

/// What the oracle saw of the log, sweep by sweep: in which sweeps a `gap
/// why=evicted` was written, and the blocks a `cmd` record names.
#[derive(Default)]
struct OracleSeen {
    sweeps: usize,
    ms: u64,
    gaps_at: Vec<usize>,
    logged: HashSet<u64>,
    last_seq: u64,
    epoch: usize,
}

/// What [`lost_command_oracle`] found over its seeds: false and missed gaps
/// (with the first few seeds of either), read commands that finished on
/// their block but were never logged, and how much of each thing it means to
/// cover the runs did.
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

/// The nonce the oracle's terminal authorizes.
const ORACLE_NONCE: [u8; 32] = [0xab; 32];

/// One mark, `body` (`A`, `C`, `D;0`, …), as OSC 133 or 633, signed with
/// the authorized nonce (`Some(true)`), a wrong one (`Some(false)`) or none.
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

/// The oracle's sweep: the real sweep step on the log's own cursor
/// ([`sweep_term`], through `mutate`), then what it wrote and which commands
/// its cursor (or a `cmd` record) covered while their blocks were held.
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

/// Ground truth of which blocks went: any the terminal no longer holds.
fn oracle_gone(term: &aterm_core::terminal::Terminal, cmds: &mut [OracleCmd], sweeps: usize) {
    let held: HashSet<u64> = term.all_blocks().map(|b| b.id).collect();
    for c in cmds.iter_mut() {
        if c.gone_by.is_none() && c.blk.is_some_and(|b| !held.contains(&b)) {
            c.gone_by = Some(sweeps);
        }
    }
}

/// A successor's timeline: its log adopted (no carry) and opened at `ms`.
fn oracle_adopted(ms: u64) -> (Mutex<SessionTimeline>, Hand) {
    let tl = Mutex::new(SessionTimeline::default());
    let hand = hand_clock(&tl);
    hand.set(ms);
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().adopt(None, false, now);
        g.slog_mut().open_session(None, now);
        g.note_root_group(100);
    }
    (tl, hand)
}

/// THE ORACLE: seeded runs of a real terminal read by the real sweep step,
/// judged against what the terminal did. The oracle knows every command the
/// shell started (a C the gate took, checked against the terminal's own
/// count), whether it finished on its block, and when its block went (the
/// terminal no longer holds it: a clear, RIS, or the 1000-block store's
/// eviction). A command is read when a sweep left the log's cursor past its
/// block (or a `cmd` record names it) while the block was held; one whose
/// block went first is lost. Each sweep that is the first after a lost
/// command's block went must write `gap why=evicted`, and no other sweep
/// may; a loss after the epoch's last sweep is the handoff's (`gap
/// why=handoff`), not the epoch's to say.
fn lost_command_oracle(cfg: OracleCfg) -> OracleTally {
    use OraclePhase::{Done, Exec, None as Idle, Prompt, Typing};
    use aterm_core::terminal::{CheckpointMeta, ClockReading, Terminal};
    let mut tally = OracleTally::default();
    for seed in cfg.seeds.0..=cfg.seeds.1 {
        let mut rng = Seeded(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        // The marks' wall stamps are the hand's (the sweep's clock); the
        // monotonic reading only paces the bell and mode 2026.
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
        // The terminal's started-command count as the oracle expects it.
        let mut count = 0u64;
        let mut adopt_at: Vec<usize> = Vec::new();
        let mut blocks_made = 0usize;
        // The session's first sweep comes at its spawn: losses before the
        // log's first read are out of reach (no cursor yet).
        oracle_sweep(&tl, &hand, &term, cfg.mutate, &mut seen, &mut cmds);
        for _ in 0..cfg.steps {
            let ms = seen.ms;
            let osc633 = cfg.osc633 && rng.below(2) == 0;
            // Signed marks pass the gate; unsigned or wrong ones only while
            // it is off.
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
                // A prompt (A, B): closes the running block with no D.
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
                // A bare A, and a bare B (which ends only an A's prompt).
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
                // A C in any phase: the gate takes it only at a typed prompt.
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
                // The shell's D (refused unless a command runs).
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
                // A burst between two sweeps: past the read's cap, and (giant)
                // past the terminal's store.
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
                        // Commands whose blocks the store evicted in the burst.
                        let held: HashSet<u64> = term.all_blocks().map(|b| b.id).collect();
                        tally.store_evicted += cmds
                            .iter()
                            .filter(|c| {
                                c.gone_by.is_none() && c.blk.is_some_and(|b| !held.contains(&b))
                            })
                            .count();
                    }
                }
                // ED 3 (a `clear`, or a program's) and Cmd-K: the blocks go,
                // the phase stays.
                64..=67 => feed(&mut term, b"\x1b[3J", ms),
                68..=70 => term.clear_scrollback(),
                // RIS: the blocks and the phase go.
                71..=73 => {
                    feed(&mut term, b"\x1bc", ms);
                    phase = Idle;
                }
                74 if cfg.adopt && adopt_at.len() < 3 => {
                    let cp = term.checkpoint();
                    let meta = CheckpointMeta::from_checkpoint(&cp);
                    let mut value = aterm_json::to_value(&meta).expect("the meta serializes");
                    // The shell's object is absent until a mark landed.
                    let shell = value
                        .as_object_mut()
                        .and_then(|o| o.get_mut("shell"))
                        .and_then(|s| s.as_object_mut());
                    if rng.below(2) == 0
                        && let Some(shell) = shell
                    {
                        shell.remove("started_seq");
                        count = 0;
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
                        "seed {seed}: the count crosses (an old predecessor's starts at 0)"
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
        // Read out what is held: every owed look, then more.
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

/// NO SILENT LOSS AND NO FALSE GAP, BY ORACLE ([`lost_command_oracle`]):
/// prompts, commands and Ds as OSC 133 and 633, a bare A or B, a C in any
/// phase (the gate refuses it but at a typed prompt), the nonce gate switched
/// on and off with signed, unsigned and wrong-`id=` marks, bursts past the
/// read's cap and past the terminal's 1000-block store, ED 3, Cmd-K and RIS
/// at any phase, a C after a clear took its block, and seamless updates whose
/// meta crosses as JSON, an older predecessor's with no `started_seq`. Every
/// seed of every mix: 0 false gaps, 0 missed gaps, every command read out by
/// the end, and every read command that finished on its block logged; each
/// mix covers what it means to (losses, refused C's and marks, evictions,
/// both kinds of adoption). NEGATIVE CONTROLS: the oracle sees a log told no
/// count (it falls back on the dropped ids, and says a gap for a clear that
/// took no command) and one told nothing of the drop (it says no gap for a
/// command that went unread).
#[test]
fn the_block_read_says_every_lost_command_and_nothing_else() {
    let keep: fn(&mut BlockRead) = |_| {};
    let base = OracleCfg {
        seeds: (1, 400),
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
                seeds: (1, 200),
                steps: 120,
                osc633: true,
                nonce: true,
                ..base
            },
        ),
        (
            "seamless updates",
            OracleCfg {
                seeds: (1, 200),
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

    // NEGATIVE CONTROLS: the oracle catches a false gap and a missed one.
    let no_count = lost_command_oracle(OracleCfg {
        seeds: (1, 100),
        mutate: |r| r.started = None,
        ..base
    });
    assert!(no_count.false_gaps > 0, "{no_count:?}");
    let blind = lost_command_oracle(OracleCfg {
        seeds: (1, 100),
        mutate: |r| {
            r.started = None;
            r.dropped = 0;
        },
        ..base
    });
    assert!(blind.missed_gaps > 0, "{blind:?}");
}

// ---- review round: the verbs ---------------------------------------------------

/// `log --json` IS ONE OBJECT PER LINE, its gap included (`{"op":"gap",…}`):
/// a reader behind the ring and one of another epoch both parse every body
/// line. NEGATIVE CONTROL: the text reply's gap line is not JSON.
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

/// THE `--json` CONVENTION: `--json` or bare `json` and the one `<node>` stand
/// anywhere; a second node is still refused. `brief`'s `<node>`/`all` too.
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

/// THE BRIEF'S GAP IS ITS READ'S: a brief of one node, or past a cursor, of a
/// ring that never evicted says no loss; a cursor of another epoch, or ahead of
/// the head, says `gap why=reset` as `log` does; a cursor behind the ring, and
/// the whole ring once it evicted, `gap why=evicted` with the exact count.
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
        behind.contains("\ngap why=evicted lost=2 resync_to=3\n"),
        "{behind}"
    );
    let whole = render_brief(&small, "s-x", Node::SESSION, None, 600, now);
    assert!(
        whole.contains("\ngap why=evicted lost=3 resync_to=3\n"),
        "{whole}"
    );

    // Through the verb.
    let a = test_handle(1);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(a.clone());
    let on = crate::control::cmd_brief(&a.ctx, &store, "since=1 ep=1");
    assert!(on.starts_with("OK ") && !on.contains("\ngap "), "{on}");
    let stale = crate::control::cmd_brief(&a.ctx, &store, "since=1 ep=9");
    assert!(stale.contains("\ngap why=reset "), "{stale}");
}

/// `brief all` KEEPS TO ITS BUDGET: forty sessions under the default 1000 and
/// under 100 answer within it (`used=` the whole reply's estimate), the
/// sessions that did not fit counted in `more=sessions:<k>`, and the header's
/// count is the lines that follow; a
/// budget no session fits answers the header and the count alone. NEGATIVE
/// CONTROL: two sessions under 1000 lose none.
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

    // NEGATIVE CONTROL.
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

/// THE REPLY IS ENCODED OFF THE LOCK, THROUGH THE VERBS: `log`, `log
/// --json`, `brief` and `brief all` copy what they show under each
/// timeline's leaf lock and encode after releasing it, so a probe at the
/// start of every encoding ([`ENCODE_PROBE`]) finds every session's
/// timeline lock free. NEGATIVE CONTROL: the same probe, with an encoding run
/// under the lock, finds it held. The copy renders exactly what the one-call
/// form does.
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

    // NEGATIVE CONTROL: encoding under the lock is seen.
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
