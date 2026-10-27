// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm link notify --on need[:<word>]`: each session's need as `aterm fabric brief` folds it
//! from the session log's bus mirror (design §5.5, §7.1).
//!
//! The watch keeps the brief's own read ([`crate::fabric_brief::Read`], built by
//! [`Read::following`]) and folds a session's newest seq space with the brief's
//! [`crate::fabric_brief::newest_fold`] after each of its `ev` records: there is no second parser
//! of `ev` bodies and no second fold. What the notifier does with the need it names (fire on a
//! rise, stand on a repeat) is [`crate::notify`]'s standing rule, the one `attention` follows.

use aterm_slog::bus::BusSpace;
use aterm_slog::{BusRec, Key};

use crate::fabric_brief::{self, Read};
use crate::transport::{self, Conn};

/// The records of a session's newest space a watch keeps before it folds them from their
/// newest snapshot on: four snapshots' worth ([`aterm_slog::SNAP_EVERY`]).
pub const SPACE_KEEP: usize = 4 * aterm_slog::SNAP_EVERY as usize;

/// A space past this many records with no snapshot to fold from within this many keeps only
/// its newest [`SPACE_KEEP`] (its fold then starts there, as a brief's does past its window,
/// and is partial until the next snapshot). The mirror sends its snapshot when a take drains
/// what the session wrote, and what it has not sent is held in the log's ring
/// ([`aterm_slog::MAX_RECORDS`]) or said lost by a cover the next snapshot folds past, so a
/// snapshot comes within two rings unless the session outwrites its bridge for that long.
pub const SPACE_MAX: usize = 2 * aterm_slog::MAX_RECORDS;

/// The records below the cursor a watch folds before it judges any (the brief's own window,
/// [`fabric_brief::WINDOW_DEFAULT`]).
pub const WARM_WINDOW: u64 = fabric_brief::WINDOW_DEFAULT;

/// The need a session's fold names: its word (one of [`aterm_slog::vocab::NEED_WORDS`]) and the
/// detail the brief shows beside it (a wall's reset, a failure's exit code).
pub type Folded = (&'static str, Option<String>);

/// What one record did to the needs a [`NeedWatch`] holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Not a face the watch reads, or a body it could not place.
    Nothing,
    /// One of the session's `ev` records: its face (the `ev` subject, which the standing rule is
    /// keyed by), its node and sid, and the need its fold names now (`None`: none, or the
    /// session is gone without its close, [`gone_unclosed`]).
    Folded {
        face: String,
        node: String,
        sid: String,
        need: Option<Folded>,
    },
    /// One of the session's `ev` records whose fold is partial ([`aterm_slog::bus::Seed::partial`]:
    /// the watch holds neither the session's start nor a snapshot that seeds every need, or it
    /// lost records no snapshot folds past) and holds no close of the session: the watch does
    /// not know the session's need, so the record neither pages nor clears one. Its face.
    Partial(String),
    /// A presence row says these sessions are gone without their close: each one's face.
    Gone(Vec<String>),
}

/// Whether the session is gone although its newest fold holds no close of it: its node's row
/// says it is gone, or its own row says it exited, before its close reached the bus
/// ([`fabric_brief::gone_settled`], the brief's `partial` words for the same case). Nobody
/// there can answer its need. A session whose close the fold holds is not: its close raised
/// `exited` (§7.1) and dropped every other need, whichever of its close and its presence row
/// the bus carried first.
fn gone_unclosed(read: &Read, node: &str, s: &fabric_brief::Session) -> bool {
    fabric_brief::gone_settled(read, node, s).is_some()
        && fabric_brief::newest_fold(s).is_none_or(|(_, _, fold, _)| fold.state().closed.is_none())
}

/// The sessions a `need` selector follows.
pub struct NeedWatch {
    fleet: String,
    read: Read,
}

impl NeedWatch {
    /// An empty watch of `fleet`'s faces.
    #[must_use]
    pub fn new(fleet: &str) -> Self {
        Self {
            fleet: fleet.to_string(),
            read: Read::following(),
        }
    }

    /// The `(node, sid, leaf)` of a session or node face this watch reads: `/f/<F>/pub/<node>/
    /// <sid>/ev` or `…/presence`, its node a principal and its sid a session's (`s-…`) or the
    /// node's own (`node`).
    fn face<'a>(&self, subject: &'a str) -> Option<(&'a str, &'a str, &'static str)> {
        let principal = |node: &str, sid: &str| {
            crate::subject::is_principal(node)
                && (sid == "node" || (sid.starts_with("s-") && crate::subject::is_principal(sid)))
        };
        ["ev", "presence"].into_iter().find_map(|leaf| {
            fabric_brief::owner(&self.fleet, subject, leaf)
                .filter(|(node, sid)| principal(node, sid))
                .map(|(node, sid)| (node, sid, leaf))
        })
    }

    /// One record read before the cursor: folded, and judged by nothing.
    pub fn warm(&mut self, off: u64, subject: &str, body: &[u8]) {
        let _ = self.take(off, subject, body);
    }

    /// One record past the cursor, in read order: what it did to the needs.
    pub fn step(&mut self, off: u64, subject: &str, body: &[u8]) -> Step {
        let Some((node, sid, leaf)) = self.take(off, subject, body) else {
            return Step::Nothing;
        };
        let face = crate::subject::session_face(&self.fleet, &node, &sid, "ev");
        if leaf == "presence" {
            if sid == "node" {
                if !fabric_brief::node_gone(&self.read, &node) {
                    return Step::Nothing;
                }
                let faces = self
                    .read
                    .sessions
                    .iter()
                    .filter(|((n, s), sess)| {
                        *n == node && s != "node" && gone_unclosed(&self.read, n, sess)
                    })
                    .map(|((n, s), _)| crate::subject::session_face(&self.fleet, n, s, "ev"))
                    .collect();
                return Step::Gone(faces);
            }
            let key = (node, sid);
            return match self.read.sessions.get(&key) {
                Some(s) if gone_unclosed(&self.read, &key.0, s) => Step::Gone(vec![face]),
                _ => Step::Nothing,
            };
        }
        if sid == "node" {
            return Step::Nothing;
        }
        let key = (node, sid);
        let Some(s) = self.read.sessions.get(&key) else {
            return Step::Nothing;
        };
        let Some((_, _, fold, seed)) = fabric_brief::newest_fold(s) else {
            return Step::Nothing;
        };
        let st = fold.state();
        let unclosed = st.closed.is_none();
        let need = if unclosed && fabric_brief::gone_settled(&self.read, &key.0, s).is_some() {
            None
        } else if unclosed && seed.partial() {
            return Step::Partial(face);
        } else {
            st.top_observed_need().map(|n| (n.word, n.detail.clone()))
        };
        let (node, sid) = key;
        Step::Folded {
            face,
            node,
            sid,
            need,
        }
    }

    /// Fold one record into the read, and bound what it holds: the session's face it was, when it
    /// was one this watch reads and its body placed (an `ev` body the mirror's decoder refused
    /// is the brief's `opaque`, and names no session).
    fn take(
        &mut self,
        off: u64,
        subject: &str,
        body: &[u8],
    ) -> Option<(String, String, &'static str)> {
        let (node, sid, leaf) = self.face(subject)?;
        let (node, sid) = (node.to_string(), sid.to_string());
        if leaf == "presence" {
            self.read.presence(&node, &sid, body).ok()?;
            return Some((node, sid, leaf));
        }
        let opaque = self.read.opaque;
        self.read
            .ingest(&self.fleet, None, off, subject, body)
            .ok()?;
        if self.read.opaque != opaque {
            return None;
        }
        if let Some(s) = self.read.sessions.get_mut(&(node.clone(), sid.clone())) {
            while s.spaces.len() > 1 {
                s.spaces.pop_first();
            }
            if let Some(space) = s.spaces.values_mut().next_back() {
                bound(space);
            }
        }
        Some((node, sid, leaf))
    }

    /// The sessions this watch holds a fold of (a test's view of its bound).
    #[cfg(test)]
    pub(crate) fn held(&self) -> Vec<(String, String, usize)> {
        self.read
            .sessions
            .iter()
            .flat_map(|((n, s), sess)| {
                sess.spaces
                    .values()
                    .map(move |space| (n.clone(), s.clone(), space.len()))
            })
            .collect()
    }
}

/// Keep a space within [`SPACE_KEEP`] records: past it, the newest snapshot and the records
/// after it, which fold to the same needs (a snapshot is a late reader's seed, §5.5), unless
/// that snapshot seeds fewer needs than its session held (the brief's `snapshot-needs`) where
/// the space it would replace folds whole: a partial fold judges nothing ([`Step::Partial`]),
/// so the watch keeps the whole view until [`SPACE_MAX`]. Past [`SPACE_MAX`], the newest
/// snapshot on whatever it seeds, when what it keeps is within [`SPACE_MAX`]; else the newest
/// [`SPACE_KEEP`] records.
fn bound(space: &mut BusSpace) {
    if space.len() <= SPACE_KEEP {
        return;
    }
    let capped = space.len() > SPACE_MAX;
    if let Some(from_snap) = from_newest_snap(space) {
        let keeps_seed = !from_snap.fold_seeded().1.partial() || space.fold_seeded().1.partial();
        if from_snap.len() <= SPACE_MAX && (capped || keeps_seed) {
            *space = from_snap;
            return;
        }
    }
    if capped {
        let ep = space.ep();
        let skip = space.len() - SPACE_KEEP;
        let mut kept = BusSpace::new(ep);
        for (ms, r) in space.records_ms().skip(skip) {
            kept.offer(BusRec {
                ep,
                ms: Some(ms),
                rec: r.clone(),
            });
        }
        *space = kept;
    }
}

/// The space rebuilt from its newest snapshot: that snapshot and every record after it, when the
/// snapshot folds past every cover the space holds (else `None`: the fold would not start there).
fn from_newest_snap(space: &BusSpace) -> Option<BusSpace> {
    let snap = space.snap()?;
    let upto = snap.f.get_u(Key::Upto)?;
    let past_every_cover = space
        .covers()
        .iter()
        .all(|(lo, n, _)| lo.saturating_add(n.saturating_sub(1)) <= upto);
    if !past_every_cover {
        return None;
    }
    let ep = space.ep();
    let mut rebuilt = BusSpace::new(ep);
    rebuilt.offer(BusRec {
        ep,
        ms: None,
        rec: snap.clone(),
    });
    for (ms, r) in space.records_ms().filter(|(_, r)| r.seq > snap.seq) {
        rebuilt.offer(BusRec {
            ep,
            ms: Some(ms),
            rec: r.clone(),
        });
    }
    Some(rebuilt)
}

/// Read what a watch folds before its cursor `from`, as the brief reads the bus: each session's
/// and node's last presence row below `from` (the roster's last-value walk), then the `ev`
/// records in the [`WARM_WINDOW`] below it. `each` takes every record, oldest `ev` first, and
/// ends the read early by answering `false`.
///
/// # Errors
/// The broker's or the transport's, as the brief's read.
pub fn warm(
    conn: &mut Conn,
    fleet: &str,
    from: u64,
    mut each: impl FnMut(u64, String, Vec<u8>) -> bool,
) -> std::io::Result<()> {
    if from == 0 {
        return Ok(());
    }
    let mut rows = Vec::new();
    transport::walk_last(conn, &format!("/f/{fleet}/pub/*/*/presence"), |row| {
        let (off, subject, body) = row;
        if *off < from {
            rows.push((*off, subject.clone(), body.clone()));
        }
        Ok(())
    })?;
    for (off, subject, body) in rows {
        if !each(off, subject, body) {
            return Ok(());
        }
    }
    let ev = format!("/f/{fleet}/pub/*/*/ev");
    let mut at = from.saturating_sub(WARM_WINDOW);
    while at < from {
        let ((page, (next, _)), _) = transport::fetch_kept(conn, at, &ev, fabric_brief::PAGE_MAX)?;
        for (off, subject, body) in page {
            if off >= from {
                return Ok(());
            }
            if !each(off, subject, body) {
                return Ok(());
            }
        }
        if next <= at {
            break;
        }
        at = next;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_slog::{
        Actor, Close, Conf, Fields, Kind, Log, Mark, Node, Open, Out, RunOut, Src, Stamp, Tier,
    };

    const FACE: &str = "/f/f1/pub/n-a/s-x/ev";

    fn mirrored(log: &mut Log) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            let take = log
                .bus_copy(64, Stamp::new(9, 9))
                .encode(Tier::Wire, &|_| None);
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
                let Step::Folded { need, .. } = watch.step(off, FACE, body.as_bytes()) else {
                    panic!("an ev record of the session folds: {body}");
                };
                whole.offer(aterm_slog::decode_bus(&body).expect("a mirror body"));
                let unbounded = whole.fold_seeded().0;
                let want = unbounded.state().top_observed_need().map(|n| n.word);
                assert_eq!(
                    need.map(|(w, _)| w),
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
    fn a_partial_fold_judges_nothing_and_the_bound_keeps_a_whole_view_over_a_short_snapshot() {
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
            assert_eq!(
                step,
                Step::Partial(FACE.to_string()),
                "@{off}, after a snapshot {seen_snap}: a watch that joined the session late \
                 holds no start and only short snapshots, so it judges nothing"
            );
        }

        let mut whole = NeedWatch::new("f1");
        let (mut longest, mut failed) = (0, false);
        for body in opening.iter().chain(&bodies) {
            off += 1;
            failed |= body.contains("out=fail");
            let step = whole.step(off, FACE, body.as_bytes());
            let Step::Folded { need, .. } = step else {
                panic!("@{off}: a watch that holds the session's start judges it: {step:?}");
            };
            assert_eq!(
                need.map(|(w, _)| w),
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
            if let Step::Folded { need, .. } = late.step(off, FACE, body.as_bytes()) {
                exited = need.map(|(w, _)| w);
            }
        }
        assert_eq!(
            exited,
            Some("exited"),
            "a partial fold that holds the session's close knows its one need"
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

    const TAKE: usize = 50;
}
