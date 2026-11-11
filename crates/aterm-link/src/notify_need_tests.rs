// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_slog::{
    Actor, Close, Conf, Fields, Kind, Log, Mark, Node, Open, Out, RunOut, Src, Stamp, Tier,
};

const FACE: &str = "/f/f1/pub/n-a/s-x/ev";

fn mirrored(log: &mut Log) -> Vec<String> {
    mirrored_at(log, Stamp::new(9, 9))
}

/// [`mirrored`], each take at `at` (the clock a moved mail count's snapshot is rated by).
fn mirrored_at(log: &mut Log, at: Stamp) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        let take = log.bus_copy(64, at).encode(Tier::Wire, &|_| None);
        if take.slots.is_empty() {
            return out;
        }
        out.extend(take.slots.iter().map(|s| s.body.clone()));
        log.land(take.slots.last().expect("a slot").at)
            .expect("lands");
        if !take.more {
            return out;
        }
    }
}

#[test]
fn a_long_session_folds_to_the_same_needs_within_the_watchs_bound() {
    let mut log = Log::new(1);
    let mut ms = 0;
    let mut at = || {
        ms += 1;
        Stamp::new(1_760_000_000_000 + ms, ms)
    };
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(),
    )
    .expect("opens");
    let mut watch = NeedWatch::new("f1");
    let mut whole = BusSpace::new(1);
    let (mut off, mut longest, mut words) = (0, 0, Vec::new());
    let mut check = |log: &mut Log, watch: &mut NeedWatch, whole: &mut BusSpace| {
        for body in mirrored(log) {
            off += 1;
            let Step::Folded {
                needs, whole: held, ..
            } = watch.step(off, FACE, body.as_bytes())
            else {
                panic!("an ev record of the session folds: {body}");
            };
            assert!(
                held,
                "@{off}: a watch that holds the session's start folds it whole"
            );
            whole.offer(aterm_slog::decode_bus(&body).expect("a mirror body"));
            let unbounded = whole.fold_seeded().0;
            let want = unbounded.state().top_observed_need().map(|n| n.word);
            assert_eq!(
                needs.first().map(|n| n.pair.word),
                want,
                "@{off}: the bounded fold names the need the whole space folds to"
            );
            if words.last() != Some(&want) {
                words.push(want);
            }
            let held = watch.held();
            assert_eq!(held.len(), 1, "one session, one space: {held:?}");
            longest = longest.max(held[0].2);
            assert!(held[0].2 <= SPACE_MAX, "@{off}: {held:?}");
        }
        let held = watch.held();
        assert!(
            held[0].2 <= SPACE_KEEP,
            "@{off}: a drained take's snapshot folds the space from it: {held:?}"
        );
    };
    let cmd = |log: &mut Log, out: RunOut, at: Stamp| {
        let c = log
            .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at)
            .expect("a command");
        log.close(c, Close::new(Out::Run(out), Src::Shell, Conf::Exact), at)
            .expect("closes");
    };
    let quiet = |log: &mut Log, n: usize, at: &mut dyn FnMut() -> Stamp| {
        for _ in 0..n {
            log.mark(
                Mark::new(Kind::Handoff, Src::Aterm, Conf::Exact)
                    .at(Node::SESSION)
                    .by(Actor::Aterm),
                at(),
            )
            .expect("a handoff mark");
        }
    };
    cmd(&mut log, RunOut::Fail, at());
    check(&mut log, &mut watch, &mut whole);
    for _ in 0..8 {
        quiet(&mut log, 50, &mut at);
        check(&mut log, &mut watch, &mut whole);
    }
    cmd(&mut log, RunOut::Ok, at());
    quiet(&mut log, 100, &mut at);
    check(&mut log, &mut watch, &mut whole);
    cmd(&mut log, RunOut::Fail, at());
    quiet(&mut log, 400, &mut at);
    check(&mut log, &mut watch, &mut whole);
    assert_eq!(
        words,
        [None, Some("failure"), None, Some("failure")],
        "the needs the session moved through"
    );
    assert!(
        whole.len() > 2 * SPACE_KEEP && longest > SPACE_KEEP,
        "the session outgrew the bound ({} records, the watch held {longest})",
        whole.len()
    );
}

/// `body` as a snapshot that says its session held more needs than it lists (the brief's
/// `snapshot-needs`), any other body as it is.
fn short(body: &str) -> String {
    if !body.contains(" op=snap ") {
        return body.to_string();
    }
    body.split(' ')
        .map(|t| if t.starts_with("n=") { "n=7" } else { t })
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn a_partial_fold_pages_no_seeded_need_and_the_bound_keeps_a_whole_view_over_a_short_snapshot() {
    let mut log = Log::new(1);
    let mut ms = 0;
    let mut at = || {
        ms += 1;
        Stamp::new(1_760_000_000_000 + ms, ms)
    };
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(),
    )
    .expect("opens");
    let cmd = log
        .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at())
        .expect("a command");
    log.close(
        cmd,
        Close::new(Out::Run(RunOut::Fail), Src::Shell, Conf::Exact),
        at(),
    )
    .expect("closes");
    let opening = mirrored(&mut log);
    let quiet = |log: &mut Log, n: usize, at: &mut dyn FnMut() -> Stamp| {
        for _ in 0..n {
            log.mark(
                Mark::new(Kind::Handoff, Src::Aterm, Conf::Exact)
                    .at(Node::SESSION)
                    .by(Actor::Aterm),
                at(),
            )
            .expect("a handoff mark");
        }
        mirrored(log)
    };
    let mut bodies = Vec::new();
    for _ in 0..6 {
        bodies.extend(quiet(&mut log, 50, &mut at));
    }
    let snaps = bodies.iter().filter(|b| b.contains(" op=snap ")).count();
    assert!(
        bodies.len() > SPACE_KEEP && bodies.len() < SPACE_MAX && snaps > 1,
        "PRECONDITION: the session outgrows the keep with snapshots, within the cap"
    );
    let bodies: Vec<String> = bodies.iter().map(|b| short(b)).collect();

    let mut late = NeedWatch::new("f1");
    let mut off = 0;
    let mut seen_snap = false;
    for body in &bodies {
        off += 1;
        seen_snap |= body.contains(" op=snap ");
        let step = late.step(off, FACE, body.as_bytes());
        let Step::Folded { needs, whole, .. } = &step else {
            panic!("@{off}: an ev record of the session folds: {step:?}");
        };
        assert!(
            !whole && needs.iter().all(|n| !n.rose),
            "@{off}, after a snapshot {seen_snap}: a watch that joined the session late \
             holds no start and only short snapshots, so it sees no need rise: {step:?}"
        );
    }

    let mut whole = NeedWatch::new("f1");
    let (mut longest, mut failed) = (0, false);
    for body in opening.iter().chain(&bodies) {
        off += 1;
        failed |= body.contains("out=fail");
        let step = whole.step(off, FACE, body.as_bytes());
        let Step::Folded {
            needs, whole: true, ..
        } = step
        else {
            panic!("@{off}: a watch that holds the session's start judges it: {step:?}");
        };
        assert_eq!(
            needs.first().map(|n| n.pair.word),
            failed.then_some("failure"),
            "@{off}: the session's failure, from its run's close on"
        );
        longest = longest.max(whole.held()[0].2);
    }
    assert!(
        longest > SPACE_KEEP,
        "the whole view was kept past the keep rather than traded for a short snapshot"
    );

    let mut log2 = log;
    log2.close_session(
        Close::new(
            Out::Session(aterm_slog::SessionOut::ShellExit),
            Src::Shell,
            Conf::Exact,
        ),
        at(),
    )
    .expect("closes");
    let mut exited = None;
    for body in mirrored(&mut log2) {
        off += 1;
        if let Step::Folded { needs, whole, .. } = late.step(off, FACE, body.as_bytes()) {
            assert!(whole, "a fold that holds the session's close is whole");
            exited = needs.first().filter(|n| n.rose).map(|n| n.pair.word);
        }
    }
    assert_eq!(
        exited,
        Some("exited"),
        "a partial fold that holds the session's close knows its one need, and saw it rise"
    );
}

#[test]
fn a_space_with_no_snapshot_to_fold_from_is_held_to_its_cap() {
    let mut log = Log::new(1);
    let at = |ms: u64| Stamp::new(1_760_000_000_000 + ms, ms);
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(0),
    )
    .expect("opens");
    let mut space = BusSpace::new(1);
    let mut ms = 0;
    while space.len() <= SPACE_MAX {
        for _ in 0..TAKE {
            ms += 1;
            log.mark(
                Mark::new(Kind::Handoff, Src::Aterm, Conf::Exact)
                    .at(Node::SESSION)
                    .by(Actor::Aterm),
                at(ms),
            )
            .expect("a handoff mark");
        }
        for body in mirrored(&mut log) {
            let b = aterm_slog::decode_bus(&body).expect("a mirror body");
            if b.rec.op != aterm_slog::Op::Snap {
                space.offer(b);
            }
        }
        assert!(space.snap().is_none(), "PRECONDITION: no snapshot held");
        if space.len() <= SPACE_MAX {
            let before = space.len();
            bound(&mut space);
            assert_eq!(
                space.len(),
                before,
                "below the cap a space keeps its records"
            );
        }
    }
    let newest = space.records().last().map(|r| r.seq);
    bound(&mut space);
    assert_eq!(space.len(), SPACE_KEEP, "past the cap, its newest records");
    assert_eq!(space.records().last().map(|r| r.seq), newest);
}

#[test]
fn a_space_whose_one_snapshot_is_older_than_its_cap_is_held_to_it() {
    let mut log = Log::new(1);
    let at = |ms: u64| Stamp::new(1_760_000_000_000 + ms, ms);
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(0),
    )
    .expect("opens");
    let mut space = BusSpace::new(1);
    let (mut ms, mut snaps, mut longest) = (0, 0, 0);
    while longest <= SPACE_MAX {
        for _ in 0..TAKE {
            ms += 1;
            log.mark(
                Mark::new(Kind::Handoff, Src::Aterm, Conf::Exact)
                    .at(Node::SESSION)
                    .by(Actor::Aterm),
                at(ms),
            )
            .expect("a handoff mark");
        }
        for body in mirrored(&mut log) {
            let b = aterm_slog::decode_bus(&body).expect("a mirror body");
            if b.rec.op == aterm_slog::Op::Snap {
                snaps += 1;
                if snaps > 1 {
                    continue;
                }
            }
            space.offer(b);
        }
        longest = longest.max(space.len());
        bound(&mut space);
        assert!(
            space.len() <= SPACE_MAX,
            "after {ms} marks the space holds {} records",
            space.len()
        );
        if ms > 10 * SPACE_MAX as u64 {
            break;
        }
    }
    assert!(
        snaps > 1 && longest > SPACE_MAX,
        "PRECONDITION: one snapshot held, and the space outgrew the cap ({longest})"
    );
}

#[test]
fn a_pair_spells_and_matches_as_the_standing_store_reads_it() {
    let at = |seq| Pair {
        word: "failure",
        raised: Raised::At { ep: 3, seq },
    };
    let by = |seq| Pair {
        word: "failure",
        raised: Raised::By { ep: 3, seq },
    };
    let other_space = Pair {
        word: "failure",
        raised: Raised::At { ep: 4, seq: 5 },
    };
    let old = Pair {
        word: "failure",
        raised: Raised::Unknown,
    };
    for p in [at(5), by(70), other_space.clone(), old.clone()] {
        assert_eq!(Pair::parse(&p.token()), Some(p.clone()), "{}", p.token());
    }
    assert_eq!(at(5).token(), "failure/3/5");
    assert_eq!(by(70).token(), "failure/3/~70");
    assert_eq!(old.token(), "failure");
    for bad in [
        "nope/3/5",
        "failure/3",
        "failure/x/5",
        "failure/3/~",
        "failure/3/5/6",
        "",
    ] {
        assert_eq!(Pair::parse(bad), None, "{bad}");
    }
    assert!(
        at(5).same(&at(5)) && !at(5).same(&at(9)),
        "one record, one need"
    );
    assert!(
        at(5).same(&by(70)) && by(70).same(&at(5)) && !at(71).same(&by(70)),
        "a snapshot's need is any the space raised at or before it"
    );
    assert!(
        by(70).same(&by(130)),
        "two snapshots of one space name one need"
    );
    assert!(
        !other_space.same(&at(5)) && !other_space.same(&by(70)),
        "another space's need is another need"
    );
    assert!(
        old.same(&at(9)) && by(1).same(&old),
        "an old store's word is any of its word"
    );
    let question = Pair {
        word: "question",
        raised: Raised::At { ep: 3, seq: 5 },
    };
    assert!(
        !question.same(&at(5)) && !question.same(&old),
        "words differ"
    );
}

const TAKE: usize = 50;

/// A session whose records the fix unit J0-3 tests mirror, on its own hand-set clock.
struct Sess {
    log: Log,
    ms: u64,
}

impl Sess {
    /// An open session, and the bodies of its first take (its start and first snapshot).
    fn open() -> (Self, Vec<String>) {
        let mut log = Log::new(1);
        log.open_session(
            Kind::Spawn,
            Src::Aterm,
            Conf::Exact,
            Actor::Aterm,
            Fields::new(),
            Stamp::new(1_760_000_000_000, 0),
        )
        .expect("opens");
        let mut s = Self { log, ms: 0 };
        let first = s.bodies();
        (s, first)
    }

    fn at(&mut self) -> Stamp {
        self.ms += 1;
        Stamp::new(1_760_000_000_000 + self.ms, self.ms)
    }

    /// `n` marks that move no need.
    fn quiet(&mut self, n: usize) {
        for _ in 0..n {
            let at = self.at();
            self.log
                .mark(
                    Mark::new(Kind::Handoff, Src::Aterm, Conf::Exact)
                        .at(Node::SESSION)
                        .by(Actor::Aterm),
                    at,
                )
                .expect("a handoff mark");
        }
    }

    /// An agent run, a turn under it and a usage wall under that, which raises
    /// `wall:usage-session`.
    fn wall(&mut self) {
        let at = self.at();
        let run = self
            .log
            .open(Open::new(Kind::Agent, Src::Status, Conf::Strong), at)
            .expect("an agent run");
        let at = self.at();
        let turn = self
            .log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                at,
            )
            .expect("a turn");
        let at = self.at();
        self.log
            .open(
                Open::new(Kind::Wall, Src::Screen, Conf::Strong)
                    .under(turn)
                    .fields(
                        Fields::new()
                            .word(Key::Wall, "usage-session")
                            .expect("wall"),
                    ),
                at,
            )
            .expect("the wall opens");
    }

    /// A failed shell command, which raises `failure`.
    fn fail(&mut self) {
        let at = self.at();
        let cmd = self
            .log
            .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at)
            .expect("a command");
        let at = self.at();
        self.log
            .close(
                cmd,
                Close::new(Out::Run(RunOut::Fail), Src::Shell, Conf::Exact),
                at,
            )
            .expect("closes");
    }

    /// Mail delivered, a local mark that moves the session's counts: the next take ends
    /// with a snapshot, whose seq is that mark's.
    fn mail(&mut self) {
        let at = self.at();
        self.log
            .mark(
                Mark::new(Kind::Mail, Src::Fabric, Conf::Exact)
                    .fields(Fields::new().word(Key::Dir, "in").expect("dir")),
                at,
            )
            .expect("mail");
    }

    /// What the mirror takes now, at the session's clock.
    fn bodies(&mut self) -> Vec<String> {
        let at = Stamp::new(1_760_000_000_000 + self.ms, self.ms);
        mirrored_at(&mut self.log, at)
    }
}

fn is_snap(body: &str) -> bool {
    body.contains(" op=snap ")
}

/// The seq of the record `body` mirrors.
fn seq_of(body: &str) -> u64 {
    aterm_slog::decode_bus(body).expect("a mirror body").rec.seq
}

/// Whether `body` is the record that raised `word` (a wall's step open, a run's failed
/// close).
fn raises(body: &str, word: &str) -> bool {
    match word {
        "wall:usage-session" => body.contains(" op=open ") && body.contains(" k=wall "),
        "failure" => body.contains(" op=close ") && body.contains(" out=fail"),
        _ => panic!("no raise known for {word}"),
    }
}

/// Step `bodies` into `watch` as `FACE`'s records from `*off + 1` on: each one's offset and
/// the need of `word` its fold names, if any.
fn judge(
    watch: &mut NeedWatch,
    off: &mut u64,
    bodies: &[String],
    word: &str,
) -> Vec<(u64, Option<Named>)> {
    bodies
        .iter()
        .map(|body| {
            *off += 1;
            let step = watch.step(*off, FACE, body.as_bytes());
            let Step::Folded { needs, .. } = step else {
                panic!("@{off}: an ev record of the session folds: {step:?}");
            };
            (*off, needs.into_iter().find(|n| n.pair.word == word))
        })
        .collect()
}

/// The offsets a notifier's standing rule pages at among `judged`: a need that rose and is
/// the same need as none paged before (`crate::notify`'s `need_hit`, rc=0 every time).
fn pages(judged: &[(u64, Option<Named>)], paged: &mut Vec<Pair>) -> Vec<u64> {
    judged
        .iter()
        .filter_map(|(off, n)| {
            let n = n.as_ref().filter(|n| n.rose)?;
            if paged.iter().any(|p| p.same(&n.pair)) {
                return None;
            }
            paged.push(n.pair.clone());
            Some(*off)
        })
        .collect()
}

/// The step `judged` took at `off`.
fn at_off(judged: &[(u64, Option<Named>)], off: u64) -> Option<&Named> {
    judged
        .iter()
        .find(|(o, _)| *o == off)
        .and_then(|(_, n)| n.as_ref())
}

/// A NEED WHOSE RAISE THE BUS LOST RISES WHERE A SNAPSHOT NAMES IT, TO A WATCH THAT READ ITS
/// SPACE LIVE (fix unit J0-3, gap G2-g6). The watch steps the session from its start; then
/// the bus loses the record that raised the need, and the snapshot after it is the first the
/// watch knows of it. Two losses: the wall's step open withheld (a hole: no `ms` of it
/// reaches the watch, and the snapshot's `upto=` says it was there), and a failure's close
/// the writer could not publish (a cover in its slot, `op=gap why=dropped n=1`). The
/// snapshot's step names the need risen, paired At that snapshot (the first record that told
/// the watch of it), and it pages once: no later record or snapshot pages it again.
#[test]
fn a_need_whose_raise_the_bus_lost_rises_once_where_a_snapshot_names_it_to_a_live_watch() {
    let (mut s, start) = Sess::open();
    let mut watch = NeedWatch::new("f1");
    let mut off = 0;
    let word = "wall:usage-session";
    let mut judged = judge(&mut watch, &mut off, &start, word);
    s.quiet(8);
    judged.extend(judge(&mut watch, &mut off, &s.bodies(), word));
    s.wall();
    s.mail();
    let take = s.bodies();
    assert!(
        take.iter().any(|b| raises(b, word)) && take.last().is_some_and(|b| is_snap(b)),
        "PRECONDITION: the wall's raise, then a snapshot naming it: {take:?}"
    );
    let lost: Vec<String> = take.into_iter().filter(|b| !raises(b, word)).collect();
    let snapped = judge(&mut watch, &mut off, &lost, word);
    let snap_off = off;
    judged.extend(snapped);
    let named = at_off(&judged, snap_off).expect("the snapshot names the wall");
    let snap_seq = seq_of(lost.last().expect("the snapshot"));
    assert!(
        named.pair.raised
            == Raised::At {
                ep: 1,
                seq: snap_seq
            }
            && named.seen
            && named.rose,
        "the hole's need, seeded by the snapshot past it, rose there: {named:?}"
    );
    s.quiet(3);
    judged.extend(judge(&mut watch, &mut off, &s.bodies(), word));
    s.ms += aterm_slog::MAIL_SNAP_GAP_MS;
    s.mail();
    s.quiet(1);
    let again = s.bodies();
    assert!(
        again.iter().any(|b| is_snap(b)),
        "PRECONDITION: a later snapshot names the wall again"
    );
    judged.extend(judge(&mut watch, &mut off, &again, word));
    assert!(
        judged
            .iter()
            .filter(|(o, _)| *o > snap_off)
            .all(|(_, n)| n.as_ref().is_some_and(|n| !n.rose)),
        "the wall stands past the snapshot and rises no more: {judged:?}"
    );
    assert_eq!(pages(&judged, &mut Vec::new()), [snap_off], "it pages once");

    let (mut s, start) = Sess::open();
    let mut watch = NeedWatch::new("f1");
    let mut judged = judge(&mut watch, &mut off, &start, "failure");
    s.quiet(4);
    judged.extend(judge(&mut watch, &mut off, &s.bodies(), "failure"));
    s.fail();
    s.mail();
    let take = s.bodies();
    assert!(
        take.iter().any(|b| raises(b, "failure")) && take.last().is_some_and(|b| is_snap(b)),
        "PRECONDITION: the failure's raise, then a snapshot naming it: {take:?}"
    );
    let covered: Vec<String> = take
        .into_iter()
        .map(|b| {
            if !raises(&b, "failure") {
                return b;
            }
            let d = aterm_slog::decode_bus(&b).expect("a mirror body");
            let ms = d.ms.expect("a record's ms");
            aterm_slog::cover_body(
                d.ep,
                d.rec.seq,
                ms,
                1,
                aterm_slog::CoverWhy::Dropped,
                d.rec.t,
            )
        })
        .collect();
    judged.extend(judge(&mut watch, &mut off, &covered, "failure"));
    let snap_off = off;
    let named = at_off(&judged, snap_off).expect("the snapshot names the failure");
    let snap_seq = seq_of(covered.last().expect("the snapshot"));
    assert!(
        named.pair.raised
            == Raised::At {
                ep: 1,
                seq: snap_seq
            }
            && named.seen
            && named.rose,
        "the cover's need, seeded by the snapshot past it, rose there: {named:?}"
    );
    s.quiet(2);
    judged.extend(judge(&mut watch, &mut off, &s.bodies(), "failure"));
    assert_eq!(
        pages(&judged, &mut Vec::new()),
        [snap_off],
        "the failure pages once"
    );
}

/// A LATE WATCH PAGES NO NEED THAT STOOD BEFORE IT READ THE SPACE WHOLE (fix unit J0-3, the
/// LOOP2-1 rule kept). (a) A watch whose warm-up folded everything below the snapshot
/// judges the snapshot first: it read nothing of the space live. (b) A watch that joined
/// after the session's start reads its records live but folds them partial, so a mail
/// snapshot (its seq the local mail mark's, past every record the watch read) naming a
/// failure raised before the watch joined is no rise. (c) Its first snapshot's whole fold
/// names that failure; a later snapshot past a record the bus lost names it again, newer
/// than anything the watch read whole, and it is still no rise: the whole fold named it.
#[test]
fn a_late_watch_pages_no_need_a_snapshot_names_that_stood_before_it_read_the_space_whole() {
    let word = "wall:usage-session";
    let (mut s, start) = Sess::open();
    s.quiet(8);
    let mut below = start;
    below.extend(s.bodies());
    s.wall();
    s.mail();
    let take = s.bodies();
    let (snap, rest): (Vec<String>, Vec<String>) = take
        .into_iter()
        .filter(|b| !raises(b, word))
        .partition(|b| is_snap(b));
    below.extend(rest);
    let mut warmed = NeedWatch::new("f1");
    for (i, b) in below.iter().enumerate() {
        warmed.warm(i as u64 + 1, FACE, b.as_bytes());
    }
    let mut off = below.len() as u64;
    let judged = judge(&mut warmed, &mut off, &snap, word);
    let named = judged[0].1.as_ref().expect("the snapshot names the wall");
    assert!(
        matches!(named.pair.raised, Raised::By { .. }) && !named.rose,
        "(a) a snapshot the watch reads first raises nothing where it could see: {named:?}"
    );

    let (mut s, withheld) = Sess::open();
    assert!(
        withheld.iter().any(|b| is_snap(b)),
        "PRECONDITION: the late watch never gets the session's start or first snapshot"
    );
    s.fail();
    let before = s.bodies();
    assert!(
        before.iter().any(|b| raises(b, "failure")) && !before.iter().any(|b| is_snap(b)),
        "PRECONDITION: the failure is raised before the watch joins, no snapshot with it"
    );
    let mut late = NeedWatch::new("f1");
    let mut off = 0;
    s.quiet(4);
    let mut judged = judge(&mut late, &mut off, &s.bodies(), "failure");
    assert!(
        judged.iter().all(|(_, n)| n.is_none()),
        "PRECONDITION: the partial fold of what the watch joined at names no failure"
    );
    s.mail();
    let snapped = s.bodies();
    assert!(
        snapped.len() == 1 && is_snap(&snapped[0]),
        "PRECONDITION: the mail's snapshot, alone"
    );
    let first = judge(&mut late, &mut off, &snapped, "failure");
    let named = first[0].1.as_ref().expect("the snapshot names the failure");
    assert!(
        matches!(named.pair.raised, Raised::By { .. }) && !named.rose,
        "(b) a failure older than the watch's whole view raises nothing: {named:?}"
    );
    judged.extend(first);
    s.quiet(2);
    let gapped = s.bodies();
    assert_eq!(gapped.len(), 2, "PRECONDITION: two records");
    judged.extend(judge(&mut late, &mut off, &gapped[1..], "failure"));
    s.ms += aterm_slog::MAIL_SNAP_GAP_MS;
    s.mail();
    let again = s.bodies();
    assert!(
        again.len() == 1 && is_snap(&again[0]),
        "PRECONDITION: the next mail's snapshot, alone"
    );
    let second = judge(&mut late, &mut off, &again, "failure");
    let named = second[0]
        .1
        .as_ref()
        .expect("the snapshot names the failure");
    assert!(
        matches!(named.pair.raised, Raised::By { .. }) && !named.rose,
        "(c) the failure the watch's whole fold named raises nothing again: {named:?}"
    );
    judged.extend(second);
    assert_eq!(pages(&judged, &mut Vec::new()), Vec::<u64>::new());
}

/// A NEED THE WATCH SAW RISE PAGES ONCE ACROSS THE BOUND'S REBUILD (fix unit J0-3): the watch
/// holds the failure's raise live and it rises there; the session then outgrows
/// [`SPACE_KEEP`] in one burst, whose closing snapshot the bound refolds the space from, so
/// the failure is seeded by that snapshot. The watch's whole fold named it, so it rises no
/// more, and pages once.
#[test]
fn a_need_the_watch_saw_rise_pages_once_across_the_bounds_rebuild_from_a_snapshot() {
    let (mut s, start) = Sess::open();
    let mut watch = NeedWatch::new("f1");
    let mut off = 0;
    let mut judged = judge(&mut watch, &mut off, &start, "failure");
    s.fail();
    let raised = s.bodies();
    judged.extend(judge(&mut watch, &mut off, &raised, "failure"));
    let rose_at = off;
    let named = at_off(&judged, rose_at).expect("the failure");
    assert!(
        matches!(named.pair.raised, Raised::At { .. }) && named.rose,
        "PRECONDITION: the failure rose where the watch held its raise: {named:?}"
    );
    s.quiet(SPACE_KEEP + 50);
    let burst = s.bodies();
    assert!(
        burst.iter().filter(|b| is_snap(b)).count() == 1
            && burst.last().is_some_and(|b| is_snap(b)),
        "PRECONDITION: one snapshot, at the burst's end"
    );
    judged.extend(judge(&mut watch, &mut off, &burst, "failure"));
    let named = at_off(&judged, off).expect("the rebuilt fold names the failure");
    assert!(
        matches!(named.pair.raised, Raised::By { .. }) && !named.rose,
        "the rebuilt space's snapshot seeds the failure the watch's whole fold named: \
         {named:?}"
    );
    assert!(
        watch.held()[0].2 <= SPACE_KEEP,
        "PRECONDITION: the bound refolded the space from the snapshot"
    );
    assert_eq!(pages(&judged, &mut Vec::new()), [rose_at], "it pages once");
}

/// WHAT A WATCH READ LIVE GOES WITH ITS SPACE AND ITS SESSION (fix unit J0-3): a session's
/// new seq space drops its old space's mark, even before the watch reads the new one whole,
/// and past [`fabric_brief::SESSIONS_MAX`] sessions the evicted one's mark goes with it.
#[test]
fn a_watchs_live_marks_go_with_their_space_and_their_session() {
    let (_, start) = Sess::open();
    let mut watch = NeedWatch::new("f1");
    let mut off = 0;
    judge(&mut watch, &mut off, &start, "failure");
    assert_eq!(
        watch.live_eps(),
        [1],
        "PRECONDITION: the first space's mark"
    );
    let mut next = Log::new(2);
    next.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        Stamp::new(1_760_000_100_000, 0),
    )
    .expect("opens");
    mirrored(&mut next);
    next.mark(
        Mark::new(Kind::Handoff, Src::Aterm, Conf::Exact)
            .at(Node::SESSION)
            .by(Actor::Aterm),
        Stamp::new(1_760_000_100_001, 1),
    )
    .expect("a handoff mark");
    let partial = mirrored(&mut next);
    assert!(
        partial.len() == 1 && partial[0].contains(" ep=2 "),
        "PRECONDITION: one record of the session's next space, past its start"
    );
    let judged = judge(&mut watch, &mut off, &partial, "failure");
    assert!(judged[0].1.is_none(), "PRECONDITION: no need");
    assert_eq!(
        watch.live_eps(),
        Vec::<u32>::new(),
        "the old space's mark went with it, and the new space's partial fold sets none"
    );
    for i in 0..fabric_brief::SESSIONS_MAX {
        off += 1;
        let face = format!("/f/f1/pub/n-b/s-{i}/ev");
        watch.step(off, &face, start[0].as_bytes());
    }
    assert!(
        !watch
            .read
            .sessions
            .contains_key(&("n-a".into(), "s-x".into())),
        "PRECONDITION: the stalest session was evicted"
    );
    off += 1;
    watch.step(off, "/f/f1/pub/n-b/s-last/ev", start[0].as_bytes());
    assert_eq!(
        watch.live_eps().len(),
        fabric_brief::SESSIONS_MAX,
        "one mark per session held, the evicted ones' gone"
    );
}

/// A WATCH THAT LOST A RAISE BEFORE IT READ THE SPACE LIVE PAGES NO SNAPSHOT NAMING IT (fix
/// unit J0-3's lander, the LOOP2-1 rule kept): the wall's raise is a hole in what the
/// warm-up folded, below the watch's cursor, so the wall stood before the watch started. The
/// watch then reads a quiet record live, and the next snapshot names the wall: the live
/// record's fold lost the raise, so it is no whole view the wall was missing from, and the
/// snapshot's wall is no rise. Two warm-ups: (a) one that holds the session's start (its
/// fold `Seed::Start`, with the hole), (b) one that holds only the session's first snapshot
/// and records past it (`Seed::Snap`, the hole past that snapshot's `upto=`).
#[test]
fn a_watch_that_lost_a_raise_before_it_read_the_space_live_pages_no_snapshot_naming_it() {
    let word = "wall:usage-session";
    for from_snap in [false, true] {
        let (mut s, start) = Sess::open();
        let mut below: Vec<String> = if from_snap {
            start.into_iter().filter(|b| is_snap(b)).collect()
        } else {
            start
        };
        assert_eq!(
            below.iter().any(|b| !is_snap(b)),
            !from_snap,
            "PRECONDITION: the warm-up holds the session's start, or only its first snapshot"
        );
        s.quiet(4);
        below.extend(s.bodies());
        s.wall();
        let raised = s.bodies();
        assert!(
            raised.iter().any(|b| raises(b, word)) && !raised.iter().any(|b| is_snap(b)),
            "PRECONDITION: the wall's raise, with no snapshot: {raised:?}"
        );
        below.extend(raised.into_iter().filter(|b| !raises(b, word)));
        let mut watch = NeedWatch::new("f1");
        for (i, b) in below.iter().enumerate() {
            watch.warm(i as u64 + 1, FACE, b.as_bytes());
        }
        let mut off = below.len() as u64;
        s.quiet(1);
        let mut judged = judge(&mut watch, &mut off, &s.bodies(), word);
        assert!(
            judged.iter().all(|(_, n)| n.is_none()),
            "PRECONDITION: the live record's fold lost the wall's raise"
        );
        s.mail();
        let snapped = s.bodies();
        assert!(
            snapped.len() == 1 && is_snap(&snapped[0]),
            "PRECONDITION: the mail's snapshot, alone"
        );
        judged.extend(judge(&mut watch, &mut off, &snapped, word));
        let named = at_off(&judged, off).expect("the snapshot names the wall");
        assert!(
            matches!(named.pair.raised, Raised::By { .. }) && named.seen && !named.rose,
            "from its snapshot {from_snap}: a wall lost before the watch started stood \
             then, and did not rise: {named:?}"
        );
        s.quiet(2);
        judged.extend(judge(&mut watch, &mut off, &s.bodies(), word));
        assert_eq!(
            pages(&judged, &mut Vec::new()),
            Vec::<u64>::new(),
            "from its snapshot {from_snap}: nothing pages"
        );
    }
}

/// A SHORT SNAPSHOT PAGES NO NEED WHOSE RAISE THE BUS LOST, AND THE NEXT WHOLE ONE PAGES IT
/// ONCE (fix unit J0-3's lander): a live watch loses the wall's raise (a hole), and the
/// snapshot past it says its session held more needs than it lists (the brief's
/// `snapshot-needs`). A partial fold pages no seeded need: the wall is not seen there and
/// does not rise, nor at the records after it. The next mail's snapshot is whole: the wall
/// rises there, paired At it, and no record after it pages it again.
#[test]
fn a_short_snapshot_pages_no_need_whose_raise_the_bus_lost_and_the_next_whole_one_pages_it_once() {
    let word = "wall:usage-session";
    let (mut s, start) = Sess::open();
    let mut watch = NeedWatch::new("f1");
    let mut off = 0;
    let mut judged = judge(&mut watch, &mut off, &start, word);
    s.quiet(8);
    judged.extend(judge(&mut watch, &mut off, &s.bodies(), word));
    s.wall();
    s.mail();
    let take = s.bodies();
    assert!(
        take.iter().any(|b| raises(b, word)) && take.last().is_some_and(|b| is_snap(b)),
        "PRECONDITION: the wall's raise, then a snapshot naming it: {take:?}"
    );
    let lost: Vec<String> = take
        .iter()
        .filter(|b| !raises(b, word))
        .map(|b| short(b))
        .collect();
    judged.extend(judge(&mut watch, &mut off, &lost, word));
    let short_off = off;
    let named = at_off(&judged, short_off).expect("the short snapshot names the wall");
    assert!(
        matches!(named.pair.raised, Raised::By { .. }) && !named.seen && !named.rose,
        "a short snapshot's fold is partial, and pages no seeded need: {named:?}"
    );
    s.quiet(2);
    judged.extend(judge(&mut watch, &mut off, &s.bodies(), word));
    s.ms += aterm_slog::MAIL_SNAP_GAP_MS;
    s.mail();
    let again = s.bodies();
    assert!(
        again.len() == 1 && is_snap(&again[0]),
        "PRECONDITION: the next mail's snapshot, alone, and whole: {again:?}"
    );
    judged.extend(judge(&mut watch, &mut off, &again, word));
    let whole_off = off;
    let named = at_off(&judged, whole_off).expect("the whole snapshot names the wall");
    assert!(
        named.pair.raised
            == Raised::At {
                ep: 1,
                seq: seq_of(&again[0])
            }
            && named.seen
            && named.rose,
        "the whole snapshot is the first the watch can trust of the wall: {named:?}"
    );
    s.quiet(2);
    judged.extend(judge(&mut watch, &mut off, &s.bodies(), word));
    assert!(
        judged
            .iter()
            .filter(|(o, _)| *o != whole_off)
            .all(|(_, n)| n.as_ref().is_none_or(|n| !n.rose)),
        "the wall rose at the whole snapshot only: {judged:?}"
    );
    assert_eq!(
        pages(&judged, &mut Vec::new()),
        [whole_off],
        "it pages once, at the whole snapshot"
    );
}
