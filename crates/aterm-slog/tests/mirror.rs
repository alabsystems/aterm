// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bus mirror's engine half (design §5.5, phase 4): the bus class and
//! its `ms`, the take and its covers, the landing and the fence, the carry
//! across an update, snapshots, aliases, and the reader's view
//! (`aterm_slog::bus`). Each ruling's named test is here, against the real
//! `Log`.

use aterm_slog::bus::{BusSpace, Offered};
use aterm_slog::{
    Actor, BusCursor, CarryAdopt, CarryBudget, Close, Conf, ENTRY_BYTES, Fields, Fold, Key, Kind,
    LandRefused, Landed, Limits, Log, Lvl, Mark, Node, Op, Open, Out, Prior, Rec, RunOut,
    SessionOut, ShortId, SlotKind, Src, StepOut, Tier, TurnOut, decode_bus, encode_mirror,
};
use aterm_spec::rng::XorShift;

use crate::support::{agent_session, at};

/// No alias: a sid is written `by=-`.
fn no_alias(_: &ShortId) -> Option<ShortId> {
    None
}

/// The test's alias: a fixed function of the sid, `e` and eight hex digits,
/// as the host's keyed hash shapes it.
fn alias(sid: &ShortId) -> Option<ShortId> {
    let h = sid.as_str().bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    ShortId::new(&format!("e{h:08x}"))
}

/// The bus fold's own actors are already aliased.
fn same(sid: &ShortId) -> Option<ShortId> {
    Some(*sid)
}

/// One take, encoded, offered to `view` and landed.
fn take_into(log: &mut Log, view: &mut BusSpace, room: usize, ms: u64) -> usize {
    let take = log.bus_copy(room, at(ms)).encode(Tier::WireMeta, &alias);
    let n = take.slots.len();
    for s in &take.slots {
        let b = decode_bus(&s.body).expect("a bus body decodes");
        view.offer(b);
    }
    if let Some(last) = take.slots.last() {
        log.land(last.at).expect("lands");
    }
    n
}

/// Take until nothing is left.
fn drain(log: &mut Log, view: &mut BusSpace, ms: u64) {
    for _ in 0..10_000 {
        let before = log.mirror_landed();
        let n = take_into(log, view, 32, ms);
        if n == 0 || (log.mirror_settled() && before == log.mirror_landed()) {
            return;
        }
    }
    panic!("the take never settled");
}

fn cmd(log: &mut Log, ms: u64, ok: bool) -> Node {
    let c = log
        .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(ms))
        .expect("cmd opens");
    let out = if ok { RunOut::Ok } else { RunOut::Fail };
    log.close(
        c,
        Close::new(Out::Run(out), Src::Shell, Conf::Exact),
        at(ms + 1),
    )
    .expect("cmd closes");
    c
}

fn bell(log: &mut Log, ms: u64) {
    log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(ms))
        .expect("a bell");
}

fn session(log: &mut Log) {
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(0),
    )
    .expect("the session opens");
}

/// RULING Q5: the bus class is its own predicate. The turn class is what it
/// was; the bus class holds it, the need-bearing steps and `mark seen`, and
/// nothing else.
#[test]
fn the_bus_class_is_its_own_predicate_and_holds_the_turn_class() {
    let ops = [Op::Open, Op::Close, Op::Amend, Op::Mark, Op::Gap, Op::Snap];
    let nodes = [
        Node::SESSION,
        Node::new(Lvl::Run, 1),
        Node::new(Lvl::Turn, 1),
        Node::new(Lvl::Step, 1),
    ];
    let mut bus_only = std::collections::BTreeSet::new();
    for op in ops {
        for node in nodes {
            for k in Kind::ALL {
                if !k.fits(op, node.lvl) {
                    continue;
                }
                let r = Rec {
                    seq: 1,
                    t: 0,
                    op,
                    node,
                    par: None,
                    k: *k,
                    src: Src::Aterm,
                    conf: Conf::Exact,
                    by: Actor::Aterm,
                    f: Fields::new(),
                };
                if r.is_turn_class() {
                    assert!(r.is_bus_class(), "{r:?}: the turn class is bus class");
                } else if r.is_bus_class() {
                    bus_only.insert(format!("{} {}", op.word(), k.word()));
                }
            }
        }
    }
    let want: std::collections::BTreeSet<String> = [
        "open approval",
        "close approval",
        "amend approval",
        "open question",
        "close question",
        "amend question",
        "open wall",
        "close wall",
        "amend wall",
        "open stall",
        "close stall",
        "amend stall",
        "mark seen",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(bus_only, want, "the bus class past the turn class");
}

/// RULING Q5: `ms` costs exactly its 8 bytes a record in heap, mirror on or
/// off, and numbers the bus class densely; a local record has none.
#[test]
fn ms_costs_eight_bytes_a_record_and_numbers_the_bus_class_densely() {
    assert_eq!(
        ENTRY_BYTES,
        std::mem::size_of::<Rec>() + std::mem::size_of::<usize>() + std::mem::size_of::<u64>(),
        "an entry is its record, its bytes and its ms"
    );
    let mut log = Log::new(1);
    session(&mut log);
    let empty = log.footprint();
    for i in 0..10 {
        bell(&mut log, 1 + i);
    }
    let per = (log.footprint() - empty) / 10;
    assert_eq!(per, ENTRY_BYTES, "a fieldless mark costs exactly one entry");
    let c = cmd(&mut log, 20, true);
    assert_eq!(log.ms_head(), 3, "the session's open and the command's two");
    let ms: Vec<(u64, u64)> = log
        .window()
        .map(|r| (r.seq, log.ms_of(r.seq).expect("live")))
        .collect();
    assert_eq!(ms[0].1, 1, "the session's open is ms 1");
    assert!(ms[1..11].iter().all(|(_, m)| *m == 0), "a bell is local");
    assert_eq!(&ms[11..], &[(12, 2), (13, 3)], "{c}");
    assert_eq!(log.mirror_landed(), None, "nothing mirrors until a take");
}

/// RULING Q5 (the carry golden): a log no bridge ever took from carries the
/// bytes a build without phase 4 wrote, its `ms` included in nothing.
#[test]
fn an_unmirrored_carry_is_the_bytes_it_was() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong)
                .under(run)
                .by(Actor::Human),
            at(10),
        )
        .expect("a turn");
    let f = Fields::new().word(Key::Box, "bash").expect("box");
    let step = log
        .open(
            Open::new(Kind::Approval, Src::Screen, Conf::Strong)
                .under(turn)
                .fields(f),
            at(20),
        )
        .expect("an approval");
    log.close(
        step,
        Close::new(Out::Step(StepOut::Approved), Src::Screen, Conf::Strong).by(Actor::Human),
        at(30),
    )
    .expect("approved");
    bell(&mut log, 40);
    let out = log
        .carry(CarryBudget::SESSION, &["journal v=1 head=3".to_string()])
        .expect("carries");
    // The bytes the phase-3A build (no phase 4) writes for this log.
    let golden: Vec<String> = [
        "slog-carry v=1 ep=1 base=1 first=1 last=6 lost=0 newest_lost=0 ids=1,1,1 carried=6 shed=0 lines=25",
        "sess kind=spawn opened=1 closed=0 t=1760000000000 roll=1,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0",
        "cpar p1 t1",
        "fold v=1 as_of=6 ep=1 rev=6 last_t=1760000000040 mail_open=0",
        "fold phase word=- src=unknown conf=unknown t=0",
        "fold totals turns=1 done=0 asked=0 walled=0 interrupted=0 errors=0 other=0 steps=1 appr=1 appr_by=1,0,0,0 qs=0 stalls=0 tools=0,0,0,0,0,0,0,0,0,0 mail_in=0 mail_out=0 human=0 lost=0",
        "fold open r1 agent par=- by=- src=status conf=strong t=1760000000000",
        "fold f t0 agent strong 2 claude",
        "fold open t1 turn par=r1 by=human src=screen conf=strong t=1760000000010",
        "fold last 2 p1 approval out=approved dur_ms=10 opened_t=1760000000020 t=1760000000030 by=human opened_by=- src=screen conf=strong",
        "fold f l2 box strong 4 bash",
        "fold f l2 out strong 5 approved",
        "fold f l2 dur_ms strong 5 10",
        "host journal v=1 head=3",
        "node - par=- open=1 below=0",
        "node r1 par=- open=1 below=0",
        "node t1 par=r1 open=1 below=0",
        "open r1 kind=agent par=- t=1760000000000 orphan=0 roll=1,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0",
        "open t1 kind=turn par=r1 t=1760000000010 orphan=0 roll=1,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0",
        "r 1 open - spawn by=aterm src=aterm conf=exact t=1760000000000",
        "r 2 open r1 agent par=- agent=claude src=status conf=strong t=1760000000000",
        "r 3 open t1 turn par=r1 by=human src=screen conf=strong t=1760000000010",
        "r 4 open p1 approval par=t1 box=bash src=screen conf=strong t=1760000000020",
        "r 5 close p1 approval out=approved dur_ms=10 by=human src=screen conf=strong t=1760000000030",
        "r 6 mark t1 bell src=engine conf=exact t=1760000000040",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    assert_eq!(out.lines, golden, "the carry of an unmirrored log");
}

fn adopt(log: &Log, ms: u64) -> CarryAdopt {
    CarryAdopt {
        ep: log.ep() + 1,
        next_seq: log.last_seq() + 1000,
        ids: log.id_counters(),
        limits: Limits::SESSION,
        at: at(ms),
    }
}

/// RULING Q7: a phase-4 successor adopts the carry a 2A writer made and the
/// carry a 2B writer made — no `ms=`, `r` lines only, an update's overlap
/// hole, and (2B) the journal's host line: accepted, its bus class numbered
/// densely from 1 in seq order over the carried records, `ms`'s head their
/// count, and no cursor; its first take then covers nothing.
#[test]
fn a_phase4_successor_adopts_a_2a_carry_and_a_2b_carry() {
    let small = Limits::new(16, 1 << 20).expect("a small ring");
    let mut log = Log::with_limits(1, small);
    let run = agent_session(&mut log, 0);
    for i in 0..6 {
        cmd(&mut log, 10 + 10 * i, i % 3 != 0);
    }
    log.skip_to(log.last_seq() + 10);
    for i in 0..12 {
        bell(&mut log, 200 + i);
    }
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(300),
        )
        .expect("a turn");
    assert!(log.lost() > 0, "PRECONDITION: the ring evicted");
    let a2 = log.carry(CarryBudget::SESSION, &[]).expect("carries").lines;
    let b2 = log
        .carry(
            CarryBudget::SESSION,
            &[format!("journal v=1 head={}", log.last_seq())],
        )
        .expect("carries")
        .lines;
    for (who, carry) in [("2A", a2), ("2B", b2)] {
        assert!(
            !carry[0].contains(" ms=") && carry.iter().all(|l| !l.starts_with("R ")),
            "{who}: the shape an older writer wrote"
        );
        assert!(
            carry.iter().any(|l| l.starts_with("hole ")),
            "{who}: an overlap"
        );
        let carried_bus: Vec<u64> = carry
            .iter()
            .filter_map(|l| l.strip_prefix("r "))
            .map(|l| aterm_slog::decode(l).expect("a record"))
            .filter(Rec::is_bus_class)
            .map(|r| r.seq)
            .collect();
        let (mut next, _, _) =
            Log::from_carry(&carry, adopt(&log, 400)).unwrap_or_else(|e| panic!("{who}: {e:?}"));
        assert_eq!(next.mirror_landed(), None, "{who}: no cursor crossed");
        assert!(next.is_open(turn), "{who}");
        let ms: Vec<(u64, u64)> = next
            .window()
            .filter(|r| r.is_bus_class())
            .map(|r| (r.seq, next.ms_of(r.seq).expect("live")))
            .collect();
        let mut want: Vec<(u64, u64)> = carried_bus
            .iter()
            .enumerate()
            .map(|(i, s)| (*s, i as u64 + 1))
            .collect();
        want.push((next.last_seq(), carried_bus.len() as u64 + 1));
        assert_eq!(
            ms, want,
            "{who}: dense from 1, then the successor's handoff mark"
        );
        assert_eq!(next.ms_head(), carried_bus.len() as u64 + 1, "{who}");
        let mut view = BusSpace::new(next.base_ep());
        drain(&mut next, &mut view, 500);
        assert!(view.covers().is_empty(), "{who}: {:?}", view.covers());
        assert!(view.holes().is_empty(), "{who}");
    }
}

/// The cursor and the `ms` of a mirrored log cross the carry: the successor
/// takes on from where the export's cursor stood, numbering on from the
/// carried head, and the cut's shed bus-class records are one exact cover.
#[test]
fn the_landed_cursor_and_ms_cross_the_carry() {
    let mut log = Log::new(1);
    session(&mut log);
    for i in 0..5 {
        cmd(&mut log, 10 * (i + 1), true);
    }
    let mut view = BusSpace::new(1);
    let took = take_into(&mut log, &mut view, 4, 100);
    assert_eq!(took, 4);
    let landed = log.mirror_landed().expect("engaged");
    assert_eq!(landed.ms, 4);
    for i in 0..3 {
        cmd(&mut log, 200 + 10 * i, false);
    }
    let out = log.carry(CarryBudget::SESSION, &[]).expect("carries");
    assert!(
        out.lines[0].contains(&format!(" ms=17 mirror={} ", landed.word())),
        "{}",
        out.lines[0]
    );
    assert!(out.lines.iter().any(|l| l.starts_with("R 5 ")), "{out:#?}");
    let (mut next, _, _) = Log::from_carry(&out.lines, adopt(&log, 300)).expect("reads");
    assert_eq!(next.mirror_landed(), Some(landed));
    assert_eq!(next.ms_head(), 18, "the carried head and the handoff mark");
    drain(&mut next, &mut view, 400);
    assert!(view.holes().is_empty() && view.covers().is_empty());
    assert_eq!(view.max_ms(), 18);
    assert_eq!(view.dup, 0);
}

/// RULINGS Q2 and Q15: a take derives every loss from `ms` alone. One cover
/// per hole, each exact, the covers' counts summing to the bus-class
/// records evicted; a loss of local-class records only is no cover at all.
#[test]
fn a_take_says_each_hole_once_with_its_exact_count() {
    let small = Limits::new(12, 1 << 20).expect("a small ring");
    let mut log = Log::with_limits(1, small);
    session(&mut log);
    for i in 0..40 {
        bell(&mut log, 1 + i);
    }
    let mut view = BusSpace::new(1);
    drain(&mut log, &mut view, 100);
    assert!(view.covers().is_empty(), "bells only: no bus-class loss");
    let before = log.mirror_landed().expect("engaged").ms;
    // Commands while nothing takes: their opens go first (closed runs).
    for i in 0..30 {
        cmd(&mut log, 1000 + 10 * i, true);
    }
    let live: Vec<u64> = log.live_ms().filter(|ms| *ms > before).collect();
    let evicted = (log.ms_head() - before) - live.len() as u64;
    assert!(
        evicted > 0,
        "PRECONDITION: the ring evicted bus-class records"
    );
    let mut holes = 0;
    let mut prev = before;
    for ms in live
        .iter()
        .copied()
        .chain(std::iter::once(log.ms_head() + 1))
    {
        if ms > prev + 1 {
            holes += 1;
        }
        prev = ms;
    }
    drain(&mut log, &mut view, 2000);
    let covers = view.covers();
    assert_eq!(covers.len(), holes, "one cover per hole: {covers:?}");
    assert_eq!(
        covers.iter().map(|c| c.1).sum::<u64>(),
        evicted,
        "the covers' n= sum to the evicted bus-class count"
    );
    assert!(covers.iter().all(|c| c.2 == "evicted"));
    assert!(view.holes().is_empty(), "ms dense in the reader's view");
    assert_eq!(view.max_ms(), log.ms_head());
}

/// RULINGS Q2 and Q12: under an update's fence a take stops at the fence's
/// seq and its tail cover at the fence's `ms` head, and no landing moves the
/// cursor the export drew; once the fence lifts, the take goes on.
#[test]
fn a_take_under_the_fence_stops_at_it_and_no_landing_passes_it() {
    let small = Limits::new(10, 1 << 20).expect("a small ring");
    let mut log = Log::with_limits(1, small);
    session(&mut log);
    cmd(&mut log, 10, true);
    let mut view = BusSpace::new(1);
    drain(&mut log, &mut view, 20);
    let drawn = log.mirror_landed().expect("engaged");
    let _carry = log.carry_snapshot();
    log.fence_reads();
    let fh = log.ms_head();
    let fence = log.reader_last_seq();
    for i in 0..20 {
        cmd(&mut log, 100 + 10 * i, true);
    }
    assert!(
        log.ms_head() > fh,
        "PRECONDITION: the overlap wrote bus class"
    );
    let take = log.bus_copy(64, at(500)).encode(Tier::Wire, &no_alias);
    for s in &take.slots {
        assert!(s.at.seq <= fence && s.at.ms <= fh, "{s:?} past the fence");
    }
    assert_eq!(
        log.land(take.slots.last().map_or(drawn, |s| s.at)),
        Err(LandRefused::Fenced),
        "the fence refuses every landing"
    );
    assert_eq!(log.mirror_landed(), Some(drawn), "nothing moved");
    log.lift_read_fence();
    drain(&mut log, &mut view, 600);
    assert!(view.holes().is_empty(), "{:?}", view.holes());
    assert_eq!(view.max_ms(), log.ms_head());
}

/// A landing is monotone, names its space, and never passes what the log
/// numbered.
#[test]
fn a_landing_is_monotone_and_never_ahead() {
    let mut log = Log::new(1);
    session(&mut log);
    cmd(&mut log, 10, true);
    let c = BusCursor::new(1, 3, 3);
    assert_eq!(log.land(c), Ok(Landed::Moved));
    assert_eq!(log.land(c), Ok(Landed::Stale));
    assert_eq!(log.land(BusCursor::new(1, 2, 2)), Ok(Landed::Stale));
    assert_eq!(
        log.land(BusCursor::new(9, 9, 9)),
        Ok(Landed::Stale),
        "another space"
    );
    assert_eq!(log.land(BusCursor::new(1, 9, 4)), Err(LandRefused::Ahead));
    assert_eq!(log.mirror_landed(), Some(c));
}

/// RULING S17: a snapshot's `open=` and `needs=` carry only bus-class nodes.
/// Taken while a tool step is open beneath the turn (and beneath an open
/// approval), the snap names the run, the turn and the approval; a bus fold
/// seeded from it holds the open path and needs the local fold's bus facts
/// say, and the records after it apply in place.
#[test]
fn a_bus_fold_seeds_from_a_snap_taken_while_a_local_step_is_open() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(10),
        )
        .expect("a turn");
    let f = Fields::new().word(Key::Box, "bash").expect("box");
    let appr = log
        .open(
            Open::new(Kind::Approval, Src::Screen, Conf::Strong)
                .under(turn)
                .fields(f),
            at(20),
        )
        .expect("an approval");
    let tool = log
        .open(
            Open::new(Kind::Tool, Src::Transcript, Conf::Exact)
                .under(appr)
                .fields(Fields::new().word(Key::Cls, "shell").expect("cls")),
            at(30),
        )
        .expect("a tool step");
    assert_eq!(log.state().open[2].as_ref().map(|o| o.node), Some(tool));
    let take = log.bus_copy(64, at(40)).encode(Tier::WireMeta, &alias);
    let snap = take
        .slots
        .iter()
        .find(|s| s.kind == SlotKind::Snap)
        .expect("the space's first take snapshots");
    assert!(
        snap.body
            .contains(&format!(" open={run}:agent,{turn}:turn,{appr}:approval ")),
        "{}",
        snap.body
    );
    assert!(!snap.body.contains(&format!("{tool}:")), "{}", snap.body);
    assert!(snap.body.contains(" needs=approval:bash."), "{}", snap.body);
    // A reader that joins at the snap: nothing before it.
    let mut late = Fold::new(0);
    assert!(late.seed_snap(&decode_bus(&snap.body).expect("decodes").rec));
    assert_eq!(late.bus_facts(&same), log.fold().bus_facts(&alias));
    // The records after it apply in place: the approval closes, its need goes.
    let lands = take.slots.last().expect("slots").at;
    log.land(lands).expect("lands");
    log.close(
        tool,
        Close::new(Out::Step(StepOut::Ok), Src::Transcript, Conf::Exact),
        at(50),
    )
    .expect("tool closes");
    log.close(
        appr,
        Close::new(Out::Step(StepOut::Approved), Src::Screen, Conf::Strong).by(Actor::Human),
        at(60),
    )
    .expect("approved");
    let after = log.bus_copy(64, at(70)).encode(Tier::WireMeta, &alias);
    for s in &after.slots {
        late.apply(&decode_bus(&s.body).expect("decodes").rec);
    }
    assert_eq!(late.bus_facts(&same), log.fold().bus_facts(&alias));
    assert!(late.state().needs.iter().all(Option::is_none));
}

/// RULING Q6: an edge's actor crosses as its stable alias, never its sid,
/// so two edges' needs and clears leave the same needs on the bus as here.
#[test]
fn two_edges_needs_and_clears_agree_locally_and_on_the_bus() {
    let mut log = Log::new(1);
    session(&mut log);
    let a = Actor::Sid(ShortId::new("s-aaaaaaaaaaaaaaaaaaaa").expect("sid"));
    let b = Actor::Sid(ShortId::new("s-bbbbbbbbbbbbbbbbbbbb").expect("sid"));
    let need = |log: &mut Log, by: Actor, kind: Kind, ms: u64| {
        log.mark(
            Mark::new(kind, Src::Mark, Conf::Claim)
                .by(by)
                .fields(Fields::new().word(Key::Word, "help").expect("word")),
            at(ms),
        )
        .expect("a claim");
    };
    need(&mut log, a, Kind::Need, 1);
    need(&mut log, b, Kind::Need, 2);
    need(&mut log, a, Kind::Clear, 3);
    let mut view = BusSpace::new(1);
    let take = log.bus_copy(64, at(4)).encode(Tier::WireMeta, &alias);
    for s in &take.slots {
        assert!(
            !s.body.contains("s-aaaa") && !s.body.contains("s-bbbb"),
            "a sid on the bus: {}",
            s.body
        );
        view.offer(decode_bus(&s.body).expect("decodes"));
    }
    let local = log.fold().bus_facts(&alias);
    assert_eq!(local.needs.len(), 1, "b's need stands, a's is lifted");
    let (bus, partial) = view.fold();
    assert!(!partial);
    assert_eq!(bus.bus_facts(&same), local, "the same needs on the bus");
    // NEGATIVE CONTROL: one alias for every edge (the sid dropped to `-`)
    // and a's clear lifts b's need on the bus.
    let mut again = Log::new(1);
    session(&mut again);
    need(&mut again, a, Kind::Need, 1);
    need(&mut again, b, Kind::Need, 2);
    need(&mut again, a, Kind::Clear, 3);
    let mut flat = BusSpace::new(1);
    for s in &again
        .bus_copy(64, at(4))
        .encode(Tier::WireMeta, &no_alias)
        .slots
    {
        flat.offer(decode_bus(&s.body).expect("decodes"));
    }
    let (flat, _) = flat.fold();
    assert!(
        flat.state().needs.iter().all(Option::is_none),
        "CONTROL: without aliases a's clear lifted b's need on the bus"
    );
}

/// RULING Q5: `mark seen` is bus class, so a remote fold lifts
/// `need=failure` as the local one does.
#[test]
fn seen_lifts_failure_on_the_bus() {
    let mut log = Log::new(1);
    session(&mut log);
    cmd(&mut log, 10, false);
    assert!(
        log.state().needs[0]
            .as_ref()
            .is_some_and(|n| n.word == "failure")
    );
    log.mark(
        Mark::new(Kind::Seen, Src::Status, Conf::Strong).by(Actor::Human),
        at(20),
    )
    .expect("seen");
    let mut view = BusSpace::new(1);
    drain(&mut log, &mut view, 30);
    let (bus, _) = view.fold();
    assert!(bus.state().needs.iter().all(Option::is_none));
    assert_eq!(bus.bus_facts(&same), log.fold().bus_facts(&alias));
}

/// The second wall: the mirror's encoder refuses a record of the local
/// class, and no planted mail mark's party, kind, inbox or offset, nor any
/// host or content key, reaches a body a take makes.
#[test]
fn encode_mirror_refuses_local_class_and_no_host_key_crosses() {
    let mut log = Log::new(1);
    session(&mut log);
    let mail = Fields::new()
        .word(Key::Dir, "in")
        .and_then(|f| f.word(Key::MailKind, "task"))
        .and_then(|f| f.u(Key::Inbox, 7))
        .and_then(|f| f.text(Key::From, "h-person@n-1"))
        .and_then(|f| f.u(Key::Off, 88231))
        .expect("mail fields");
    let seq = log
        .mark(
            Mark::new(Kind::Mail, Src::Fabric, Conf::Exact).fields(mail),
            at(1),
        )
        .expect("a mail mark");
    let rec = log.window().find(|r| r.seq == seq).expect("live").clone();
    assert_eq!(
        encode_mirror(&rec, Tier::WireMeta, 1, Some(1), &alias),
        Err(aterm_slog::EncodeError::NotBusClass)
    );
    let run = log
        .open(
            Open::new(Kind::Agent, Src::Status, Conf::Strong).fields(
                Fields::new()
                    .text(Key::Cwd, "/Users//someone/secret")
                    .expect("cwd"),
            ),
            at(2),
        )
        .expect("run");
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong)
                .under(run)
                .fields(Fields::new().cause("@88231").expect("cause")),
            at(3),
        )
        .expect("turn");
    log.mark(
        Mark::new(Kind::Title, Src::Engine, Conf::Exact)
            .fields(Fields::new().text(Key::Title, "TOP SECRET").expect("t")),
        at(4),
    )
    .expect("title");
    log.close(
        turn,
        Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
        at(5),
    )
    .expect("closes");
    let take = log.bus_copy(64, at(6)).encode(Tier::WireMeta, &alias);
    assert!(take.slots.iter().any(|s| s.body.contains(" cause_k=mail")));
    for s in &take.slots {
        for leak in [
            " kind=", " dir=", " inbox=", " from=", " to=", " off=", " cause=", " re=", " cwd=",
            " title=", "secret", "SECRET", "88231",
        ] {
            assert!(!s.body.contains(leak), "{leak} crossed: {}", s.body);
        }
    }
}

/// RULING Q7, the uncarried path: the successor's first take says the old
/// space's tail as one exact `why=reset` cover in that space, then takes its
/// own from its first record; a landing in the old space moves only the
/// old space's owed tail.
#[test]
fn an_uncarried_update_says_the_old_spaces_tail_as_one_reset_cover() {
    let mut log = Log::after_handoff(
        7,
        Limits::SESSION,
        aterm_slog::IdCounters::default(),
        500,
        at(0),
    );
    log.note_prior_space(Prior {
        landed: BusCursor::new(3, 120, 40),
        ms_head: 52,
    });
    // A late landing of the old space's pinned slots moves its tail first.
    assert_eq!(log.land(BusCursor::new(3, 130, 45)), Ok(Landed::Moved));
    let take = log.bus_copy(64, at(10)).encode(Tier::Wire, &no_alias);
    let first = decode_bus(&take.slots[0].body).expect("decodes");
    assert_eq!((first.ep, first.ms), (3, Some(46)));
    assert_eq!(first.rec.f.get_u(Key::N), Some(7), "52 - 45, exact");
    assert_eq!(first.rec.f.get_word(Key::Why), Some("reset"));
    let second = decode_bus(&take.slots[1].body).expect("decodes");
    assert_eq!((second.ep, second.ms, second.rec.seq), (7, Some(1), 500));
    // Landed as the bridge lands a batch: each space's LAST cursor (its
    // `landed_pairs`), never slot by slot.
    let mut last: Vec<BusCursor> = Vec::new();
    for s in &take.slots {
        match last.iter_mut().find(|c| c.ep == s.at.ep) {
            Some(c) => *c = s.at,
            None => last.push(s.at),
        }
    }
    assert_eq!(
        last.len(),
        2,
        "the old space's cover and the new space's records"
    );
    for at in last {
        assert_eq!(log.land(at), Ok(Landed::Moved));
    }
    assert_eq!(log.mirror_prior(), None, "the old space is said");
    let again = log.bus_copy(64, at(20)).encode(Tier::Wire, &no_alias);
    assert!(
        again.slots.iter().all(|s| !s.body.contains(" why=reset ")),
        "the reset cover is not taken again: {:?}",
        again.slots
    );
}

/// RULING Q4's engine half: a closed session's bus tail is its bus-class
/// records past the cursor, its close among them, and a take of it settles
/// once its close has landed.
#[test]
fn a_closed_sessions_bus_tail_holds_its_close() {
    let mut log = Log::new(1);
    session(&mut log);
    cmd(&mut log, 10, true);
    let mut view = BusSpace::new(1);
    take_into(&mut log, &mut view, 2, 20);
    for i in 0..30 {
        bell(&mut log, 30 + i);
    }
    log.close_session(
        Close::new(Out::Session(SessionOut::ShellExit), Src::Aterm, Conf::Exact),
        at(100),
    )
    .expect("closes");
    let mut tail = log.bus_tail();
    assert!(
        tail.len() <= 3,
        "only the bus class past the cursor: {}",
        tail.len()
    );
    assert!(!tail.mirror_settled());
    drain(&mut tail, &mut view, 200);
    assert!(tail.mirror_settled());
    let (bus, _) = view.fold();
    assert_eq!(bus.bus_totals().closed, Some("shell-exit"));
    assert!(view.holes().is_empty());
}

/// RULING Q0: the reader's view is the law. A seeded session mirrored with
/// planted duplicates, whole batches offered twice and a batch offered in
/// reverse (a re-pinned slot landing late) shows each record once, `ms`
/// dense, and folds to the local fold's bus facts and totals.
#[test]
fn bus_fold_equals_local_bus_view() {
    for seed in 1..=40u64 {
        let mut rng = XorShift::new(seed);
        let mut log = Log::new(1);
        let run = agent_session(&mut log, 0);
        let mut view = BusSpace::new(1);
        let mut turn: Option<Node> = None;
        let mut step: Option<Node> = None;
        let mut offered = 0u64;
        for i in 0..200u64 {
            let ms = 10 + i * 10;
            match rng.below(9) {
                0 if turn.is_none() => {
                    turn = log
                        .open(
                            Open::new(Kind::Turn, Src::Screen, Conf::Strong)
                                .under(run)
                                .by(Actor::Human),
                            at(ms),
                        )
                        .ok();
                }
                1 if turn.is_some() && step.is_none() => {
                    let t = turn.take().expect("open");
                    let out = [TurnOut::Done, TurnOut::Asked, TurnOut::Interrupted][rng.index(3)];
                    let _ = log.close(
                        t,
                        Close::new(Out::Turn(out), Src::Screen, Conf::Strong),
                        at(ms),
                    );
                }
                2 if turn.is_some() && step.is_none() => {
                    let kind = [Kind::Approval, Kind::Question, Kind::Tool][rng.index(3)];
                    let f = if kind == Kind::Approval {
                        Fields::new().word(Key::Box, "edit").expect("box")
                    } else {
                        Fields::new()
                    };
                    step = log
                        .open(
                            Open::new(kind, Src::Screen, Conf::Strong)
                                .under(turn.expect("open"))
                                .fields(f),
                            at(ms),
                        )
                        .ok();
                }
                3 if step.is_some() => {
                    let by = [Actor::Human, Actor::Harness, Actor::Ctl][rng.index(3)];
                    let _ = log.close(
                        step.take().expect("open"),
                        Close::new(Out::Step(StepOut::Passed), Src::Screen, Conf::Strong).by(by),
                        at(ms),
                    );
                }
                4 => {
                    let _ = cmd(&mut log, ms, rng.below(3) != 0);
                }
                5 => {
                    let by = [
                        Actor::Harness,
                        Actor::Sid(ShortId::new("s-1111111111").expect("sid")),
                        Actor::Sid(ShortId::new("s-2222222222").expect("sid")),
                    ][rng.index(3)];
                    let k = if rng.below(2) == 0 {
                        Kind::Need
                    } else {
                        Kind::Clear
                    };
                    let _ = log.mark(
                        Mark::new(k, Src::Mark, Conf::Claim)
                            .by(by)
                            .fields(Fields::new().word(Key::Word, "review").expect("w")),
                        at(ms),
                    );
                }
                6 => {
                    let _ = log.mark(
                        Mark::new(Kind::Seen, Src::Status, Conf::Strong).by(Actor::Human),
                        at(ms),
                    );
                }
                7 => bell(&mut log, ms),
                _ => {
                    // A take: planted duplicates and a reversed batch.
                    let take = log
                        .bus_copy(1 + rng.index(16), at(ms))
                        .encode(Tier::WireMeta, &alias);
                    let mut bodies: Vec<&str> =
                        take.slots.iter().map(|s| s.body.as_str()).collect();
                    if rng.below(4) == 0 {
                        bodies.reverse();
                    }
                    for b in &bodies {
                        view.offer(decode_bus(b).expect("decodes"));
                        offered += 1;
                    }
                    if rng.below(3) == 0 {
                        for b in &bodies {
                            assert_ne!(
                                view.offer(decode_bus(b).expect("decodes")),
                                Offered::Record,
                                "a second copy is never a record"
                            );
                        }
                    }
                    if let Some(last) = take.slots.last() {
                        log.land(last.at).expect("lands");
                    }
                }
            }
        }
        drain(&mut log, &mut view, 10_000);
        assert!(offered > 0);
        assert!(view.holes().is_empty(), "seed {seed}: {:?}", view.holes());
        assert_eq!(view.max_ms(), log.ms_head(), "seed {seed}");
        let bus_count = log.window().filter(|r| r.is_bus_class()).count();
        assert_eq!(view.len(), bus_count, "seed {seed}: each record once");
        let (bus, partial) = view.fold();
        assert!(!partial, "seed {seed}");
        assert_eq!(
            bus.bus_facts(&same),
            log.fold().bus_facts(&alias),
            "seed {seed}: facts"
        );
        assert_eq!(
            bus.bus_totals(),
            log.fold().bus_totals(),
            "seed {seed}: totals"
        );
    }
}

/// RULING Q4 UNDER A FENCE (fixer F1): a session that closes while an
/// update's export holds its readers' fence is judged against its LIVE `ms`
/// head, not the fence's: its tail is owed even when the cursor stands at
/// the fence. The tail carries the fence (no landing passes the export's
/// draw), and once the host lifts it the take reaches the close.
#[test]
fn a_session_closing_under_the_fence_owes_its_tail_past_the_fence() {
    let mut log = Log::new(1);
    session(&mut log);
    cmd(&mut log, 10, true);
    let mut view = BusSpace::new(1);
    drain(&mut log, &mut view, 20);
    log.fence_reads();
    cmd(&mut log, 30, false);
    log.close_session(
        Close::new(Out::Session(SessionOut::ShellExit), Src::Aterm, Conf::Exact),
        at(40),
    )
    .expect("closes");
    assert!(log.mirror_settled(), "the reader's head is the fence's");
    assert!(
        !log.mirror_complete(),
        "the live head is past the cursor: the close is owed"
    );
    let mut tail = log.bus_tail();
    assert_eq!(tail.read_fence(), log.read_fence(), "the export's fence");
    assert_eq!(
        tail.land(BusCursor::new(1, tail.last_seq(), tail.ms_head())),
        Err(LandRefused::Fenced)
    );
    tail.lift_read_fence();
    drain(&mut tail, &mut view, 50);
    assert!(tail.mirror_complete());
    let (bus, seed) = view.fold_seeded();
    assert_eq!(bus.bus_totals().closed, Some("shell-exit"), "{seed:?}");
    assert!(view.holes().is_empty());
}

/// A LOG NO TAKE EVER REACHED (fixer F2): its bus tail is engaged from its
/// first record, so its take says everything the ring no longer holds as one
/// exact cover, then its live records and its close.
#[test]
fn an_unengaged_logs_bus_tail_is_whole_from_its_first_record() {
    let mut log = Log::with_limits(1, Limits::new(8, 1 << 20).expect("a ring"));
    session(&mut log);
    for i in 0..6 {
        cmd(&mut log, 10 + 10 * i, true);
    }
    log.close_session(
        Close::new(Out::Session(SessionOut::ShellExit), Src::Aterm, Conf::Exact),
        at(100),
    )
    .expect("closes");
    assert_eq!(log.mirror_landed(), None, "PRECONDITION: never taken from");
    let mut tail = log.bus_tail();
    assert_eq!(
        tail.mirror_landed(),
        Some(BusCursor::new(1, 0, 0)),
        "engaged at the space's start"
    );
    let mut view = BusSpace::new(1);
    drain(&mut tail, &mut view, 200);
    assert!(view.holes().is_empty(), "{:?}", view.holes());
    assert_eq!(view.max_ms(), log.ms_head(), "every ms said");
    assert!(
        view.covered() > 0,
        "PRECONDITION: the ring evicted bus-class records"
    );
    let (bus, _) = view.fold();
    assert_eq!(bus.bus_totals().closed, Some("shell-exit"));
}

/// A VIEW THAT LOST RECORDS SEEDS FROM A SNAPSHOT PAST THEM (fixer F5): a
/// claim evicted before the first take is no record of the view, but the
/// take's snapshot folds it (`upto=` past the cover); a view whose cover no
/// snapshot it holds reaches is `lost`, never whole.
#[test]
fn a_view_holding_its_start_and_a_cover_seeds_from_the_snap_past_it() {
    let mut log = Log::with_limits(1, Limits::new(8, 1 << 20).expect("a ring"));
    session(&mut log);
    log.mark(
        Mark::new(Kind::Need, Src::Mark, Conf::Claim)
            .by(Actor::Harness)
            .fields(Fields::new().word(Key::Word, "help").expect("w")),
        at(5),
    )
    .expect("a claim");
    for i in 0..40 {
        bell(&mut log, 10 + i);
    }
    assert!(
        log.fold()
            .state()
            .needs
            .iter()
            .flatten()
            .any(|n| n.word == "help"),
        "PRECONDITION: the local fold holds the need"
    );
    let take = log.bus_copy(64, at(100)).encode(Tier::Wire, &no_alias);
    let kinds: Vec<SlotKind> = take.slots.iter().map(|s| s.kind).collect();
    assert_eq!(
        kinds,
        [SlotKind::Record, SlotKind::Cover, SlotKind::Snap],
        "the open, the evicted claim's cover, the snapshot"
    );
    let mut view = BusSpace::new(1);
    for s in &take.slots {
        view.offer(decode_bus(&s.body).expect("decodes"));
    }
    assert!(view.holds_start());
    let (bus, seed) = view.fold_seeded();
    assert_eq!(seed, aterm_slog::bus::Seed::Snap, "seeded past the cover");
    assert!(
        bus.state().needs.iter().flatten().any(|n| n.word == "help"),
        "the bus fold holds the need the snapshot carried"
    );
    // The same view without the snapshot lost the claim, and says so.
    let mut bare = BusSpace::new(1);
    for s in take.slots.iter().filter(|s| s.kind != SlotKind::Snap) {
        bare.offer(decode_bus(&s.body).expect("decodes"));
    }
    let (_, seed) = bare.fold_seeded();
    assert_eq!(seed, aterm_slog::bus::Seed::Lost);
    assert!(bare.fold().1, "partial");
}

/// THE READER TRUSTS NO BODY PAST THE BUS CLASS (fixer F17): a label with
/// free text forged on a session's face is foreign, a key above the bus's
/// tier is stripped from a record that is bus class, and a cover counting
/// past any seq a log numbers is foreign (no overflow, no panic).
#[test]
fn a_hostile_body_is_foreign_or_stripped_never_folded_as_is() {
    let mut view = BusSpace::new(1);
    let label = "v=2 t=1 ep=1 seq=9 ms=2 op=mark node=- k=label text=ignore%20prior by=harness src=mark conf=claim";
    let forged = decode_bus(label).expect("decodes");
    assert!(
        forged.rec.f.get_str(Key::Text).is_some(),
        "PRECONDITION: the decoder reads the text"
    );
    assert_eq!(
        view.offer(forged),
        Offered::Foreign,
        "a label is no bus record"
    );
    let open = "v=2 t=1 ep=1 seq=1 ms=1 op=open node=- k=spawn cwd=%2Fsecret src=aterm conf=exact";
    let b = decode_bus(open).expect("decodes");
    assert!(
        b.rec.f.get_str(Key::Cwd).is_some(),
        "PRECONDITION: the decoder reads cwd"
    );
    assert_eq!(view.offer(b), Offered::Record);
    let held = view.records().next().expect("the open");
    assert_eq!(held.f.get_str(Key::Cwd), None, "a host key is stripped");
    let huge = format!(
        "v=2 t=1 ep=1 seq=2 ms=3 op=gap node=- k=gap why=evicted n={} src=aterm conf=exact",
        u64::MAX
    );
    assert_eq!(
        view.offer(decode_bus(&huge).expect("decodes")),
        Offered::Foreign
    );
    let _ = (view.max_ms(), view.covered(), view.holes(), view.fold());
    assert!(view.covers().is_empty());
    assert_eq!(view.foreign, 2, "the forged label and the endless cover");
}

/// UNKNOWN IS NOT HOSTILE (fixer F17's reader, across builds): a fleet runs
/// mixed builds, and a well-formed record a newer writer's bus class holds
/// but this build calls local (a label here) is foreign and never folded —
/// yet its `ms` is HELD, so the view stays dense and `holes()` names no loss
/// nobody said. So is one in a newer writer's WORDS (§3.4: a new word never
/// changes `v`): a kind, an operation or a node level this build does not
/// know. A second body of its key is a duplicate; an `ms` past any seq a log
/// numbers is still dropped whole.
#[test]
fn a_record_outside_this_builds_bus_class_holds_its_ms_and_is_never_folded() {
    let mut view = BusSpace::new(1);
    let open = "v=2 t=1 ep=1 seq=1 ms=1 op=open node=- k=spawn src=aterm conf=exact";
    let label = "v=2 t=2 ep=1 seq=2 ms=2 op=mark node=- k=label text=build%20ok by=harness src=mark conf=claim";
    // A newer writer's words: a mark kind, an operation, a node level.
    let newer = [
        (
            3,
            "v=2 t=3 ep=1 seq=3 ms=3 op=mark node=- k=zzznew word=help src=mark conf=claim",
        ),
        (
            4,
            "v=2 t=4 ep=1 seq=4 ms=4 op=zzzop node=t1 k=need word=help src=mark conf=claim",
        ),
        (
            5,
            "v=2 t=5 ep=1 seq=5 ms=5 op=open node=q1 par=- k=turn src=aterm conf=exact",
        ),
    ];
    let seen = "v=2 t=6 ep=1 seq=6 ms=6 op=mark node=- k=seen src=status conf=strong";
    assert_eq!(
        view.offer(decode_bus(open).expect("decodes")),
        Offered::Record
    );
    assert_eq!(
        view.offer(decode_bus(label).expect("decodes")),
        Offered::Foreign,
        "a label is no bus record of this build's"
    );
    for (n, body) in newer {
        let b = decode_bus(body).unwrap_or_else(|e| panic!("a newer word is read past: {e:?}"));
        assert_eq!(
            (b.ep, b.rec.seq, b.ms),
            (1, n, Some(n)),
            "its place: {body}"
        );
        assert!(!b.rec.is_bus_class(), "never folded: {body}");
        assert_eq!(view.offer(b), Offered::Foreign, "{body}");
    }
    assert_eq!(
        view.offer(decode_bus(seen).expect("decodes")),
        Offered::Record
    );
    assert_eq!(
        view.holes(),
        Vec::<(u64, u64)>::new(),
        "every ms outside this build's class is held: no hole nobody said"
    );
    assert_eq!(view.foreign, 4);
    assert_eq!(view.max_ms(), 6);
    assert!(view.holds_start());
    assert!(
        view.records()
            .all(|r| r.k != Kind::Label && r.f.get_str(Key::Text).is_none()),
        "nothing of the label is folded"
    );
    assert_eq!(
        view.records().map(|r| r.seq).collect::<Vec<_>>(),
        [1, 6],
        "nothing of the newer words is folded"
    );
    let (bus, seed) = view.fold_seeded();
    assert_eq!(
        seed,
        aterm_slog::bus::Seed::Start,
        "whole: nothing was lost"
    );
    assert!(
        bus.state().needs.iter().flatten().next().is_none(),
        "no newer word's claim is raised"
    );
    assert_eq!(
        view.offer(decode_bus(label).expect("decodes")),
        Offered::Duplicate
    );
    assert_eq!(
        view.offer(decode_bus(newer[1].1).expect("decodes")),
        Offered::Duplicate
    );
    assert_eq!((view.dup, view.foreign), (2, 4));
    let far = format!(
        "v=2 t=7 ep=1 seq=7 ms={} op=mark node=- k=label src=mark conf=claim",
        u64::MAX
    );
    assert_eq!(
        view.offer(decode_bus(&far).expect("decodes")),
        Offered::Foreign
    );
    assert_eq!(view.max_ms(), 6, "an ms past any seq is never held");
}

/// A SNAPSHOT COUNTS ONCE IT LANDS (fixer F3's retake): a take the bridge
/// dropped unlanded (its session's face refused by the subject budget) owed
/// its snapshot, so the retake — after the ring evicted the claim — holds
/// one again, and the view that lost the claim seeds from it with the need
/// held. A take that holds only a snapshot lands at the cursor it stood at,
/// and once a snapshot has landed none is owed until the next is due.
#[test]
fn a_dropped_takes_snapshot_is_owed_again_and_a_landed_one_is_not() {
    let mut log = Log::with_limits(1, Limits::new(8, 1 << 20).expect("a ring"));
    session(&mut log);
    log.mark(
        Mark::new(Kind::Need, Src::Mark, Conf::Claim)
            .by(Actor::Harness)
            .fields(Fields::new().word(Key::Word, "help").expect("w")),
        at(5),
    )
    .expect("a claim");
    let kinds = |take: &aterm_slog::Take| take.slots.iter().map(|s| s.kind).collect::<Vec<_>>();
    let first = log.bus_copy(64, at(6)).encode(Tier::Wire, &no_alias);
    assert_eq!(
        kinds(&first),
        [SlotKind::Record, SlotKind::Record, SlotKind::Snap],
        "the open, the claim, the snapshot"
    );
    // Dropped unlanded; the ring evicts the claim meanwhile.
    for i in 0..40 {
        bell(&mut log, 10 + i);
    }
    assert!(
        !log.live_ms().any(|m| m == 2),
        "PRECONDITION: the claim is evicted"
    );
    let retake = log.bus_copy(64, at(100)).encode(Tier::Wire, &no_alias);
    assert_eq!(
        kinds(&retake).last(),
        Some(&SlotKind::Snap),
        "the retake owes the snapshot: {:?}",
        kinds(&retake)
    );
    let mut view = BusSpace::new(1);
    for s in &retake.slots {
        view.offer(decode_bus(&s.body).expect("decodes"));
    }
    assert!(
        !view.covers().is_empty(),
        "PRECONDITION: the view lost the claim"
    );
    let (bus, seed) = view.fold_seeded();
    assert_eq!(seed, aterm_slog::bus::Seed::Snap, "seeded past the cover");
    assert!(
        bus.state().needs.iter().flatten().any(|n| n.word == "help"),
        "the need is held"
    );
    log.land(retake.slots.last().expect("a slot").at)
        .expect("lands");
    assert!(
        log.bus_copy(64, at(101)).is_empty(),
        "a landed snapshot is not owed again"
    );
    // A take that filled its room at the head leaves the snapshot to the
    // next, which stands at the landed cursor.
    let mut log = Log::new(1);
    session(&mut log);
    let full = log.bus_copy(1, at(1)).encode(Tier::Wire, &no_alias);
    assert_eq!(kinds(&full), [SlotKind::Record]);
    log.land(full.slots[0].at).expect("lands");
    let snap = log.bus_copy(64, at(2)).encode(Tier::Wire, &no_alias);
    assert_eq!(kinds(&snap), [SlotKind::Snap]);
    assert_eq!(log.land(snap.slots[0].at), Ok(Landed::Stale));
    assert!(
        log.bus_copy(64, at(3)).is_empty(),
        "its landing counted it: no snapshot owed"
    );
}

/// A RETAKE WITHOUT ROOM FOR THE SNAPSHOT STILL OWES IT (fixer F3's retake,
/// a thin bucket): a take the bridge dropped unlanded handed out a snapshot;
/// the retake's room (the bridge's bucket) holds only the open and the
/// cover of the evicted claim, so it hands out none. Its landing passes the
/// dropped snapshot's cursor, yet counts no snapshot: none reached the bus.
/// The next take holds one, and the view that lost the claim seeds from it
/// with the need held.
#[test]
fn a_retake_with_no_room_for_the_snapshot_still_owes_it() {
    let mut log = Log::with_limits(1, Limits::new(8, 1 << 20).expect("a ring"));
    session(&mut log);
    log.mark(
        Mark::new(Kind::Need, Src::Mark, Conf::Claim)
            .by(Actor::Harness)
            .fields(Fields::new().word(Key::Word, "help").expect("w")),
        at(5),
    )
    .expect("a claim");
    let kinds = |take: &aterm_slog::Take| take.slots.iter().map(|s| s.kind).collect::<Vec<_>>();
    let dropped = log.bus_copy(64, at(6)).encode(Tier::Wire, &no_alias);
    assert_eq!(
        kinds(&dropped).last(),
        Some(&SlotKind::Snap),
        "PRECONDITION: the dropped take handed out a snapshot"
    );
    for i in 0..40 {
        bell(&mut log, 10 + i);
    }
    assert!(
        !log.live_ms().any(|m| m == 2),
        "PRECONDITION: the claim is evicted"
    );
    let small = log.bus_copy(2, at(100)).encode(Tier::Wire, &no_alias);
    assert_eq!(
        kinds(&small),
        [SlotKind::Record, SlotKind::Cover],
        "PRECONDITION: the open and the claim's cover fill the room"
    );
    let mut view = BusSpace::new(1);
    for s in &small.slots {
        view.offer(decode_bus(&s.body).expect("decodes"));
    }
    let last = small.slots.last().expect("a slot").at;
    let snap_at = dropped.slots.last().expect("the snapshot").at;
    assert!(
        (last.ms, last.seq) >= (snap_at.ms, snap_at.seq),
        "PRECONDITION: the landing passes the dropped snapshot's cursor"
    );
    log.land(last).expect("lands");
    assert_eq!(
        log.mirror_snap_ms(),
        0,
        "no snapshot reached the bus, so none is counted"
    );
    drain(&mut log, &mut view, 101);
    assert!(
        !view.covers().is_empty(),
        "PRECONDITION: the view lost the claim"
    );
    let (bus, seed) = view.fold_seeded();
    assert_eq!(seed, aterm_slog::bus::Seed::Snap, "seeded past the cover");
    assert!(
        bus.state().needs.iter().flatten().any(|n| n.word == "help"),
        "the need is held"
    );
}

/// A CARRY ACROSS A CHANGE OF THE BUS CLASS (fixer F9): a mirrored carry
/// whose writer numbered a record this build calls local, and left one this
/// build calls bus class unnumbered, is adopted, not refused: its numbers
/// are the writer's.
#[test]
fn a_mirrored_carry_is_read_whatever_the_writers_bus_class() {
    let mut log = Log::new(1);
    session(&mut log);
    cmd(&mut log, 10, true);
    let mut view = BusSpace::new(1);
    take_into(&mut log, &mut view, 1, 20);
    bell(&mut log, 30);
    let lines = log.carry(CarryBudget::SESSION, &[]).expect("carries").lines;
    // The writer's class differs: its bell carried an `ms` (`R`), and its
    // command's open none (`r`).
    let head_ms = log.ms_head();
    let mut skewed: Vec<String> = Vec::new();
    let mut moved = false;
    for l in &lines {
        if let Some(rest) = l.strip_prefix("r ").filter(|r| r.contains(" bell ")) {
            skewed.push(format!("R {} {rest}", head_ms + 1));
            moved = true;
        } else if let Some(rest) = l.strip_prefix("R 2 ") {
            skewed.push(format!("r {rest}"));
        } else {
            skewed.push(l.clone());
        }
    }
    assert!(moved, "PRECONDITION: the bell crossed: {lines:#?}");
    let skewed: Vec<String> = skewed
        .into_iter()
        .map(|l| {
            if l.starts_with("slog-carry ") {
                l.replace(&format!(" ms={head_ms} "), &format!(" ms={} ", head_ms + 1))
            } else {
                l
            }
        })
        .collect();
    let (mut next, _, _) = Log::from_carry(&skewed, adopt(&log, 40))
        .unwrap_or_else(|e| panic!("refused: {e:?}: {skewed:#?}"));
    drain(&mut next, &mut view, 50);
    assert!(view.holes().is_empty(), "{:?}", view.holes());
}

/// WHICH SIDS CROSS (fixer F18, for D4): a restored session's open and its
/// `amend - restore` carry `prev=` and `lineage=` (and a spawned one's open
/// `parent=`) as sids, unaliased; no other key of theirs carries one.
#[test]
fn a_restored_sessions_lineage_sids_reach_the_bus() {
    let a = "s-aaaaaaaaaaaaaaaaaaaa";
    let b = "s-bbbbbbbbbbbbbbbbbbbb";
    let c = "s-cccccccccccccccccccc";
    let mut log = Log::new(1);
    let f = Fields::new()
        .id(Key::Prev, a)
        .and_then(|f| f.id(Key::Lineage, b))
        .and_then(|f| f.id(Key::Parent, c))
        .expect("fields");
    log.open_session(Kind::Spawn, Src::Aterm, Conf::Exact, Actor::Aterm, f, at(0))
        .expect("opens");
    let f = Fields::new()
        .id(Key::Prev, a)
        .and_then(|f| f.id(Key::Lineage, b))
        .expect("fields");
    log.amend(
        Node::SESSION,
        aterm_slog::Amend::new(Kind::Restore, Src::Aterm, Conf::Exact, f),
        at(1),
    )
    .expect("amends");
    let take = log.bus_copy(64, at(2)).encode(Tier::WireMeta, &no_alias);
    let mut keys = std::collections::BTreeSet::new();
    for s in &take.slots {
        for tok in s.body.split(' ') {
            if let Some((k, v)) = tok.split_once('=')
                && v.starts_with("s-")
            {
                keys.insert(k.to_string());
            }
        }
    }
    let want: std::collections::BTreeSet<String> = ["lineage", "parent", "prev"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(keys, want, "the sid-valued keys on the bus: {take:#?}");
}
