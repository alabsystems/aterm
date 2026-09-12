// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE FABRIC'S CONSISTENT-CUT REPLAY** (§10), rebuilt from the bus bytes
//! alone.
//!
//! §10 states the shape in two sentences and they pull in different directions:
//!
//! > With one broker per fleet the bus has a single offset spine, so a fleet cut
//! > *on the bus* is one offset. Across the session logs the built
//! > Chandy-Lamport machinery applies … over the sessions' logs joined by the
//! > recorded edges — the composition claim of §12 (A7).
//!
//! The first sentence makes a cut trivial: one number, and every consistency
//! question answers itself. The second is the one worth proving, and it needs a
//! partition per SESSION rather than per broker — a vector of offsets, and edges
//! that join sessions hosted by different nodes on different hosts.
//!
//! So a partition here is a **session**, its "log" is the sub-sequence of bus
//! records that name it, and a [`Cut`] is one bus offset per session. The
//! machinery is deliberately the same shape as astream's own
//! (`crates/astream-engine/src/fleet.rs`: `Cut`, `cut_includes`,
//! `is_consistent`, `cross_edges_from_logs`, `replay_to_cut`) because it is the
//! same idea at a different seam — and it is REIMPLEMENTED rather than imported,
//! because §11.2 pins this crate's dependencies to `astream-broker`,
//! `astream-cap`, `aterm-types` and `aterm-uds`, and `astream-engine` is in none
//! of them.
//!
//! ## What the edges are made of
//!
//! astream's durable edge is the envelope's `caused_by` pointer, which
//! `cross_edges_from_logs` rebuilds from stored bytes (`term.fleet.durable-watermark`).
//! The fabric's is `re=` — §4.1's "the offset this record answers" — and the
//! offsets are the BROKER's, so a correlation id nobody can forge is the same
//! one across two hosts as it is across two sessions (§6.4). A record on session
//! P carrying `re=<n>`, where the record at `n` belongs to a different session
//! Q, is an edge `Q → P`.
//!
//! **`re=` IS NEVER AN INPUT TO AN AUTHORITY DECISION.** That is the claim, and
//! it is narrower than "read in one place": `re=` is read here as causality for
//! an auditor, and it is read at the delivery seam and by the two renderers
//! (`bridge::Bridge::deliver_record`, `mirror`, `tui`) as CORRELATION, which is
//! a label on a row and not a permission. §6.6 is explicit that a body which
//! could trigger keystrokes on the strength of a `re=` would let any
//! lane-writer drive a worker, so the bridge's four apply conditions are the
//! holder, the hold, the epoch and the generation — and none of them is a
//! `re=`. A `re=` on an applied `term/in` is causality for an auditor and
//! authority for nobody.
//!
//! ## The honest grade
//!
//! §10 gives the aterm seam **watermark grade** and says so — "the screen that
//! followed is `term/out` after seq `n` — watermark-grade at the aterm seam, the
//! same honest grade as `term.echo.watermark-retire`", against the astream-host
//! seam where "the pointer is exact". This module is at that grade and no
//! higher:
//!
//! * the SCREEN a cut re-folds is the last record on the session's `screen`
//!   face at or below the cut — a snapshot face (§3.3: "a full `DELTA screen`
//!   snapshot, opt-in, ≤ 4/s"), so the fold is a last-value fold, the same class
//!   §10 lists beside halt, presence, control and barrier. It is NOT the
//!   byte-exact re-fold of a `term/out` stream; that is astream-host's
//!   `replay_to_cut` over envelope logs, already green as
//!   `term.fleet.consistent-cut-replay`, and this rung does not re-prove it.
//! * the EDGE is the sender's `re=`, attested only in the sense that the `<src>`
//!   segment it rides under is cap-forced. A sender can point `re=` at a record
//!   that did not cause it. What it cannot do is point at a record that does not
//!   exist, or forge whose session it is on — and that second half is a
//!   PROPERTY OF [`attributed_session_of`], not of the subject grammar. Two of
//!   the four faces that name a session name it in a segment the SENDER types
//!   (`in/<node>/<sid>/<src>/<kind>`, `term/<node>/<sid>/in/<src>`), and on the
//!   other two a node's cap covers its whole `pub/<n>/` subtree, so a claim on
//!   a sid is always available to anyone. Every one of them is corroborated
//!   against [`Hosts`] — §6.1's first-sight pin, read off the bus — before a
//!   record joins a session's log. [`session_of`] alone is the parse, and is
//!   NOT that decision.
//!
//! ## Versioned folds, loudly
//!
//! §10: "the folds a reader applies … are versioned by the body's `v=` so two
//! readers with different fold versions disagree loudly, never silently". A
//! record whose `v=` this reader does not know ends the replay with
//! [`ReplayError::UnknownVersion`] rather than being skipped — a shorter screen
//! that looks valid is the failure that rule exists to prevent.

use std::collections::{BTreeMap, BTreeSet};

use crate::body::Body;

/// The fold version this reader implements (§4.1's `v=`).
pub const FOLD_V: u32 = 1;

/// One bus record, as every reader here takes it: `(offset, subject, body)`.
pub type Record = (u64, String, Vec<u8>);

/// Why a replay stopped instead of answering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    /// A record inside the cut carries a fold version this reader does not
    /// implement. §10's rule: disagree loudly.
    UnknownVersion {
        /// The offset of the record that could not be folded.
        at: u64,
        /// The `v=` it carried.
        v: u32,
    },
}

/// One recorded cross-session causal edge, rebuilt from the bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossEdge {
    /// The session and offset of the record that was answered.
    pub cause: (String, u64),
    /// The session and offset of the record that answers it. `cause.0 !=
    /// effect.0` — a same-session `re=` is correlation, not a cut constraint.
    pub effect: (String, u64),
}

/// A global checkpoint: the highest bus offset each session admits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cut {
    /// Per-session cut offsets. A session listed twice keeps its FIRST entry,
    /// the same rule astream's `Cut::offset_of` follows.
    pub offsets: Vec<(String, u64)>,
}

impl Cut {
    /// A cut that admits everything up to `off` in every session `records`
    /// names — the "one offset on the bus" §10 starts from, expanded into the
    /// per-session vector the edges are checked against.
    #[must_use]
    pub fn flat(records: &[Record], off: u64) -> Self {
        let hosts = Hosts::from_bus(records);
        let mut sessions: Vec<String> = records
            .iter()
            .filter_map(|(_, subject, body)| attributed_session_of(subject, body, &hosts))
            .collect();
        sessions.sort();
        sessions.dedup();
        Self {
            offsets: sessions.into_iter().map(|s| (s, off)).collect(),
        }
    }

    /// The cut offset for one session, if the cut names it.
    #[must_use]
    pub fn offset_of(&self, session: &str) -> Option<u64> {
        self.offsets
            .iter()
            .find(|(s, _)| s == session)
            .map(|(_, o)| *o)
    }

    /// Move one session's cut offset, adding it if the cut did not name it.
    pub fn set(&mut self, session: &str, off: u64) {
        match self.offsets.iter_mut().find(|(s, _)| s == session) {
            Some(entry) => entry.1 = off,
            None => self.offsets.push((session.to_string(), off)),
        }
    }
}

/// Does the cut admit `at` in `session`? A session the cut does not name admits
/// nothing.
#[must_use]
pub fn cut_includes(cut: &Cut, session: &str, at: u64) -> bool {
    cut.offset_of(session).is_some_and(|o| at <= o)
}

/// Chandy-Lamport consistency: for every edge whose EFFECT the cut admits, it
/// must also admit the CAUSE.
///
/// An effect without its cause is an orphan — a keystroke that answers a
/// question the cut says was never asked — and is rejected. A cause without its
/// effect is a message in flight, and is admitted: that is the asymmetry the
/// whole construction rests on.
#[must_use]
pub fn is_consistent(cut: &Cut, edges: &[CrossEdge]) -> bool {
    edges.iter().all(|e| {
        !cut_includes(cut, &e.effect.0, e.effect.1) || cut_includes(cut, &e.cause.0, e.cause.1)
    })
}

/// sid → the node the bus itself attests hosts it, and every sid two nodes both
/// claimed.
///
/// ## Why a reader of the bus needs this at all
///
/// EXACTLY ONE SEGMENT OF A SUBJECT IS CAP-FORCED, and it is not always the one
/// that names a session. Three shapes name a session and they do not have the
/// same standing:
///
/// * `pub/<n>/<sid>/<leaf>` and `term/<n>/<sid>/out|screen` — the `<owner>` at 4
///   IS the bound principal (§8.2: `rw,p=<n>:/f/<F>/pub/<n>/>`,
///   `rw,p=<n>:/f/<F>/term/<n>/*/out`). A node is saying "this is mine" under
///   its own cap.
/// * `in/<node>/<sid>/<src>/<kind>` (`rw,p=<src>:/f/<F>/in/*/*/<src>/*`) and
///   `term/<node>/<sid>/in/<src>` (`rw,p=<src>:/f/<F>/term/*/*/in/<src>`) — the
///   bound principal is the `<src>`, and BOTH the node at 4 and the sid at 5 are
///   free text the SENDER types. A peer inside its ordinary ring can publish
///   `/f/<F>/in/n-me/s-anything/n-me/note` all day, on a session it does not
///   host and no bridge will ever deliver to.
///
/// So on those two faces the session is a CLAIM, and §6.1 already says what to
/// do with the claim — it is the same claim, reached from the reading side:
///
/// > a node's cap covers its whole `pub/<n>/` subtree, so a rogue node *can*
/// > publish a presence row for a sid it does not host; pinning stops that from
/// > becoming a route into the rogue's own read lane.
///
/// FIRST ATTESTED SIGHT PINS (TOFU, by bus offset — the one order every reader
/// agrees on), and a later claim on a pinned sid from a different node is a
/// CONFLICT: it is refused attribution and listed in [`Hosts::conflicts`]. That
/// asymmetry is deliberate and it is the whole security argument. A rogue cannot
/// STEAL a live session, because the host that runs it claimed it first, at a
/// lower offset, the moment it published presence; and it cannot ERASE one
/// either, because a conflict drops the interloper's records and never the
/// pin's. All it can do is pre-claim a sid nobody has minted yet, which is a
/// guess at 16 random hex characters.
///
/// AND THE CONFLICT IS LOUD. §10's rule for this module is "disagree loudly,
/// never silently"; a caller asking why a session's edges are thin gets the
/// answer from [`Hosts::conflicts`] rather than having to re-derive it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hosts {
    pinned: BTreeMap<String, String>,
    conflicts: BTreeSet<String>,
}

impl Hosts {
    /// Learn the pins from a page of bus records. Order-independent: the claims
    /// are folded in BUS-OFFSET order, not in the order `records` happens to
    /// hold them, so two readers paging the same bus differently pin the same
    /// way.
    #[must_use]
    pub fn from_bus(records: &[Record]) -> Self {
        let mut claims: Vec<(u64, String, String)> = Vec::new();
        for (off, subject, body) in records {
            if let Some((node, sid)) = hosting_claim(subject, body) {
                claims.push((*off, sid, node));
            }
        }
        claims.sort_unstable();
        let mut out = Self::default();
        for (_, sid, node) in claims {
            match out.pinned.get(&sid) {
                None => {
                    out.pinned.insert(sid, node);
                }
                Some(first) if *first == node => {}
                Some(_) => {
                    out.conflicts.insert(sid);
                }
            }
        }
        out
    }

    /// The node this reader has pinned as `sid`'s host, if any.
    #[must_use]
    pub fn host_of(&self, sid: &str) -> Option<&str> {
        self.pinned.get(sid).map(String::as_str)
    }

    /// Every sid a second node claimed after it was pinned — §6.1's `✗ conflict`,
    /// seen from the bus rather than from a sender's roster.
    #[must_use]
    pub fn conflicts(&self) -> Vec<&str> {
        self.conflicts.iter().map(String::as_str).collect()
    }
}

/// The `(node, sid)` a record claims a HOSTING relationship for, under the
/// node's own cap-forced segment — the raw material of [`Hosts`].
///
/// Two positions can carry it and both are cap-forced to a NODE: the `<owner>`
/// at 4 of an owner face, and the `<src>` at 6 of a principal's lane, whose
/// body's `from=` is the node saying "one of mine sent this" (§4.3). Nothing
/// else is a claim: on the addressee-chosen faces the node segment is typed by
/// the sender and would let a stranger pin any sid to any node.
fn hosting_claim(subject: &str, body: &[u8]) -> Option<(String, String)> {
    let segs: Vec<&str> = subject.split('/').collect();
    if segs.len() < 6 || !segs[0].is_empty() || segs[1] != "f" {
        return None;
    }
    match (segs[3], segs.len()) {
        ("pub", 7) if is_node(segs[4]) && is_sid(segs[5]) => {
            Some((segs[4].to_string(), segs[5].to_string()))
        }
        ("term", 7) if is_node(segs[4]) && is_sid(segs[5]) && is_term_leaf(segs[6]) => {
            Some((segs[4].to_string(), segs[5].to_string()))
        }
        ("in", 8)
            if segs[4] == "p" && crate::subject::is_principal(segs[5]) && is_node(segs[6]) =>
        {
            Body::decode(body)
                .0
                .from
                .filter(|f| is_sid(f))
                .map(|f| (segs[6].to_string(), f))
        }
        _ => None,
    }
}

/// A session principal (§3.2).
fn is_sid(s: &str) -> bool {
    crate::subject::is_principal(s) && s.starts_with("s-")
}

/// A node principal (§3.2).
fn is_node(s: &str) -> bool {
    crate::subject::is_principal(s) && s.starts_with("n-")
}

/// The two `term` leaves a NODE owns (§3.3). `in` is the drive face and is
/// owned by its `<src>`, not by the node in the subject.
fn is_term_leaf(leaf: &str) -> bool {
    matches!(leaf, "out" | "screen")
}

/// Which session a record's OWN BYTES name, under a segment its author could not
/// choose — or `None` for a record that names no session (a node's own presence,
/// its `ev` digest, a fleet halt) and for one whose session is only the sender's
/// claim.
///
/// Parsed BY POSITION FROM THE LEFT, the rule §3.3 pins and
/// [`crate::subject::parse_in`] already applies at the delivery seam: a
/// right-anchored read of an over-long subject is exactly the forgery that rule
/// exists to refuse, and a reader that fell for it would attribute a stranger's
/// record to somebody else's session.
///
/// The one place the BODY is consulted is a message on a principal's lane
/// (`/f/<F>/in/p/<h>/<src>/<kind>`), which names no session in its subject.
/// There the sending session is the body's `from=` — attested rather than
/// claimed, because the node's `<src>` segment is cap-forced and the node could
/// type as any session it hosts anyway (§4.1, §8.3).
///
/// ATTESTED IS THE WHOLE OF IT, so all three positions the word rests on are
/// checked: the literal `p` at 4, a principal at 5, and a `<src>` at 6 that is
/// a NODE. That is the same test [`crate::bridge::Bridge::render_from`] makes
/// at the delivery seam, and it must be made here too — a `from=` under a
/// human's or a peer session's own cap-forced `<src>` is a claim, not an
/// attestation.
///
/// # THIS IS NOT ATTRIBUTION, and callers must not use it as such
///
/// What it establishes is that a NODE said this record is on that session. It
/// does NOT establish that the node hosts the session: a node's cap covers its
/// whole `pub/<n>/` subtree (§6.1), so a rogue can publish
/// `pub/n-rogue/<victim sid>/control` and this function will read the sid out of
/// it. Deciding whose LOG a record joins is
/// [`attributed_session_of`], which corroborates the node against [`Hosts`]; the
/// two addressee-chosen faces do not even reach here, because the sid on them is
/// not attested by anything at all.
#[must_use]
pub fn session_of(subject: &str, body: &[u8]) -> Option<String> {
    let segs: Vec<&str> = subject.split('/').collect();
    // `["", "f", "<F>", "<face>", "<owner>", "<sid|leaf>", …]` — the leading
    // slash yields an empty first segment, so the face is at index 3 and the
    // owner at 4. Every subject that names a session names it at index 5, and
    // nothing else is looked at.
    if segs.len() < 6 || !segs[0].is_empty() || segs[1] != "f" {
        return None;
    }
    match (segs[3], segs.len()) {
        // `/f/<F>/pub/<node>/<sid>/<leaf>` — seven, exactly, and the owner at 4
        // is the node whose cap bound the publish. An owner that is not a node
        // (`pub/p/<h>/…`) and the node's own faces (`pub/<n>/node/…`) name no
        // session.
        ("pub", 7) if is_node(segs[4]) && is_sid(segs[5]) => Some(segs[5].to_string()),
        // `/f/<F>/term/<node>/<sid>/out|screen` — seven, and the leaf is pinned:
        // the EIGHT-segment `term/<node>/<sid>/in/<src>` is the drive face, whose
        // cap binds the `<src>` at 7 and leaves the sid at 5 free text. It is an
        // addressed claim, not an attestation, and it is read by
        // [`addressed_session_of`].
        ("term", 7) if is_node(segs[4]) && is_sid(segs[5]) && is_term_leaf(segs[6]) => {
            Some(segs[5].to_string())
        }
        // `/f/<F>/in/p/<h>/<src>/<kind>` addresses a PRINCIPAL and names no
        // session in its subject, so the sending session is the attested
        // `from=`, or none. EVERY position is pinned, because "attested" is a
        // statement about the `<src>` at 6 and nothing else: only a node could
        // have typed as the session its body names. An `in` record whose
        // addressee is not the `p` literal, or whose `<src>` is a human or a
        // peer session, names NO session — its `from=` is a claim its own cap
        // never backed.
        ("in", 8)
            if segs[4] == "p" && crate::subject::is_principal(segs[5]) && is_node(segs[6]) =>
        {
            Body::decode(body).0.from.filter(|f| is_sid(f))
        }
        _ => None,
    }
}

/// The `(node, sid)` an ADDRESSEE-CHOSEN face is aimed at — a claim by the
/// sender, and never on its own a statement about whose session it is.
///
/// The two faces are `/f/<F>/in/<node>/<sid>/<src>/<kind>` and
/// `/f/<F>/term/<node>/<sid>/in/<src>`. On both, the cap binds the `<src>` and
/// nothing else; the node and the sid are typed by whoever published. An `in`
/// subject of any other length is the over-long forgery §3.3 refuses, and names
/// nothing here either.
#[must_use]
pub fn addressed_session_of(subject: &str) -> Option<(String, String)> {
    let segs: Vec<&str> = subject.split('/').collect();
    if segs.len() != 8
        || !segs[0].is_empty()
        || segs[1] != "f"
        || !is_node(segs[4])
        || !is_sid(segs[5])
    {
        return None;
    }
    match segs[3] {
        "in" if crate::subject::is_principal(segs[6]) => {
            Some((segs[4].to_string(), segs[5].to_string()))
        }
        "term" if segs[6] == "in" && crate::subject::is_principal(segs[7]) => {
            Some((segs[4].to_string(), segs[5].to_string()))
        }
        _ => None,
    }
}

/// WHOSE LOG A RECORD JOINS — the only session question this module's machinery
/// ever asks, and the one the `attested` in the header is about.
///
/// A record is on session `S` when either
///
/// * a NODE attested it ([`session_of`]) **and** that node is the one `hosts`
///   pinned for `S` — so a rogue publishing `pub/n-rogue/<victim>/control`
///   cannot rewrite the victim's folded holder or screen; or
/// * it is addressed to `S` on a sender-chosen face ([`addressed_session_of`])
///   **and** the node it is addressed to is `S`'s pinned host — so a peer
///   cannot file a record under a session it does not host, and every `re=`
///   pointing at that record cannot bind that session's future cuts to a cause
///   it never had.
///
/// A sid WITH NO PIN is attributed nothing. That costs edges rather than
/// inventing them, which is the safe direction: a missing edge admits cuts that
/// a fuller reader would also admit, while an invented one rejects cuts that
/// really are consistent.
///
/// A CONTESTED SID KEEPS ITS FIRST PIN, and this function never asks
/// [`Hosts::conflicts`]. The interloper's records are attributed nothing — they
/// fail the corroboration above like any other stranger's — and the pinned
/// host's records go on attributing, go on joining the `owner` map of
/// [`cross_edges_from_bus`], and go on binding cuts. That is [`Hosts`]'s whole
/// asymmetry, deliberately: if a second claim erased the sid instead, a rogue
/// could ERASE any live session's edges by publishing one presence row, which is
/// a cheaper attack than the one the pin exists to stop. This doc used to say a
/// sid "two nodes claimed is attributed NOTHING" — an exclusion the machinery
/// has never performed, and one an auditor would trust because it reads as the
/// conservative direction. [`Hosts::conflicts`] is how a caller LEARNS a sid was
/// contested (§10's "disagree loudly"); it is not a filter this function
/// applies.
#[must_use]
pub fn attributed_session_of(subject: &str, body: &[u8], hosts: &Hosts) -> Option<String> {
    if let Some(sid) = session_of(subject, body) {
        // The attesting node is the owner segment, except on the principal lane
        // where it is the `<src>`.
        let segs: Vec<&str> = subject.split('/').collect();
        let by = if segs.get(3) == Some(&"in") {
            segs.get(6)
        } else {
            segs.get(4)
        };
        return (hosts.host_of(&sid) == by.copied()).then_some(sid);
    }
    let (node, sid) = addressed_session_of(subject)?;
    (hosts.host_of(&sid) == Some(node.as_str())).then_some(sid)
}

/// Rebuild the fabric's cross-session edges from the bus bytes alone.
///
/// One pass to learn which session each offset belongs to, one to emit an edge
/// for every `re=` that points across a session boundary. A `re=` naming an
/// offset this reader has not seen — outside the page, or a record no session
/// claims — yields no edge: an edge to a record that is not there would let a
/// sender invent a constraint on every future cut.
#[must_use]
pub fn cross_edges_from_bus(records: &[Record]) -> Vec<CrossEdge> {
    let hosts = Hosts::from_bus(records);
    let mut owner: BTreeMap<u64, String> = BTreeMap::new();
    for (off, subject, body) in records {
        if let Some(session) = attributed_session_of(subject, body, &hosts) {
            owner.insert(*off, session);
        }
    }
    let mut edges = Vec::new();
    for (off, subject, body) in records {
        let Some(effect) = attributed_session_of(subject, body, &hosts) else {
            continue;
        };
        let Some(re) = Body::decode(body).0.re else {
            continue;
        };
        let Some(cause) = owner.get(&re) else {
            continue;
        };
        if *cause != effect {
            edges.push(CrossEdge {
                cause: (cause.clone(), re),
                effect: (effect, *off),
            });
        }
    }
    edges
}

/// A session's screen as the bus recorded it — the `screen` face's last frame at
/// or below a cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// The offset the frame was published at.
    pub at: u64,
    /// `<content_seq>:<fp16>` — the generation §6.6 fences a keystroke on, so a
    /// re-folded screen can be matched against the `gen=` a driver used.
    pub gen: String,
    /// The visible rows, as the `text --json` frame carried them.
    pub rows: String,
}

impl Screen {
    /// Whether the re-folded screen shows `needle`.
    #[must_use]
    pub fn contains(&self, needle: &str) -> bool {
        self.rows.contains(needle)
    }
}

/// One session, replayed to a cut.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CutReplay {
    /// The last offset actually folded, or `None` when the cut names this
    /// session but its log reached nothing. Compare it with the cut offset to
    /// know whether the log reached the cut or ended before it.
    pub folded_through: Option<u64>,
    /// The screen at the cut. `None` when the session never published one — the
    /// `screen` face is opt-in (§3.3), so its absence is a configuration fact,
    /// never a replay failure.
    pub screen: Option<Screen>,
    /// Who held the keyboard at the cut, folded from the last-value `control`
    /// row.
    pub holder: Option<String>,
    /// Whether the session was held at the cut, folded from its presence row.
    pub held: bool,
    /// Every record of this session the cut admits, in offset order.
    pub records: Vec<Record>,
}

/// Replay one session from the bus, folding every record the cut admits.
///
/// # Errors
///
/// [`ReplayError::UnknownVersion`] when a record inside the cut carries a `v=`
/// this reader does not implement (§10). The replay stops there rather than
/// folding over it: a checkpoint that silently skipped what it could not read
/// would be a shorter screen that looks valid.
pub fn replay_to_cut(
    cut: &Cut,
    session: &str,
    records: &[Record],
) -> Result<CutReplay, ReplayError> {
    let mut out = CutReplay::default();
    let Some(cut_off) = cut.offset_of(session) else {
        return Ok(out);
    };
    let hosts = Hosts::from_bus(records);
    for (off, subject, raw) in records {
        if *off > cut_off {
            break;
        }
        if attributed_session_of(subject, raw, &hosts).as_deref() != Some(session) {
            continue;
        }
        let (body, tail) = Body::decode(raw);
        if body.v != FOLD_V {
            return Err(ReplayError::UnknownVersion {
                at: *off,
                v: body.v,
            });
        }
        out.folded_through = Some(*off);
        let leaf = subject.rsplit('/').next().unwrap_or_default();
        match leaf {
            // LAST-VALUE FACES. Each is a fold whose answer is simply the newest
            // record at or below the cut, which is what "last-value" means and
            // why a cut over them needs no state of its own.
            "screen" => {
                out.screen = Some(Screen {
                    at: *off,
                    gen: body.gen.clone().unwrap_or_else(|| "-".into()),
                    rows: String::from_utf8_lossy(&tail.unwrap_or_default()).into_owned(),
                });
            }
            "control" => {
                out.holder = body.unknown.get("holder").filter(|h| *h != "-").cloned();
            }
            "presence" => {
                out.held = body.unknown.get("hold").is_some_and(|h| h == "1");
            }
            _ => {}
        }
        out.records.push((*off, subject.clone(), raw.clone()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(off: u64, subject: &str, body: &str) -> Record {
        (off, subject.to_string(), body.as_bytes().to_vec())
    }

    /// A record's session is read BY POSITION, and a principal's lane falls back
    /// to the ATTESTED `from=` rather than to a guess. An over-long subject
    /// whose tail reads like a session belongs to no session at all — the same
    /// refusal `parse_in` makes at the delivery seam, for the same reason.
    #[test]
    fn a_records_session_is_read_by_position_and_from_is_the_only_body_fallback() {
        assert_eq!(
            session_of("/f/f1/pub/n-a/s-worker/control", b"v=1 t=1"),
            Some("s-worker".to_string())
        );
        assert_eq!(
            session_of("/f/f1/term/n-a/s-worker/screen", b"v=1 t=1"),
            Some("s-worker".to_string())
        );
        // THE TWO ADDRESSEE-CHOSEN FACES ATTEST NOTHING. The cap binds the
        // `<src>`, so the sid is the sender's own text; `session_of` says so by
        // answering `None`, and the claim is read by `addressed_session_of`.
        for addressed in [
            "/f/f1/term/n-a/s-worker/in/h-andrew",
            "/f/f1/in/n-a/s-worker/h-andrew/answer",
        ] {
            assert_eq!(session_of(addressed, b"v=1 t=1"), None, "{addressed}");
            assert_eq!(
                addressed_session_of(addressed),
                Some(("n-a".to_string(), "s-worker".to_string())),
                "{addressed}"
            );
        }
        // The drive face's EIGHT segments are the drive face; the seven-segment
        // `term` leaves are the node's own, and no other leaf is either.
        assert_eq!(
            session_of("/f/f1/term/n-a/s-worker/inbox", b"v=1 t=1"),
            None
        );
        assert_eq!(addressed_session_of("/f/f1/term/n-a/s-worker/at/h-a"), None);
        // A node's own faces name no session.
        assert_eq!(session_of("/f/f1/pub/n-a/node/ev", b"v=1 t=1"), None);
        assert_eq!(session_of("/f/f1/fleet/h-andrew/halt", b"v=1 t=1"), None);
        // A human's lane: the attested `from=`, or nothing.
        assert_eq!(
            session_of("/f/f1/in/p/h-andrew/n-a/ask", b"v=1 t=1 from=s-agent"),
            Some("s-agent".to_string())
        );
        assert_eq!(session_of("/f/f1/in/p/h-andrew/n-a/ask", b"v=1 t=1"), None);
        // A `from=` that is not a session principal is not one.
        assert_eq!(
            session_of("/f/f1/in/p/h-andrew/n-a/ask", b"v=1 t=1 from=h-andrew"),
            None
        );
        // EIGHT SEGMENTS: whatever its tail spells, it is not an `in` record.
        assert_eq!(
            session_of("/f/f1/in/n-a/s-b/s-1/h-andrew/answer", b"v=1 t=1"),
            None
        );
        // A `from=` IS ONLY EVER ATTESTED ON THE PRINCIPAL LANE, UNDER A NODE.
        // Each of these is eight segments and reaches the same fall-through the
        // human's lane does, and each names no session: the addressee is a node
        // rather than the `p` literal; the `<src>` is a peer session; the
        // `<src>` is a human; the addressee position holds a reserved literal
        // instead of a principal. A body's `from=` cannot buy any of them a
        // session it never held.
        for forged in [
            "/f/f1/in/n-a/n-b/s-evil/note",
            "/f/f1/in/p/h-andrew/s-evil/note",
            "/f/f1/in/p/h-andrew/h-evil/note",
            "/f/f1/in/p/node/n-a/note",
        ] {
            assert_eq!(
                session_of(forged, b"v=1 t=1 from=s-victim"),
                None,
                "{forged}"
            );
        }
    }

    /// A STRANGER CANNOT INVENT A CONSTRAINT ON A SESSION THEY DO NOT HOST.
    ///
    /// The forged record sits on an `in` lane addressed to a NODE, published
    /// under the peer's own cap-forced `<src>`, and its body claims the
    /// victim's session. Before the `<src>`/lane test, `session_of` believed
    /// it: offset 11 entered the owner map as the victim's, and the worker's
    /// `re=11` became a cross-edge whose cause was a session that never wrote a
    /// byte — so `is_consistent` would reject every future cut that admitted
    /// the worker's keystroke without the victim's phantom, and `replay_to_cut`
    /// folded the stranger's record into the victim's own history.
    #[test]
    fn a_claimed_from_under_a_strangers_src_owns_no_offset() {
        let bus = vec![
            rec(
                11,
                "/f/f1/in/n-a/n-b/s-evil/note",
                "v=1 t=1 from=s-victim text=x",
            ),
            rec(
                12,
                "/f/f1/term/n-2/s-worker/in/h-a",
                "v=1 t=1 re=11 len=2\nhi",
            ),
        ];
        assert_eq!(cross_edges_from_bus(&bus), vec![]);

        let mut cut = Cut::default();
        cut.set("s-victim", 99);
        let folded = replay_to_cut(&cut, "s-victim", &bus).expect("fold");
        assert_eq!(folded.records, vec![]);
        assert_eq!(folded.folded_through, None);
    }

    /// **A STRANGER CANNOT FILE A RECORD UNDER A SESSION IT DOES NOT HOST — ON
    /// ANY FACE.**
    ///
    /// The header's "what it cannot do is … forge whose session it is on" was
    /// true of exactly one of the four faces that name a session. Round 1 pinned
    /// the principal lane; this is the rest of the class, and every row of it is
    /// inside an ordinary §8.2 ring:
    ///
    /// * `in/<node>/<sid>/<src>/<kind>` — the cap binds the `<src>` at 6, so
    ///   `rw,p=n-rogue:/f/f1/in/*/*/n-rogue/*` matches ANY node and ANY sid at 4
    ///   and 5. Two such records with a `re=` between them invented a permanent
    ///   `CrossEdge` between two sessions the rogue has no relationship with,
    ///   and `is_consistent` then rejected every cut that admitted one without
    ///   the other.
    /// * `term/<node>/<sid>/in/<src>` — the same shape one face over: the cap
    ///   binds the `<src>` at 7, and `session_of` read the sid at 5 anyway.
    /// * `pub/<node>/<sid>/<leaf>` — a node's cap covers its whole `pub/<n>/`
    ///   subtree (§6.1 says so in as many words), so a rogue could publish a
    ///   `control` row for somebody else's sid and `replay_to_cut` folded it as
    ///   that session's holder.
    ///
    /// The pin is §6.1's, read off the bus: node 1 claimed the sid first, at a
    /// lower offset, so every later claim by anyone else is refused and listed.
    #[test]
    fn a_stranger_cannot_file_a_record_under_a_session_it_does_not_host() {
        const VICTIM: &str = "s-victim0000";
        const OTHER: &str = "s-other0000";
        let bus = vec![
            // The host claims its own session, first, under its own cap.
            rec(
                1,
                "/f/f1/pub/n-host/s-victim0000/presence",
                "v=1 t=1 state=live",
            ),
            rec(
                2,
                "/f/f1/pub/n-host/s-victim0000/control",
                "v=1 t=1 holder=h-andrew",
            ),
            // Every row below is the rogue, inside its own ring.
            rec(
                10,
                "/f/f1/in/n-rogue/s-victim0000/n-rogue/note",
                "v=1 t=1 text=x",
            ),
            rec(
                11,
                "/f/f1/in/n-rogue/s-other0000/n-rogue/note",
                "v=1 t=1 re=10 text=y",
            ),
            rec(
                12,
                "/f/f1/term/n-rogue/s-victim0000/in/n-rogue",
                "v=1 t=1 len=2\nhi",
            ),
            rec(
                13,
                "/f/f1/pub/n-rogue/s-victim0000/control",
                "v=1 t=1 holder=n-rogue",
            ),
        ];

        // THE PIN, AND THE CONFLICT THAT DOES NOT MOVE IT.
        let hosts = Hosts::from_bus(&bus);
        assert_eq!(hosts.host_of(VICTIM), Some("n-host"));
        assert_eq!(hosts.host_of(OTHER), None, "nobody ever claimed to host it");
        assert_eq!(hosts.conflicts(), vec![VICTIM], "the second claim is LOUD");

        // NOT ONE OF THE ROGUE'S ROWS JOINS A SESSION'S LOG.
        for (_, subject, body) in &bus[2..] {
            assert_eq!(
                attributed_session_of(subject, body, &hosts),
                None,
                "{subject}"
            );
        }

        // NO INVENTED EDGE, and no invented partition to hang one on.
        assert_eq!(cross_edges_from_bus(&bus), vec![]);
        assert_eq!(
            Cut::flat(&bus, 99).offsets,
            vec![(VICTIM.to_string(), 99)],
            "a session only the rogue ever named is not a partition"
        );

        // AND THE VICTIM'S OWN HISTORY IS ITS OWN: the fold stops at the two
        // rows its host published, and the holder is the one its host wrote.
        let mut cut = Cut::default();
        cut.set(VICTIM, 99);
        let folded = replay_to_cut(&cut, VICTIM, &bus).expect("fold");
        assert_eq!(folded.holder.as_deref(), Some("h-andrew"));
        assert_eq!(
            folded
                .records
                .iter()
                .map(|(o, _, _)| *o)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    /// **FIRST SIGHT PINS, BY BUS OFFSET, AND ONLY A NODE'S OWN CAP-FORCED
    /// SEGMENT IS A CLAIM.**
    ///
    /// The order `records` happens to arrive in is not the order the bus is in,
    /// so the fold is over offsets — two auditors paging the same log
    /// differently must pin identically or their cuts disagree. And the claim
    /// itself comes only from a position a node could not have typed for
    /// somebody else: the `<owner>` of an owner face, or the `<src>` of a
    /// principal lane whose body says `from=`.
    #[test]
    fn a_hosting_pin_is_first_sight_by_offset_and_comes_only_from_a_cap_forced_node() {
        let out_of_order = vec![
            rec(20, "/f/f1/pub/n-late/s-w0/presence", "v=1 t=1"),
            rec(5, "/f/f1/pub/n-first/s-w0/presence", "v=1 t=1"),
            // A principal's lane: the `<src>` at 6 is the node, so `from=` is
            // that node saying "one of mine".
            rec(6, "/f/f1/in/p/h-a/n-first/ask", "v=1 t=1 from=s-w1 text=hi"),
            // A sender-chosen face names a node and a sid and claims NOTHING.
            rec(7, "/f/f1/in/n-ghost/s-w2/n-rogue/note", "v=1 t=1"),
            rec(8, "/f/f1/term/n-ghost/s-w2/in/n-rogue", "v=1 t=1 len=1\nx"),
            // A `from=` under a HUMAN's cap-forced `<src>` is a claim, not an
            // attestation, and buys no pin either.
            rec(9, "/f/f1/in/p/h-a/h-b/ask", "v=1 t=1 from=s-w3 text=hi"),
        ];
        let hosts = Hosts::from_bus(&out_of_order);
        assert_eq!(hosts.host_of("s-w0"), Some("n-first"), "lowest offset wins");
        assert_eq!(hosts.conflicts(), vec!["s-w0"]);
        assert_eq!(hosts.host_of("s-w1"), Some("n-first"));
        assert_eq!(hosts.host_of("s-w2"), None);
        assert_eq!(hosts.host_of("s-w3"), None);
    }

    /// The edges are CROSS-SESSION only, they come off `re=`, and a `re=` at an
    /// offset no session claims yields nothing — otherwise a sender could invent
    /// a constraint on every cut by pointing at a number.
    #[test]
    fn the_edges_are_cross_session_and_a_dangling_re_makes_none() {
        let bus = vec![
            // THE HOSTS, ATTESTED. `s-worker` is only ever named on faces whose
            // sid segment the sender types, so without node 2's own claim it is
            // attributed nothing at all — see
            // `a_stranger_cannot_file_a_record_under_a_session_it_does_not_host`.
            rec(9, "/f/f1/pub/n-2/s-worker/presence", "v=1 t=1 state=live"),
            rec(
                10,
                "/f/f1/in/p/h-a/n-1/ask",
                "v=1 t=1 from=s-agent text=which",
            ),
            // Same session: correlation, not a cut constraint.
            rec(
                11,
                "/f/f1/in/n-1/s-agent/h-a/answer",
                "v=1 t=1 re=10 text=main",
            ),
            // Across sessions: the keystroke on the WORKER that answers the
            // AGENT's ask. This is the composition edge.
            rec(
                12,
                "/f/f1/term/n-2/s-worker/in/h-a",
                "v=1 t=1 re=10 len=2\nhi",
            ),
            // A `re=` nobody's record sits at.
            rec(
                13,
                "/f/f1/term/n-2/s-worker/in/h-a",
                "v=1 t=1 re=9999 len=2\nno",
            ),
        ];
        let edges = cross_edges_from_bus(&bus);
        assert_eq!(
            edges,
            vec![CrossEdge {
                cause: ("s-agent".to_string(), 10),
                effect: ("s-worker".to_string(), 12),
            }]
        );

        // A cut that admits the effect must admit the cause. That is the whole
        // rule, and both directions of it are asserted.
        let mut cut = Cut::default();
        cut.set("s-agent", 10);
        cut.set("s-worker", 12);
        assert!(is_consistent(&cut, &edges));
        // The keystroke admitted, the ask that caused it not: an ORPHAN.
        cut.set("s-agent", 9);
        assert!(!is_consistent(&cut, &edges));
        // The ask admitted, the keystroke not: IN FLIGHT, and admissible.
        cut.set("s-agent", 10);
        cut.set("s-worker", 11);
        assert!(is_consistent(&cut, &edges));
        // A session the cut does not name admits nothing, so an edge whose
        // effect is unnamed constrains nothing.
        assert!(is_consistent(&Cut::default(), &edges));
    }

    /// A replay folds the last-value faces to their newest record at or below the
    /// cut, and a record it cannot read ENDS it rather than being skipped.
    #[test]
    fn a_replay_folds_the_last_value_faces_and_refuses_a_version_it_cannot_read() {
        let bus = vec![
            rec(1, "/f/f1/pub/n-2/s-worker/control", "v=1 t=1 holder=-"),
            rec(
                2,
                "/f/f1/term/n-2/s-worker/screen",
                "v=1 t=1 gen=4:aaaa len=8\n[\"idle\"]",
            ),
            rec(
                3,
                "/f/f1/pub/n-2/s-worker/control",
                "v=1 t=1 holder=h-andrew",
            ),
            rec(
                4,
                "/f/f1/term/n-2/s-worker/screen",
                "v=1 t=1 gen=9:bbbb len=10\n[\"A7MARK\"]",
            ),
            rec(5, "/f/f1/pub/n-2/s-worker/presence", "v=1 t=1 hold=1"),
        ];
        let mut cut = Cut::default();
        cut.set("s-worker", 3);
        let early = replay_to_cut(&cut, "s-worker", &bus).expect("fold");
        assert_eq!(early.folded_through, Some(3));
        assert_eq!(early.holder.as_deref(), Some("h-andrew"));
        assert!(!early.held);
        let screen = early.screen.expect("a screen at the cut");
        assert_eq!(screen.gen, "4:aaaa");
        assert!(!screen.contains("A7MARK"), "the cut folded a LATER screen");

        cut.set("s-worker", 5);
        let late = replay_to_cut(&cut, "s-worker", &bus).expect("fold");
        assert!(late.held);
        assert!(late.screen.expect("a screen").contains("A7MARK"));

        // A session the cut does not name folds empty rather than folding
        // everything — the same rule as astream's.
        let none = replay_to_cut(&cut, "s-other", &bus).expect("fold");
        assert_eq!(none.folded_through, None);
        assert!(none.records.is_empty());

        // §10'S LOUD DISAGREEMENT. A `v=` this reader does not implement stops
        // the replay; it is never folded over.
        let mut bad = bus.clone();
        bad.push(rec(
            6,
            "/f/f1/pub/n-2/s-worker/control",
            "v=2 t=1 holder=h-next",
        ));
        cut.set("s-worker", 6);
        assert_eq!(
            replay_to_cut(&cut, "s-worker", &bad),
            Err(ReplayError::UnknownVersion { at: 6, v: 2 })
        );
    }
    /// THE DOC ON [`attributed_session_of`] IS A CLAIM, and aterm has no
    /// evidence manifest to check it against — so it is checked here, against
    /// the code it describes.
    ///
    /// It used to read "A sid with no pin, or one two nodes claimed, is
    /// attributed NOTHING", and the second half was never true: `Hosts::from_bus`
    /// records the second claimant in `conflicts` and LEAVES THE FIRST PIN IN
    /// PLACE, `attributed_session_of` asks only `host_of`, and `conflicts` is
    /// read by nothing outside `Hosts::conflicts()`. The sentence was wrong in
    /// the direction an auditor trusts — it promised a conservative exclusion the
    /// machinery does not perform, so a reader debugging a rejected cut was told
    /// the contested session could not be the cause when it still binds every
    /// cut it ever bound.
    ///
    /// BOTH HALVES ARE PINNED: the sentence, and the behaviour it now describes.
    #[test]
    fn the_attribution_doc_describes_the_pin_a_conflict_does_not_move() {
        // THE BEHAVIOUR. `n-first` pins at offset 1; `n-late` contests at 2.
        const SID: &str = "s-contested0";
        let bus = vec![
            rec(
                1,
                "/f/f1/pub/n-first/s-contested0/presence",
                "v=1 t=1 state=live",
            ),
            rec(
                2,
                "/f/f1/pub/n-late/s-contested0/presence",
                "v=1 t=2 state=live",
            ),
            rec(
                3,
                "/f/f1/pub/n-first/s-contested0/control",
                "v=1 t=3 holder=h-a",
            ),
            rec(
                4,
                "/f/f1/pub/n-late/s-contested0/control",
                "v=1 t=4 holder=h-b",
            ),
        ];
        let hosts = Hosts::from_bus(&bus);
        assert_eq!(hosts.conflicts(), vec![SID], "the contest is LOUD");
        assert_eq!(
            attributed_session_of(&bus[2].1, &bus[2].2, &hosts).as_deref(),
            Some(SID),
            "the PINNED host's records still attribute — a second claim must not \
             be able to erase a live session's edges"
        );
        assert_eq!(
            attributed_session_of(&bus[3].1, &bus[3].2, &hosts),
            None,
            "the interloper's records attribute nothing"
        );

        // THE SENTENCE. `flat` is the doc comment of `attributed_session_of`,
        // read out of this file's own source.
        let src = include_str!("replay.rs");
        let at = src
            .find("pub fn attributed_session_of")
            .expect("the function is in this file");
        let doc: Vec<&str> = src[..at]
            .lines()
            .rev()
            .skip_while(|l| !l.trim_start().starts_with("///"))
            .take_while(|l| l.trim_start().starts_with("///"))
            .collect();
        let flat = doc
            .into_iter()
            .rev()
            .map(|l| l.trim_start().trim_start_matches("///"))
            .collect::<Vec<_>>()
            .join(" ")
            .replace('`', "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            !flat.contains("or one two nodes claimed, is attributed NOTHING"),
            "the doc promises an exclusion attributed_session_of does not \
             perform:\n{flat}"
        );
        assert!(
            flat.contains("A CONTESTED SID KEEPS ITS FIRST PIN"),
            "the doc must state what a conflict actually does:\n{flat}"
        );
    }
}
