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
mod tests {
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
    fn a_short_snapshot_pages_no_need_whose_raise_the_bus_lost_and_the_next_whole_one_pages_it_once(
    ) {
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
}
