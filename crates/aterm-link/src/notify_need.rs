// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm link notify --on need[:<word>]`: each session's need as `aterm fabric brief` folds it
//! from the session log's bus mirror (design §5.5, §7.1).
//!
//! The watch keeps the brief's own read ([`crate::fabric_brief::Read`], built by
//! [`Read::following`]) and folds a session's newest seq space with the brief's
//! [`crate::fabric_brief::newest_fold`] after each of its `ev` records, through
//! [`crate::fabric_brief::settled_fold`], which derives the close of a session a successor's
//! restore names (fix unit E9): there is no second parser of `ev` bodies and no second fold. A
//! clean quit's own close (`out=app-quit`, on the bus before its bridge's `bye quit`, fix unit
//! Q1) wins over both the derived close and the bridge's withdrawal, and pages nothing.
//! Each need the fold names is a [`Pair`], its word and the record that raised it; what the
//! notifier does with them (page a pair it has not paged, keep the pairs standing that the
//! fold still holds) is [`crate::notify`]'s standing rule.

use std::collections::BTreeMap;

use aterm_slog::bus::BusSpace;
use aterm_slog::{BusRec, Key};

use crate::fabric_brief::{self, Derive, Read};
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
    /// By the record at `seq` of seq space `ep`, which the watch holds; or, for a need whose
    /// raising record the bus lost while the watch read the space live and whole, the snapshot
    /// at `seq` that first named it ([`Named`], fix unit J0-3). Either way it is the first record
    /// that told the watch of the need, so a need of its word raised before it is another need
    /// (but `exited`, one need per space, [`Pair::same`]).
    At { ep: u32, seq: u64 },
    /// At or before `seq` of space `ep`: the snapshot there seeded it (its `since_seq` is that
    /// snapshot's own seq, [`aterm_slog::Fold::seed_snap`]: a snapshot names no raise), the
    /// watch holds no record that raised it, and it did not rise where the watch could see it.
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
    /// another need), or one of them [`Raised::Unknown`]. The terminal `exited` need is one
    /// need per space: a read can renumber its derived close or later receive its real close,
    /// without raising another exit. Its token still keeps the sequence actually observed.
    #[must_use]
    pub fn same(&self, other: &Pair) -> bool {
        if self.word != other.word {
            return false;
        }
        match (self.raised, other.raised) {
            (Raised::Unknown, _) | (_, Raised::Unknown) => true,
            (
                Raised::At { ep, .. } | Raised::By { ep, .. },
                Raised::At { ep: e, .. } | Raised::By { ep: e, .. },
            ) if self.word == "exited" => ep == e,
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
                if ep.is_empty() || !ep.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let ep = ep.parse().ok()?;
                match seq.strip_prefix('~') {
                    Some(upto) => {
                        if upto.is_empty() || !upto.bytes().all(|b| b.is_ascii_digit()) {
                            return None;
                        }
                        Raised::By {
                            ep,
                            seq: upto.parse().ok()?,
                        }
                    }
                    None => {
                        if seq.is_empty() || !seq.bytes().all(|b| b.is_ascii_digit()) {
                            return None;
                        }
                        Raised::At {
                            ep,
                            seq: seq.parse().ok()?,
                        }
                    }
                }
            }
            _ => return None,
        };
        Some(Pair { word, raised })
    }
}

/// One observed need a session's fold names ([`aterm_slog::Need::observed`]), most urgent
/// first: its pair, the detail the brief shows beside it (a wall's reset, a failure's exit
/// code), whether the fold knows the need stands now (`seen`), and whether the watch saw it
/// rise (`rose`).
///
/// A need whose raising record the watch holds is [`Raised::At`] that record, and is seen, and
/// rose, when the fold is whole or lost nothing from that record on. A need a snapshot seeded
/// is seen when that snapshot is whole and the space lost nothing past it ([`complete`]). It
/// rose only when, besides, the watch judged a record of the space past its cursor whose fold
/// was [`complete`] and did not name it ([`Live`], fix unit J0-3): the bus lost its raise while
/// the watch read the space whole, so this snapshot is the first the watch knows of it, and its
/// pair is [`Raised::At`] the snapshot. Every other seeded need is [`Raised::By`] the snapshot
/// and never rises: one standing since before the watch read the space whole (LOOP2-1), one
/// the watch already saw rise, or one a short snapshot names (a partial fold pages no seeded
/// need). A need whose page is owed pages again wherever it is seen ([`crate::notify`]'s
/// standing rule), however the fold now knows its raise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub pair: Pair,
    pub detail: Option<String>,
    pub seen: bool,
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
    /// The session's own row says it exited, and the fold holds no close of it and derives none
    /// (no successor's restore says it died, [`fabric_brief::Died::Restored`]): its face, every
    /// need of which is over, and none of which pages.
    Gone(String),
}

/// Whether the session's own row says it exited although its newest fold holds no close of it
/// ([`fabric_brief::gone_settled`]'s `no-close`): nobody there can answer its need, and none
/// comes back. A session whose close the fold holds is not: its close raised `exited` (§7.1)
/// and dropped every other need, whichever of its close and its row the bus carried first (a
/// clean quit's close, `out=app-quit`, leaves no need a watch pages, [`quit_closed`]); nor
/// is one whose successor's restore says it died, whose close the watch derives (fix unit E9,
/// [`fabric_brief::Derive::Paged`]) and whose `exited` stands once paged.
fn exited_unclosed(r: &Read, node: &str, s: &fabric_brief::Session) -> bool {
    fabric_brief::exited(s)
        && fabric_brief::settled_fold(r, node, s, Derive::Paged)
            .is_none_or(|f| f.fold.state().closed.is_none())
}

/// What a watch read live of a session's newest seq space (fix unit J0-3): its epoch, and the
/// observed needs, by word and raising node, of the newest fold of it the watch made after a
/// record of the space it judged past its cursor ([`NeedWatch::step`]; a warm-up's records set
/// none) that was [`complete`]. Every need standing then was known to that fold, so a need a
/// whole snapshot names that it did not name was raised after a record the watch read live,
/// and rose there ([`Named`]), though the bus lost its raising record (a hole, or a cover in
/// its slot). The fold that judges a need risen is itself complete and becomes the new mark,
/// so the need rises once. Kept beside the read, not in its [`BusSpace`], which [`bound`]
/// rebuilds from a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Live {
    ep: u32,
    named: Vec<(&'static str, aterm_slog::Node)>,
}

/// What a session's newest fold makes of its needs, as a watch judges it ([`named`]).
struct Judged {
    /// The observed needs ([`Named`]: a dead session's derived `exited` among them, its
    /// detail `gone`), most urgent first.
    needs: Vec<Named>,
    /// Whether the fold is whole ([`Step::Folded`]).
    whole: bool,
    /// What this fold leaves the watch knowing of its space once judged live, when it is
    /// [`complete`].
    live: Option<Live>,
}

/// The observed needs of a session's newest fold as a watch pages them, judged against what the
/// watch read live of that space before ([`Live`]; `None`: a warm-up's view, under which no
/// seeded need rises).
fn named(r: &Read, node: &str, s: &fabric_brief::Session, live: Option<&Live>) -> Option<Judged> {
    let f = fabric_brief::settled_fold(r, node, s, Derive::Paged)?;
    let (ep, space, seed, st) = (f.ep, f.space, f.seed, f.fold.state());
    let whole = !seed.partial() || st.closed.is_some();
    let seeded = matches!(
        seed,
        aterm_slog::bus::Seed::Snap | aterm_slog::bus::Seed::SnapPartial
    )
    .then(|| space.snap().map(|snap| snap.seq))
    .flatten();
    let knows_all = complete(space, seed);
    let known = live.filter(|l| l.ep == ep);
    let quit = quit_closed(st);
    let observed = || {
        st.needs
            .iter()
            .flatten()
            .filter(move |n| n.observed() && !(quit && n.word == "exited"))
    };
    let needs = observed()
        .map(|n| {
            let held = Raised::At {
                ep,
                seq: n.since_seq,
            };
            let (raised, seen, rose) = match seeded.filter(|upto| n.since_seq <= *upto) {
                None => {
                    let seen = whole || nothing_lost_from(space, n.since_seq);
                    (held, seen, seen)
                }
                Some(upto) => {
                    let fresh =
                        knows_all && known.is_some_and(|l| !l.named.contains(&(n.word, n.node)));
                    if fresh {
                        (held, true, true)
                    } else {
                        (Raised::By { ep, seq: upto }, knows_all, false)
                    }
                }
            };
            Named {
                pair: Pair {
                    word: n.word,
                    raised,
                },
                detail: f.detail(n),
                seen,
                rose,
            }
        })
        .collect();
    let live = knows_all.then(|| Live {
        ep,
        named: observed().map(|n| (n.word, n.node)).collect(),
    });
    Some(Judged { needs, whole, live })
}

/// Whether the fold holds its session's own close by a clean quit (`out=app-quit`, fix unit Q1):
/// the quit closed the session on the bus before its bridge said `bye quit`, and the successor a
/// restore makes carries it on, so its `exited` is no need a watch pages or keeps standing. Its
/// other needs the close already dropped (§7.1). A close of any other `out=` raises `exited` as
/// before, and a session that died without its close reaching the bus is the derived close's
/// ([`fabric_brief::Died::Restored`]).
fn quit_closed(st: &aterm_slog::SessionState) -> bool {
    st.closed
        .as_ref()
        .is_some_and(|c| c.out == aterm_slog::Out::Session(aterm_slog::SessionOut::AppQuit).word())
}

/// Whether a fold of `space` that started at `seed` knows every need its session held as of its
/// head: it is not partial, and the space lost nothing from where the fold started (its first
/// `ms`, or past its snapshot's `upto=`) on: no hole, and no cover past the snapshot.
fn complete(space: &BusSpace, seed: aterm_slog::bus::Seed) -> bool {
    match seed {
        aterm_slog::bus::Seed::Start => space.holes().is_empty(),
        aterm_slog::bus::Seed::Snap => nothing_lost_past_snap(space),
        aterm_slog::bus::Seed::SnapPartial
        | aterm_slog::bus::Seed::NoStart
        | aterm_slog::bus::Seed::Lost => false,
    }
}

/// Whether `space` holds a snapshot and lost nothing past its `upto=`: no cover reaches past it,
/// and no `ms` past it is missing below the newest held.
fn nothing_lost_past_snap(space: &BusSpace) -> bool {
    space
        .snap()
        .and_then(|snap| snap.f.get_u(Key::Upto))
        .is_some_and(|upto| {
            space
                .covers()
                .iter()
                .all(|(lo, n, _)| lo.saturating_add(n.saturating_sub(1)) <= upto)
                && space.holes_from(upto.saturating_add(1)).is_empty()
        })
}

/// Whether `space` holds its start and lost a record its newest snapshot folds past: an `ms` at
/// or below the snapshot's `upto=` that no record or cover of it holds. A space's fold from its
/// start reseeds its needs from a snapshot past a cover ([`BusSpace::fold_seeded`]), never past
/// a hole, so it would not name a need raised by the record the bus lost ([`bound`]).
fn lost_below_its_snap(space: &BusSpace) -> bool {
    let Some(upto) = space.snap().and_then(|snap| snap.f.get_u(Key::Upto)) else {
        return false;
    };
    space.holds_start()
        && (space.max_ms() < upto || space.holes().iter().any(|(lo, _)| *lo <= upto))
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

/// A session's `(node, sid)`, as the watch's read keys it.
type SessionKey = (String, String);

/// The sessions a `need` selector follows.
pub struct NeedWatch {
    fleet: String,
    read: Read,
    /// What the watch read live of each session's newest space ([`Live`]): at most one per
    /// session the read holds, dropped with its space ([`Self::take`]).
    live: BTreeMap<SessionKey, Live>,
}

impl NeedWatch {
    /// An empty watch of `fleet`'s faces.
    #[must_use]
    pub fn new(fleet: &str) -> Self {
        Self {
            fleet: fleet.to_string(),
            read: Read::following(),
            live: BTreeMap::new(),
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
            .filter(|((node, sid), s)| sid != "node" && !exited_unclosed(&self.read, node, s))
            .filter_map(|((node, sid), s)| {
                let pairs: Vec<Pair> = named(&self.read, node, s, None)?
                    .needs
                    .into_iter()
                    .map(|n| n.pair)
                    .collect();
                (!pairs.is_empty()).then(|| {
                    (
                        crate::subject::session_face(&self.fleet, node, sid, "ev"),
                        pairs,
                    )
                })
            })
            .collect()
    }

    /// One record past the cursor, in read order: what it did to the needs. A successor's
    /// restore that first names a session this watch holds, whose close never reached the bus
    /// and whose incarnation ended without withdrawing it ([`fabric_brief::Died::Restored`]),
    /// is that session's record: the watch derives its close ([`fabric_brief::settled_fold`])
    /// and judges its face, where its `exited` rises once (the restore itself raises nothing on
    /// the successor's own face). A restore after a quit, or a reopened tab's, is the
    /// successor's own record. A record of a session's own `ev` face whose fold is [`complete`]
    /// is what the watch now knows it read live of that space ([`Live`]).
    pub fn step(&mut self, off: u64, subject: &str, body: &[u8]) -> Step {
        let Some((node, sid, leaf, restored)) = self.take(off, subject, body) else {
            return Step::Nothing;
        };
        if sid == "node" {
            return Step::Nothing;
        }
        let died = restored.filter(|key| {
            self.read
                .sessions
                .get(key)
                .and_then(|s| fabric_brief::settled_fold(&self.read, &key.0, s, Derive::Paged))
                .is_some_and(|f| f.died.is_some())
        });
        let own = died.is_none() && leaf == "ev";
        let (node, sid, leaf) = match died {
            Some((node, sid)) => (node, sid, "ev"),
            None => (node, sid, leaf),
        };
        let face = crate::subject::session_face(&self.fleet, &node, &sid, "ev");
        let key = (node, sid);
        let Some(s) = self.read.sessions.get(&key) else {
            return Step::Nothing;
        };
        if exited_unclosed(&self.read, &key.0, s) {
            return Step::Gone(face);
        }
        if leaf == "presence" {
            return Step::Nothing;
        }
        let Some(Judged { needs, whole, live }) = named(&self.read, &key.0, s, self.live.get(&key))
        else {
            return Step::Nothing;
        };
        if let Some(live) = live.filter(|_| own) {
            self.live.insert(key.clone(), live);
            if self.live.len() > self.read.sessions.len() {
                let sessions = &self.read.sessions;
                self.live.retain(|key, _| sessions.contains_key(key));
            }
        }
        if fabric_brief::node_gone(&self.read, &key.0)
            && fabric_brief::settled_fold(&self.read, &key.0, s, Derive::Paged)
                .is_some_and(|f| f.fold.state().closed.is_none())
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
    /// is the brief's `opaque`, and names no session), and the session a successor's restore in
    /// it first named ([`Read::ingest`]).
    fn take(
        &mut self,
        off: u64,
        subject: &str,
        body: &[u8],
    ) -> Option<(String, String, &'static str, Option<SessionKey>)> {
        let (node, sid, leaf) = self.face(subject)?;
        let (node, sid) = (node.to_string(), sid.to_string());
        if leaf == "presence" {
            self.read.presence(&node, &sid, body).ok()?;
            return Some((node, sid, leaf, None));
        }
        let opaque = self.read.opaque;
        let restored = self
            .read
            .ingest(&self.fleet, None, off, subject, body)
            .ok()?;
        if self.read.opaque != opaque {
            return None;
        }
        let key = (node, sid);
        if let Some(s) = self.read.sessions.get_mut(&key) {
            while s.spaces.len() > 1 {
                s.spaces.pop_first();
            }
            let spaces = &s.spaces;
            s.offered.retain(|ep, _| spaces.contains_key(ep));
            if self
                .live
                .get(&key)
                .is_some_and(|l| !spaces.contains_key(&l.ep))
            {
                self.live.remove(&key);
            }
            if let Some(space) = s.spaces.values_mut().next_back() {
                bound(space);
            }
        }
        let (node, sid) = key;
        Some((node, sid, leaf, restored))
    }

    /// The epochs of the spaces this watch knows it read live ([`Live`]), one per session (a
    /// test's view of their bound).
    #[cfg(test)]
    pub(crate) fn live_eps(&self) -> Vec<u32> {
        self.live.values().map(|l| l.ep).collect()
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
/// [`SPACE_KEEP`] records. And at any length, a space that holds its start and lost a record
/// its newest snapshot folds past ([`lost_below_its_snap`]: the bus dropped it, a hole) is
/// folded from that snapshot on, as a cover's would be: the snapshot's needs are the session's
/// as of its `upto=`, which a fold from the start past the hole cannot know (fix unit J0-3).
fn bound(space: &mut BusSpace) {
    if lost_below_its_snap(space) {
        if let Some(from_snap) = from_newest_snap(space) {
            *space = from_snap;
        }
    }
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
#[path = "notify_need_tests.rs"]
mod tests;
