// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet-wide halts over the mesh (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.11, §11 C4).
//!
//! - **A halt starts from a person on its own Mac.** `aterm fabric halt` writes the person's
//!   request on `/f/alab/fleet/<h-person>/halts` ([`Ask`]), which only that person's ring can
//!   write (an `a-` ring writes no `fleet/` subject at all). The Mac's fabric bridge, the one
//!   writer of its node's halt subject, publishes it as a record on `/f/alab/halt/<node>`
//!   ([`Said`]), origin-signed with the node key, and the mesh relays it to every Mac.
//! - **A halt is named by its id**, 32 hex digits the request mints ([`fresh_id`]). A halt-off
//!   names the id it lifts, and is applied from any current member's person: any member's halt
//!   applies, so any member's person can release it. Binding to the id keeps a relayed or
//!   replayed halt-off from lifting a later halt. A person at any Mac can also lift a halt there
//!   for that Mac alone ([`Ask::here`]): its bridge applies it and says it to no other Mac.
//! - **Receivers keep the ids they applied** ([`Halts`]): each standing halt, and the last
//!   [`LIFTED_MAX`] ids lifted here, never dropped by age, so a record the log, a neighbour or
//!   an origin that never heard the halt-off still holds changes nothing when it comes again, in
//!   any order.
//! - **Every record reaches a Mac that was away** ([`Halts::due`]): a Mac says each of its own
//!   standing halts again every [`RESAY_MS`], inside `bus.log`'s seven days, and each halt-off
//!   its person asked, or of each of its halts lifted for every Mac, for [`OFF_RESAY_SPAN_MS`];
//!   a Mac that lifted a halt for every Mac answers its origin's saying it again with the
//!   halt-off ([`Outcome::Answer`]).
//! - **A halt held past its origin's saying is asked about** ([`Word::Ask`]): a Mac holding
//!   another Mac's halt whose newest saying is [`STALE_MS`] old asks for it every [`RESAY_MS`],
//!   and a Mac that lifted it for every Mac answers with the halt-off. So a Mac away past
//!   [`OFF_RESAY_SPAN_MS`], after every log gave up the halt-off, still hears it, while no Mac
//!   says a halt-off for longer than that span. An ask keeps nothing where it is heard.
//! - **No halt outlives its origin.** The node service hands the bridge every node the
//!   membership log removed, or whose key a `Node` record rotated (the hand-off's `gone` lines,
//!   [`crate::federate::Handoff`]); every halt of such an origin is lifted, and nothing it said
//!   is applied again ([`Halts::lift_gone`]). On a Mac the log took out, its own halts lift once,
//!   and what its person asks after that stops its own sessions alone.
//! - **The old rows move** ([`carried_id`]): a halt a person set as a `/f/<F>/fleet/<h>/halt` row
//!   before the upgrade is carried into the kept ids under an id every run derives alike, and
//!   republished, so it stands after the upgrade and a halt-off naming that id lifts it.

use std::collections::{BTreeMap, BTreeSet};

use crate::body::Body;

/// The hex digits of a halt id: 128 bits.
pub const ID_HEX: usize = 32;

/// The longest reason a halt keeps, pct-encoded, as the hold carries it.
pub const REASON_MAX: usize = 128;

/// The reason a halt with none is held under.
pub const REASON_DEFAULT: &str = "fleet-halt";

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// How long a Mac waits to say one of its records again: under half `bus.log`'s seven days, so
/// every log holds a copy of each record it keeps saying, whatever it gave up.
pub const RESAY_MS: u64 = 3 * DAY_MS;

/// How long a Mac says again a halt-off its person asked, or that of its own halt lifted for every
/// Mac: three of `bus.log`'s seven days. A halt's origin away longer learns the halt-off from any
/// Mac that lifted it, which answers the origin's saying the halt again ([`Outcome::Answer`]);
/// any other Mac away longer asks for the halt once its saying is [`STALE_MS`] old
/// ([`Word::Ask`]), and a Mac that lifted it answers with the halt-off. The span is bounded on
/// purpose: a halt-off said again for as long as the lift
/// is kept would come back, stamped anew, on every Mac that had dropped its id past
/// [`LIFTED_MAX`], pushing out the oldest kept ids there, a halt lifted on that Mac alone among
/// them, which its origin's next saying would raise again; and every bridge would say all of its
/// kept halt-offs at its attach, before it holds its sessions.
pub const OFF_RESAY_SPAN_MS: u64 = 21 * DAY_MS;

/// How old the newest saying of another Mac's halt standing here is before this Mac asks for it
/// ([`Word::Ask`]): [`RESAY_MS`] and a day for the clocks' skew and a slow relay. Its origin says
/// it again every [`RESAY_MS`] while it stands there, so a saying this old means the origin is
/// away, or lifted it and this Mac never heard the halt-off (`docs/DESIGN-alab-one-mesh-2026-11-05.md`
/// §9 `FabricHalts`, the case C4 left open).
pub const STALE_MS: u64 = RESAY_MS + DAY_MS;

/// The most halts kept standing. A halt past it is refused and said, never an older one dropped:
/// dropping a standing halt would lift it.
pub const STANDING_MAX: usize = 256;

/// The most lifted ids kept; past it the oldest go first, and never one by age alone.
pub const LIFTED_MAX: usize = 4096;

/// The state file's name in the bridge's state dir, and its first line.
pub const STATE_FILE: &str = "mesh-halts";
const STATE_HEAD: &str = "mesh-halts v1";

/// Whether `id` is a halt id: [`ID_HEX`] lowercase hex digits.
#[must_use]
pub fn is_id(id: &str) -> bool {
    aterm_codec::hex::is_lower(id, ID_HEX)
}

/// A fresh halt id from the OS CSPRNG.
///
/// # Errors
/// Where the OS refuses random bytes.
pub fn fresh_id() -> std::io::Result<String> {
    aterm_uds::rand::hex_token::<{ ID_HEX / 2 }>()
}

/// The id a halt set as an old row (`/f/<fleet>/fleet/<human>/halt`) on the Mac `node` carries
/// over under: the same on every run, so a move cut short and run again names it alike, and
/// distinct per Mac, fleet and person, the most that ever stood together before the upgrade
/// (one row per person per fleet).
#[must_use]
pub fn carried_id(node: &str, fleet: &str, human: &str) -> String {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-link carried halt v1\0");
    for part in [node, fleet, human] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    let digest = hash.finalize();
    aterm_codec::hex::encode_lower(&digest[..ID_HEX / 2])
}

/// A reason as a hold carries it: printable, at most [`REASON_MAX`] bytes, never cut inside an
/// escape, and [`REASON_DEFAULT`] when nothing is left.
#[must_use]
pub fn reason_token(raw: Option<&str>) -> String {
    let out = crate::presence::clamp_pct(raw.unwrap_or_default(), REASON_MAX);
    if out.is_empty() {
        REASON_DEFAULT.to_string()
    } else {
        out
    }
}

/// What a person asks of their own Mac's bridge: put a halt on, or lift one, by its id; and, said
/// by a bridge alone, an ask for a halt it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Word {
    /// Halt, for this reason (pct-encoded, [`reason_token`]).
    On(String),
    /// Lift.
    Off,
    /// Is this halt, which stands here and whose origin has not said it for [`STALE_MS`], still
    /// standing? A Mac that lifted it for every Mac answers with the halt-off
    /// ([`Outcome::Answer`]); a Mac that did not keeps nothing of it. Said by a bridge on its own
    /// halt subject ([`Halts::due`]), `by` the person who asked the halt; never a person's request.
    Ask,
}

/// A person's request on `/f/alab/fleet/<h-person>/halts`:
/// `v=1 t=<ms> state=on id=<id> reason=<pct>` or `v=1 t=<ms> state=off id=<id>`, the latter with
/// `scope=here` for a halt-off on this Mac alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    /// The halt it names.
    pub id: String,
    /// What it asks.
    pub word: Word,
    /// A halt-off for this Mac alone (§4.11: "A person at any Mac can also lift a halt there for
    /// that Mac"): its bridge applies it and says it to no other Mac. Never set on a halt.
    pub here: bool,
}

impl Ask {
    /// Its body, stamped `t_ms`.
    #[must_use]
    pub fn render(&self, t_ms: u64) -> String {
        match &self.word {
            Word::On(reason) => format!("v=1 t={t_ms} state=on id={} reason={reason}", self.id),
            Word::Off if self.here => format!("v=1 t={t_ms} state=off id={} scope=here", self.id),
            Word::Off => format!("v=1 t={t_ms} state=off id={}", self.id),
            Word::Ask => format!("v=1 t={t_ms} state=ask id={}", self.id),
        }
    }

    /// Read a request's body. `None` for one with no halt id or no `state=on|off`. A `scope=`
    /// other than `here` asks for every Mac, as one with none does; a halt is never for one Mac.
    #[must_use]
    pub fn parse(body: &[u8]) -> Option<Self> {
        let mut body = Body::decode(body).0;
        let mut ask = Self::of(&mut body)?;
        ask.here =
            ask.word == Word::Off && body.unknown.get("scope").map(String::as_str) == Some("here");
        Some(ask)
    }

    /// The id and word a decoded body names, taken out of it.
    fn of(body: &mut Body) -> Option<Self> {
        let id = body.unknown.remove("id").filter(|id| is_id(id))?;
        let word = match body.unknown.remove("state").as_deref() {
            Some("on") => Word::On(reason_token(body.unknown.remove("reason").as_deref())),
            Some("off") => Word::Off,
            _ => return None,
        };
        Some(Self {
            id,
            word,
            here: false,
        })
    }
}

/// What a Mac said on its halt subject (`/f/alab/halt/<origin>`), the payload of the origin's
/// envelope: `v=1 t=<ms> state=on id=<id> by=<h-person> reason=<pct>`, or `state=off` with no
/// reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said {
    /// The halt it names.
    pub id: String,
    /// The person who asked it, on the origin.
    pub by: String,
    /// Halt or lift.
    pub word: Word,
    /// The origin's wall clock when it said it.
    pub t_ms: u64,
}

/// Why a record on a halt subject changed nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Its body names no halt id, no `state=on|off`, or no `by=`.
    Malformed,
    /// It was asked by no person: only an `h-` principal halts.
    NotAPerson,
    /// Its origin was removed, or its key rotated: nothing it said applies.
    OriginGone,
    /// [`STANDING_MAX`] halts stand already.
    Full,
}

impl Refused {
    /// Its word in a journal line.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::NotAPerson => "not-a-person",
            Self::OriginGone => "origin-gone",
            Self::Full => "full",
        }
    }
}

impl Said {
    /// Its payload.
    #[must_use]
    pub fn render(&self) -> String {
        match &self.word {
            Word::On(reason) => format!(
                "v=1 t={} state=on id={} by={} reason={reason}",
                self.t_ms, self.id, self.by
            ),
            Word::Off => format!(
                "v=1 t={} state=off id={} by={}",
                self.t_ms, self.id, self.by
            ),
            Word::Ask => format!(
                "v=1 t={} state=ask id={} by={}",
                self.t_ms, self.id, self.by
            ),
        }
    }

    /// Read a payload.
    ///
    /// # Errors
    /// [`Refused::Malformed`] for one with no halt id, state or `by=`;
    /// [`Refused::NotAPerson`] for a `by=` that is no `h-` principal.
    pub fn parse(payload: &[u8]) -> Result<Self, Refused> {
        let (mut body, _) = Body::decode(payload);
        let asked = body.unknown.get("state").map(String::as_str) == Some("ask");
        if asked {
            body.unknown.insert("state".to_owned(), "off".to_owned());
        }
        let Some(Ask { id, mut word, .. }) = Ask::of(&mut body) else {
            return Err(Refused::Malformed);
        };
        if asked {
            word = Word::Ask;
        }
        let by = body.unknown.remove("by").ok_or(Refused::Malformed)?;
        if !crate::subject::is_principal(&by) {
            return Err(Refused::Malformed);
        }
        if !by.starts_with("h-") && !any_principal() {
            return Err(Refused::NotAPerson);
        }
        Ok(Self {
            id,
            by,
            word,
            t_ms: body.t,
        })
    }
}

/// One halt that stands here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Standing {
    /// The Mac whose bridge said it.
    pub origin: String,
    /// The person who asked it there.
    pub by: String,
    /// Its reason, pct-encoded ([`reason_token`]).
    pub reason: String,
    /// When this Mac applied it (its wall clock, ms).
    pub at_ms: u64,
    /// The newest stamp its origin said it under (the origin's wall clock, ms), so a halt said
    /// again after a lift is told apart from a record of it from before ([`Outcome::Answer`]).
    pub t_ms: u64,
}

/// How an id came to be lifted here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lift {
    /// A person here lifted it for this Mac alone ([`Ask::here`]).
    Here {
        /// The person.
        by: String,
    },
    /// A halt-off for every Mac lifted it.
    Fleet {
        /// The person who asked it.
        by: String,
        /// Whether this Mac's bridge said it, its own person's: it says it again for
        /// [`OFF_RESAY_SPAN_MS`] ([`Halts::due`]).
        said: bool,
    },
    /// Its origin was removed, or its key rotated ([`Halts::lift_gone`]).
    Gone,
}

/// One id lifted here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lifted {
    /// How.
    pub how: Lift,
    /// When (this Mac's wall clock, ms).
    pub at_ms: u64,
    /// The halt it lifted, where it stood here: so its origin goes on saying a halt it lifted
    /// for itself alone, and says the halt-off of one lifted for every Mac ([`Halts::due`]).
    pub was: Option<Standing>,
}

/// What applying a record did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A halt stands that did not.
    Halted,
    /// A standing halt was lifted.
    Lifted,
    /// No halt changed, and what is kept did: a lift kept ahead of its halt, a lift for this
    /// Mac alone made one for every Mac, or a halt said again under a newer stamp.
    Noted,
    /// It was applied before, names a halt lifted here, or asks for a halt this Mac did not lift
    /// for every Mac, or for one of its own ([`Word::Ask`]): nothing changed.
    Already,
    /// A halt said again, or asked for ([`Word::Ask`]), after this Mac lifted it for every Mac,
    /// `by` that person: the Mac that said it may never have heard the halt-off, so this Mac
    /// says the halt-off again (§11 C4).
    Answer {
        /// The person who asked the halt-off.
        by: String,
    },
    /// It applies to nothing, and why.
    Refused(Refused),
}

impl Outcome {
    /// Whether the halts that stand changed.
    #[must_use]
    pub const fn changed(&self) -> bool {
        matches!(self, Self::Halted | Self::Lifted)
    }

    /// Whether what this Mac keeps changed: the state file is written again.
    #[must_use]
    pub const fn kept(&self) -> bool {
        matches!(self, Self::Halted | Self::Lifted | Self::Noted)
    }
}

/// The halts this Mac applied: those that stand, the [`LIFTED_MAX`] ids lifted here last,
/// whether the old rows moved in already, and whether the membership log took this Mac out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Halts {
    standing: BTreeMap<String, Standing>,
    lifted: BTreeMap<String, Lifted>,
    moved: bool,
    out: bool,
}

impl Halts {
    /// Apply what `origin` said (`said`), at `now_ms`, on the Mac `me`. A halt-off lifts the halt
    /// its id names from whichever Mac and person it comes; an id lifted here is never raised
    /// again, and a halt said again after a halt-off lifted it for every Mac is answered
    /// ([`Outcome::Answer`]), as is an ask for a halt lifted here for every Mac ([`Word::Ask`]),
    /// which keeps nothing otherwise; nothing another node in `gone` said is applied. What this Mac's own
    /// bridge said applies here even once the log took this Mac out: it reaches no other Mac, and
    /// a person here still stops this Mac's own sessions.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "FabricHalts",
            action = "HaltA",
            project = "aterm_link::federate::conformance::halts_projection"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "FabricHalts",
            action = "HaltC",
            project = "aterm_link::federate::conformance::halts_projection"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "FabricHalts",
            action = "OffB1",
            project = "aterm_link::federate::conformance::halts_projection"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "FabricHalts",
            action = "OffA2",
            project = "aterm_link::federate::conformance::halts_projection"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "FabricHalts",
            action = "HearC",
            project = "aterm_link::federate::conformance::halts_projection"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "FabricHalts",
            action = "HearAB",
            project = "aterm_link::federate::conformance::halts_projection"
        )
    )]
    pub fn apply(
        &mut self,
        me: &str,
        origin: &str,
        said: &Said,
        gone: &BTreeSet<String>,
        now_ms: u64,
    ) -> Outcome {
        if origin != me && gone.contains(origin) && !no_lift_on_gone() {
            return Outcome::Refused(Refused::OriginGone);
        }
        match &said.word {
            Word::On(reason) => {
                if let Some(halt) = self.standing.get_mut(&said.id) {
                    if halt.origin == origin && said.t_ms > halt.t_ms {
                        halt.t_ms = said.t_ms;
                        return Outcome::Noted;
                    }
                    return Outcome::Already;
                }
                if let Some(lifted) = self.lifted.get(&said.id) {
                    // Said under a newer stamp than any this Mac had of it before the lift (in the
                    // origin's own clock; this Mac's when the halt never stood here): the origin
                    // says it again, so it holds it still.
                    let before = lifted.was.as_ref().map_or(lifted.at_ms, |was| was.t_ms);
                    return match &lifted.how {
                        Lift::Fleet { by, .. } if origin != me && said.t_ms > before => {
                            Outcome::Answer { by: by.clone() }
                        }
                        _ => Outcome::Already,
                    };
                }
                if self.standing.len() >= STANDING_MAX {
                    return Outcome::Refused(Refused::Full);
                }
                self.standing.insert(
                    said.id.clone(),
                    Standing {
                        origin: origin.to_string(),
                        by: said.by.clone(),
                        reason: reason.clone(),
                        at_ms: now_ms,
                        t_ms: said.t_ms,
                    },
                );
                Outcome::Halted
            }
            Word::Ask => match self.lifted.get(&said.id) {
                Some(Lifted {
                    how: Lift::Fleet { by, .. },
                    ..
                }) if origin != me => Outcome::Answer { by: by.clone() },
                _ => Outcome::Already,
            },
            Word::Off => {
                let how = Lift::Fleet {
                    by: said.by.clone(),
                    said: origin == me,
                };
                if off_lifts_every() {
                    let ids: Vec<String> = self.standing.keys().cloned().collect();
                    let lifted = !ids.is_empty();
                    for id in ids.iter().chain([&said.id]) {
                        self.lift(id, how.clone(), now_ms);
                    }
                    return if lifted {
                        Outcome::Lifted
                    } else {
                        Outcome::Noted
                    };
                }
                if off_from_its_origin_only()
                    && self
                        .standing
                        .get(&said.id)
                        .is_some_and(|halt| halt.origin != origin || halt.by != said.by)
                {
                    return Outcome::Refused(Refused::NotAPerson);
                }
                if self.lifted_everywhere(&said.id) {
                    return Outcome::Already;
                }
                // A halt-off that comes before its halt keeps the id lifted too, so the halt
                // never stands when it arrives; one for an id lifted here alone makes the lift
                // every Mac's.
                if self.lift(&said.id, how, now_ms) {
                    Outcome::Lifted
                } else {
                    Outcome::Noted
                }
            }
        }
    }

    /// Lift `id` on this Mac alone, `by` its person (§4.11: "A person at any Mac can also lift a
    /// halt there for that Mac"), unless it was lifted here already.
    pub fn lift_here(&mut self, id: &str, by: &str, now_ms: u64) -> Outcome {
        if self.lifted.contains_key(id) {
            return Outcome::Already;
        }
        let how = Lift::Here { by: by.to_owned() };
        if self.lift(id, how, now_ms) {
            Outcome::Lifted
        } else {
            Outcome::Noted
        }
    }

    /// Lift `id` as `how` says, the lift replacing any kept before and keeping the halt it
    /// lifted: answers whether it stood.
    fn lift(&mut self, id: &str, how: Lift, now_ms: u64) -> bool {
        let stood = self.standing.remove(id);
        let was = stood
            .clone()
            .or_else(|| self.lifted.get(id).and_then(|lifted| lifted.was.clone()));
        self.lifted.insert(
            id.to_string(),
            Lifted {
                how,
                at_ms: now_ms,
                was,
            },
        );
        self.prune();
        stood.is_some()
    }

    /// Lift every halt whose origin is in `gone` (§4.11, "No halt outlives its origin"),
    /// answering the ids lifted. This Mac's own (`me`) lift once, when `gone` first names it:
    /// what its person asks after that stands on this Mac alone, until lifted.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "FabricHalts",
            action = "Revoke",
            project = "aterm_link::federate::conformance::halts_projection"
        )
    )]
    pub fn lift_gone(&mut self, me: &str, gone: &BTreeSet<String>, now_ms: u64) -> Vec<String> {
        if no_lift_on_gone() {
            return Vec::new();
        }
        let newly_out = gone.contains(me) && !self.out;
        self.out = gone.contains(me);
        let ids: Vec<String> = self
            .standing
            .iter()
            .filter(|(_, halt)| {
                if halt.origin == me {
                    newly_out
                } else {
                    gone.contains(&halt.origin)
                }
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            self.lift(id, Lift::Gone, now_ms);
        }
        ids
    }

    /// Past [`LIFTED_MAX`] lifted ids, drop the oldest. Never by age alone: an origin that never
    /// heard a halt-off says its halt again however late, and the id must still read lifted.
    fn prune(&mut self) {
        if self.lifted.len() <= LIFTED_MAX {
            return;
        }
        let mut by_age: Vec<(u64, String)> = self
            .lifted
            .iter()
            .map(|(id, lifted)| (lifted.at_ms, id.clone()))
            .collect();
        by_age.sort();
        for (_, id) in by_age.into_iter().take(self.lifted.len() - LIFTED_MAX) {
            self.lifted.remove(&id);
        }
    }

    /// What this Mac (`me`) says again at `now_ms`, each stamped `now_ms`, `said_at` holding when
    /// it last said each id (one it never said, or whose record no log it reads still holds, is
    /// missing), once [`RESAY_MS`] passed since it said it last: each halt it said that stands
    /// here or was lifted here alone, and within [`OFF_RESAY_SPAN_MS`] of the lift each halt-off
    /// its person asked and that of each of its halts lifted for every Mac. So a Mac that was
    /// away while a record went round, and found it gone from every log, hears it. And, while
    /// the log has this Mac in, an ask ([`Word::Ask`]) for each halt of another Mac standing here
    /// whose newest saying is [`STALE_MS`] old, `by` its person, the same [`RESAY_MS`] apart: a
    /// Mac away past [`OFF_RESAY_SPAN_MS`] learns a halt-off no log holds any more from a Mac
    /// that lifted it ([`Outcome::Answer`]).
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "FabricHalts",
            action = "ResayAB",
            project = "aterm_link::federate::conformance::halts_projection"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "FabricHalts",
            action = "ResayC",
            project = "aterm_link::federate::conformance::halts_projection"
        )
    )]
    pub fn due(&self, me: &str, said_at: &BTreeMap<String, u64>, now_ms: u64) -> Vec<Said> {
        let due = |id: &String| {
            said_at
                .get(id)
                .is_none_or(|at| now_ms >= at.saturating_add(RESAY_MS))
        };
        let halt = |id: &String, halt: &Standing| Said {
            id: id.clone(),
            by: halt.by.clone(),
            word: Word::On(halt.reason.clone()),
            t_ms: now_ms,
        };
        let mine = |was: &Option<Standing>| was.as_ref().is_some_and(|was| was.origin == me);
        let halts = self
            .standing
            .iter()
            .filter(|(id, halt)| halt.origin == me && due(id))
            .map(|(id, standing)| halt(id, standing));
        let asks = self
            .standing
            .iter()
            .filter(|(id, halt)| {
                !self.out
                    && !no_stale_ask()
                    && halt.origin != me
                    && now_ms >= halt.t_ms.saturating_add(STALE_MS)
                    && due(id)
            })
            .map(|(id, standing)| Said {
                id: id.clone(),
                by: standing.by.clone(),
                word: Word::Ask,
                t_ms: now_ms,
            });
        let others = self.lifted.iter().filter_map(|(id, lifted)| {
            if !due(id) {
                return None;
            }
            match (&lifted.how, &lifted.was) {
                (Lift::Here { .. }, Some(was)) if was.origin == me => Some(halt(id, was)),
                (Lift::Fleet { by, said }, was)
                    if (*said || mine(was))
                        && now_ms < lifted.at_ms.saturating_add(OFF_RESAY_SPAN_MS) =>
                {
                    Some(Said {
                        id: id.clone(),
                        by: by.clone(),
                        word: Word::Off,
                        t_ms: now_ms,
                    })
                }
                _ => None,
            }
        });
        halts.chain(asks).chain(others).collect()
    }

    /// The halts that stand, by id.
    #[must_use]
    pub const fn standing(&self) -> &BTreeMap<String, Standing> {
        &self.standing
    }

    /// The ids lifted here that are kept.
    #[must_use]
    pub const fn lifted(&self) -> &BTreeMap<String, Lifted> {
        &self.lifted
    }

    /// Whether `id` was lifted here, for this Mac or every one, and is still kept.
    #[must_use]
    pub fn is_lifted(&self, id: &str) -> bool {
        self.lifted.contains_key(id)
    }

    /// Whether a halt-off for every Mac lifted `id` here: one naming it was said, or heard.
    #[must_use]
    pub fn lifted_everywhere(&self, id: &str) -> bool {
        self.lifted
            .get(id)
            .is_some_and(|lifted| matches!(lifted.how, Lift::Fleet { .. }))
    }

    /// Whether `id` stands or was lifted here: a request naming it was carried already.
    #[must_use]
    pub fn knows(&self, id: &str) -> bool {
        self.standing.contains_key(id) || self.lifted.contains_key(id)
    }

    /// The reason the sessions are held under while any halt stands: the newest's, its id
    /// breaking a tie.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.standing
            .iter()
            .max_by(|(a_id, a), (b_id, b)| a.at_ms.cmp(&b.at_ms).then(a_id.cmp(b_id)))
            .map(|(_, halt)| halt.reason.as_str())
    }

    /// Carry a halt that stood before the upgrade (§11 C4): it stands under `id` from `origin`
    /// (this Mac), asked by `by`, unless that id stands or was lifted already. Answers whether it
    /// was carried now.
    pub fn carry(&mut self, id: &str, origin: &str, by: &str, reason: &str, now_ms: u64) -> bool {
        if self.knows(id) || self.standing.len() >= STANDING_MAX {
            return false;
        }
        self.standing.insert(
            id.to_string(),
            Standing {
                origin: origin.to_string(),
                by: by.to_string(),
                reason: reason.to_string(),
                at_ms: now_ms,
                t_ms: now_ms,
            },
        );
        true
    }

    /// Whether the halts this Mac held under the old subject were carried in (§11 C4): the move
    /// runs once, so a lifted carried halt is never carried again once its id is pruned.
    #[must_use]
    pub const fn moved(&self) -> bool {
        self.moved
    }

    /// Mark the move done.
    pub fn set_moved(&mut self) {
        self.moved = true;
    }

    /// The state file's text: the head, a `moved` line once the old rows moved in, an `out` line
    /// while the log has this Mac out, then one `on <id> <halt>` per standing halt and one
    /// `off <id> <at_ms> <how>[ was <halt>]` per lifted id, `<halt>` being
    /// `<origin> <by> <at_ms> <t_ms> <reason>` and `<how>` `here <by>`, `fleet <by> said|heard`
    /// or `gone`.
    #[must_use]
    pub fn render(&self) -> String {
        let halt = |halt: &Standing| {
            format!(
                "{} {} {} {} {}",
                halt.origin, halt.by, halt.at_ms, halt.t_ms, halt.reason
            )
        };
        let mut out = format!("{STATE_HEAD}\n");
        if self.moved {
            out.push_str("moved\n");
        }
        if self.out {
            out.push_str("out\n");
        }
        for (id, standing) in &self.standing {
            out.push_str(&format!("on {id} {}\n", halt(standing)));
        }
        for (id, Lifted { how, at_ms, was }) in &self.lifted {
            let how = match how {
                Lift::Here { by } => format!("here {by}"),
                Lift::Fleet { by, said: true } => format!("fleet {by} said"),
                Lift::Fleet { by, said: false } => format!("fleet {by} heard"),
                Lift::Gone => "gone".to_string(),
            };
            let was = was
                .as_ref()
                .map(|was| format!(" was {}", halt(was)))
                .unwrap_or_default();
            out.push_str(&format!("off {id} {at_ms} {how}{was}\n"));
        }
        out
    }

    /// Read a state file's text. A damaged line is said and skipped, never a reason to forget
    /// the rest; a line of a kind this build does not know is skipped, so a later build can say
    /// more.
    ///
    /// # Errors
    /// A head of another version.
    pub fn parse(text: &str) -> Result<(Self, Vec<String>), String> {
        let mut lines = text.lines();
        if lines.next() != Some(STATE_HEAD) {
            return Err(format!("not a halt state (`{STATE_HEAD}` first)"));
        }
        let mut out = Self::default();
        let mut damaged = Vec::new();
        for line in lines {
            let words: Vec<&str> = line.split(' ').collect();
            match words.as_slice() {
                ["moved"] => out.moved = true,
                ["out"] => out.out = true,
                ["on", id, halt @ ..] if is_id(id) => match parse_standing(halt) {
                    Some(halt) => {
                        out.standing.insert((*id).to_string(), halt);
                    }
                    None => damaged.push(crate::render::safe(line, 80)),
                },
                ["off", id, at, rest @ ..] if is_id(id) => {
                    let (how, was) = match rest.iter().position(|word| *word == "was") {
                        Some(at) => (&rest[..at], Some(parse_standing(&rest[at + 1..]))),
                        None => (rest, None),
                    };
                    let how = match how {
                        ["here", by] if crate::subject::is_principal(by) => Some(Lift::Here {
                            by: (*by).to_string(),
                        }),
                        ["fleet", by, said @ ("said" | "heard")]
                            if crate::subject::is_principal(by) =>
                        {
                            Some(Lift::Fleet {
                                by: (*by).to_string(),
                                said: *said == "said",
                            })
                        }
                        ["gone"] => Some(Lift::Gone),
                        _ => None,
                    };
                    match (how, crate::parse_uint(at), was) {
                        (Some(how), Some(at_ms), None) => {
                            out.lifted.insert(
                                (*id).to_string(),
                                Lifted {
                                    how,
                                    at_ms,
                                    was: None,
                                },
                            );
                        }
                        (Some(how), Some(at_ms), Some(Some(was))) => {
                            out.lifted.insert(
                                (*id).to_string(),
                                Lifted {
                                    how,
                                    at_ms,
                                    was: Some(was),
                                },
                            );
                        }
                        _ => damaged.push(crate::render::safe(line, 80)),
                    }
                }
                ["on" | "off", ..] => damaged.push(crate::render::safe(line, 80)),
                _ => {}
            }
        }
        // A line that says both is lifted: a lift is never undone.
        let lifted: Vec<String> = out.lifted.keys().cloned().collect();
        for id in lifted {
            out.standing.remove(&id);
        }
        Ok((out, damaged))
    }
}

/// A halt as the state file keeps it: `<origin> <by> <at_ms> <t_ms> <reason>`.
fn parse_standing(words: &[&str]) -> Option<Standing> {
    let [origin, by, at, t, reason] = words else {
        return None;
    };
    let ok = crate::subject::is_principal(origin)
        && crate::subject::is_principal(by)
        && !reason.is_empty()
        && reason.len() <= REASON_MAX;
    ok.then_some(())?;
    Some(Standing {
        origin: (*origin).to_string(),
        by: (*by).to_string(),
        reason: (*reason).to_string(),
        at_ms: crate::parse_uint(at)?,
        t_ms: crate::parse_uint(t)?,
    })
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of the person rule (`any-principal-halts`): on a thread where it is
    /// set, a record asked by any principal, an agent's `a-` included, is read as a person's.
    pub static ANY_PRINCIPAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of the id rule (`off-lifts-every`): on a thread where it is set, a
    /// halt-off lifts every standing halt, whatever id it names, as the old per-person row did.
    pub static OFF_LIFTS_EVERY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of "any current member's person" (`off-from-origin-only`): on a
    /// thread where it is set, a halt-off lifts a halt only when the halt's own origin and person
    /// said it, as the old row (one per person, withdrawn by that person) allowed.
    pub static OFF_FROM_ORIGIN_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of "no halt outlives its origin" (`no-lift-on-gone`): on a thread
    /// where it is set, a removed or rotated origin's halts stand, and what it said applies.
    pub static NO_LIFT_ON_GONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of the move (`no-carry`): on a thread where it is set, a bridge's
    /// first start with the mesh on carries none of the halts it held under the old subject.
    pub static NO_CARRY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of the stale ask (`no-stale-ask`), the path before it: on a thread
    /// where it is set, a Mac never asks for another Mac's halt however old its saying, so one
    /// away past [`OFF_RESAY_SPAN_MS`] holds a halt every other Mac lifted until a person there
    /// lifts it.
    pub static NO_STALE_ASK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, debug_assertions))]
fn control(cell: &'static std::thread::LocalKey<std::cell::Cell<bool>>, name: &str) -> bool {
    cell.with(std::cell::Cell::get) || aterm_types::negative_control::on(name)
}

fn any_principal() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        control(&ANY_PRINCIPAL, "any-principal-halts")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

fn off_lifts_every() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        control(&OFF_LIFTS_EVERY, "off-lifts-every")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

fn off_from_its_origin_only() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        control(&OFF_FROM_ORIGIN_ONLY, "off-from-origin-only")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

fn no_lift_on_gone() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        control(&NO_LIFT_ON_GONE, "no-lift-on-gone")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

fn no_stale_ask() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        control(&NO_STALE_ASK, "no-stale-ask")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// Whether [`NO_CARRY`] is on: set on this thread, or named for this process.
pub(crate) fn no_carry() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        control(&NO_CARRY, "no-carry")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

#[cfg(test)]
#[path = "halt_tests.rs"]
mod tests;
