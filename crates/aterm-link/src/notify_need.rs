// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm link notify --on need[:<word>]`: each session's need as `aterm fabric brief` folds it
//! from the session log's bus mirror (design §5.5, §7.1).
//!
//! The watch keeps the brief's own read ([`crate::fabric_brief::Read`], built by
//! [`Read::following`]) and folds a session's newest seq space with the brief's
//! [`crate::fabric_brief::newest_fold`] after each of its `ev` records: there is no second parser
//! of `ev` bodies and no second fold. Each need the fold names is a [`Pair`], its word and the
//! record that raised it; what the notifier does with them (page a pair it has not paged, keep
//! the pairs standing that the fold still holds) is [`crate::notify`]'s standing rule.

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

/// When a need a session's fold names was raised, as far as the watch can tell: with the need's
/// word, the pair a `need` selector's standing rule is keyed by ([`Pair`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Raised {
    /// By the record at `seq` of seq space `ep`, which the watch holds.
    At { ep: u32, seq: u64 },
    /// At or before `seq` of space `ep`: the snapshot there seeded it, and the watch holds no
    /// record that raised it.
    By { ep: u32, seq: u64 },
    /// Not known: a store written before the pairs, which named the word alone.
    Unknown,
}

/// One need: its word ([`aterm_slog::observed_need_words`]) and when it was raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    pub word: &'static str,
    pub raised: Raised,
}

impl Pair {
    /// Whether `self` and `other` can be the same need: the same word, raised by the same
    /// record of one space, or one of them seeded by a snapshot of that space at or after the
    /// other's record (a seq space is one process's numbering, so a pair of another space is
    /// another need), or one of them [`Raised::Unknown`].
    #[must_use]
    pub fn same(&self, other: &Pair) -> bool {
        if self.word != other.word {
            return false;
        }
        match (self.raised, other.raised) {
            (Raised::Unknown, _) | (_, Raised::Unknown) => true,
            (Raised::At { ep, seq }, Raised::At { ep: e, seq: s }) => ep == e && seq == s,
            (Raised::At { ep, seq }, Raised::By { ep: e, seq: upto })
            | (Raised::By { ep: e, seq: upto }, Raised::At { ep, seq }) => ep == e && seq <= upto,
            (Raised::By { ep, .. }, Raised::By { ep: e, .. }) => ep == e,
        }
    }

    /// The pair as the standing store spells it: `<word>/<ep>/<seq>` ([`Raised::At`]),
    /// `<word>/<ep>/~<seq>` ([`Raised::By`]) or the word alone ([`Raised::Unknown`]).
    #[must_use]
    pub fn token(&self) -> String {
        match self.raised {
            Raised::At { ep, seq } => format!("{}/{ep}/{seq}", self.word),
            Raised::By { ep, seq } => format!("{}/{ep}/~{seq}", self.word),
            Raised::Unknown => self.word.to_string(),
        }
    }

    /// A [`Pair::token`] read back; a word that is no need word of the log's is no pair.
    #[must_use]
    pub fn parse(token: &str) -> Option<Pair> {
        let mut it = token.split('/');
        let word = aterm_slog::vocab::lookup(aterm_slog::vocab::NEED_WORDS, it.next()?)?;
        let raised = match (it.next(), it.next(), it.next()) {
            (None, None, None) => Raised::Unknown,
            (Some(ep), Some(seq), None) => {
                let ep = ep.parse().ok()?;
                match seq.strip_prefix('~') {
                    Some(upto) => Raised::By {
                        ep,
                        seq: upto.parse().ok()?,
                    },
                    None => Raised::At {
                        ep,
                        seq: seq.parse().ok()?,
                    },
                }
            }
            _ => return None,
        };
        Some(Pair { word, raised })
    }
}

/// One observed need a session's fold names ([`aterm_slog::Need::observed`]), most urgent
/// first: its pair, the detail the brief shows beside it (a wall's reset, a failure's exit
/// code), and whether the watch saw it rise: it holds the record that raised it, and the fold
/// is whole or lost nothing after that record (a need a snapshot seeded did not rise where the
/// watch could see it, so it never pages).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub pair: Pair,
    pub detail: Option<String>,
    pub rose: bool,
}

/// What one record did to the needs a [`NeedWatch`] holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Not a face the watch reads, a body it could not place, a node's row, or a record of a
    /// session whose node left its will (`state=away`, or the legacy `gone`) and whose close
    /// the fold does not hold: nothing there answers its need, and a reattach restores it, so
    /// the record pages and drops nothing.
    Nothing,
    /// One of the session's `ev` records: its face (the `ev` subject, which the standing rule
    /// is keyed by), its node and sid, the needs its fold names now, and whether that fold is
    /// whole: not partial ([`aterm_slog::bus::Seed::partial`]: the watch holds neither the
    /// session's start nor a snapshot that seeds every need, or it lost records no snapshot
    /// folds past), or it holds the session's close. A need a whole fold does not name is no
    /// longer standing; a partial fold may lack a need raised before what it holds.
    Folded {
        face: String,
        node: String,
        sid: String,
        needs: Vec<Named>,
        whole: bool,
    },
    /// The session's own row says it exited, and the fold holds no close of it: its face, every
    /// need of which is over.
    Gone(String),
}

/// Whether the session's own row says it exited although its newest fold holds no close of it
/// ([`fabric_brief::gone_settled`]'s `no-close`): nobody there can answer its need, and none
/// comes back. A session whose close the fold holds is not: its close raised `exited` (§7.1)
/// and dropped every other need, whichever of its close and its row the bus carried first.
fn exited_unclosed(s: &fabric_brief::Session) -> bool {
    fabric_brief::exited(s)
        && fabric_brief::newest_fold(s).is_none_or(|(_, _, fold, _)| fold.state().closed.is_none())
}

/// The observed needs of a session's newest fold ([`Named`]), and whether the fold is whole
/// ([`Step::Folded`]).
fn named(s: &fabric_brief::Session) -> Option<(Vec<Named>, bool)> {
    let (ep, space, fold, seed) = fabric_brief::newest_fold(s)?;
    let st = fold.state();
    let whole = !seed.partial() || st.closed.is_some();
    let seeded = matches!(
        seed,
        aterm_slog::bus::Seed::Snap | aterm_slog::bus::Seed::SnapPartial
    )
    .then(|| space.snap().map(|snap| snap.seq))
    .flatten();
    let needs = st
        .needs
        .iter()
        .flatten()
        .filter(|n| n.observed())
        .map(|n| {
            let raised = match seeded {
                Some(upto) if n.since_seq <= upto => Raised::By { ep, seq: upto },
                _ => Raised::At {
                    ep,
                    seq: n.since_seq,
                },
            };
            let rose = matches!(raised, Raised::At { .. })
                && (whole || nothing_lost_from(space, n.since_seq));
            Named {
                pair: Pair {
                    word: n.word,
                    raised,
                },
                detail: n.detail.clone(),
                rose,
            }
        })
        .collect();
    Some((needs, whole))
}

/// Whether `space` holds the record at `seq` and lost nothing from it on: no cover reaches it
/// or past it, and no record of the bus class is missing between it and the newest held.
fn nothing_lost_from(space: &BusSpace, seq: u64) -> bool {
    let Some(ms) = space
        .records_ms()
        .find(|(_, r)| r.seq == seq)
        .map(|(ms, _)| ms)
    else {
        return false;
    };
    space
        .covers()
        .iter()
        .all(|(lo, n, _)| lo.saturating_add(n.saturating_sub(1)) < ms)
        && space.holes_from(ms).is_empty()
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

    /// Whether the watch holds a fold of the session whose `ev` face is `face`: one whose
    /// standing needs a bound must not drop, since the fold may name them again and a need it
    /// names without a pair standing pages ([`crate::fabric_brief::SESSIONS_MAX`] at most).
    #[must_use]
    pub fn holds(&self, face: &str) -> bool {
        self.face(face).is_some_and(|(node, sid, leaf)| {
            leaf == "ev"
                && self
                    .read
                    .sessions
                    .contains_key(&(node.to_string(), sid.to_string()))
        })
    }

    /// One record read before the cursor: folded, and judged by nothing.
    pub fn warm(&mut self, off: u64, subject: &str, body: &[u8]) {
        let _ = self.take(off, subject, body);
    }

    /// Every session's face and the needs its newest fold names, as a warm-up leaves them: a
    /// need raised below the cursor stands ([`Step::Gone`]'s sessions name none).
    #[must_use]
    pub fn standing(&self) -> Vec<(String, Vec<Pair>)> {
        self.read
            .sessions
            .iter()
            .filter(|((_, sid), s)| sid != "node" && !exited_unclosed(s))
            .filter_map(|((node, sid), s)| {
                let (needs, _) = named(s)?;
                let pairs: Vec<Pair> = needs.into_iter().map(|n| n.pair).collect();
                (!pairs.is_empty()).then(|| {
                    (
                        crate::subject::session_face(&self.fleet, node, sid, "ev"),
                        pairs,
                    )
                })
            })
            .collect()
    }

    /// One record past the cursor, in read order: what it did to the needs.
    pub fn step(&mut self, off: u64, subject: &str, body: &[u8]) -> Step {
        let Some((node, sid, leaf)) = self.take(off, subject, body) else {
            return Step::Nothing;
        };
        if sid == "node" {
            return Step::Nothing;
        }
        let face = crate::subject::session_face(&self.fleet, &node, &sid, "ev");
        let key = (node, sid);
        let Some(s) = self.read.sessions.get(&key) else {
            return Step::Nothing;
        };
        if exited_unclosed(s) {
            return Step::Gone(face);
        }
        if leaf == "presence" {
            return Step::Nothing;
        }
        let Some((needs, whole)) = named(s) else {
            return Step::Nothing;
        };
        if fabric_brief::node_gone(&self.read, &key.0)
            && fabric_brief::newest_fold(s).is_some_and(|(_, _, f, _)| f.state().closed.is_none())
        {
            return Step::Nothing;
        }
        let (node, sid) = key;
        Step::Folded {
            face,
            node,
            sid,
            needs,
            whole,
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
/// the space it would replace folds whole: a partial fold drops no standing need and pages
/// only a need whose raise it holds with nothing lost after it ([`Step::Folded`]), so the
/// watch keeps the whole view until [`SPACE_MAX`]. Past [`SPACE_MAX`], the newest
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
    fn a_partial_fold_pages_no_seeded_need_and_the_bound_keeps_a_whole_view_over_a_short_snapshot()
    {
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
}
