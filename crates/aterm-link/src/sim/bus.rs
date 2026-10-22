// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUS DOOR'S SIMULATED ARM: an in-memory model of astream's broker,
//! served over astream's OWN wire (`astream_broker::proto`'s frames, decoded
//! and encoded by astream's own codec) to astream's OWN client, so the bridge
//! reaches it through exactly the calls it makes on a real broker.
//!
//! ## What it serves
//!
//! The requests the bridge makes, with astream's semantics:
//!
//! * `Hello`: the log's identity, the tier served and the table's rule count,
//!   the LINEAGE (empty until the log's first move), the request set;
//! * `Publish`: deduped by `(producer, seq)`; a malformed subject, or one of
//!   the broker's own four (`/a/commit`, `/a/will`, `/a/bind`, `/a/retire`),
//!   refused; a subject new to the index refused past its producer's budget
//!   (`ERR_SUBJECT_BOUND`: the subjects it is charged for and its wills'
//!   reservations); and a pipelined publish's acks in order;
//! * `/a/retire` (a publish, any sequence): the subject retire — moot for an
//!   absent target, refused for one charged to another producer
//!   (`charged: `) or one a will can still append to (`will: `), and for a
//!   target that is malformed or the broker's own;
//! * `Will`: refused for a subject a publish could not take, a second
//!   producer on a connection that holds one, a sequence in the ack half,
//!   and a new subject past the budget; otherwise a hidden registration
//!   record, which RESERVES its subject against its producer's budget until a
//!   record of its lands there. It fires when its connection ends, and at
//!   every open each producer's latest will on the log fires again; deduped
//!   like a publish, fenced by any later sequence of its producer below
//!   [`astream_broker::ACK_SEQ_BASE`], and exempt from the budget;
//! * `Last` (a page in subject order that resumes after its last row once it
//!   reached its max), `Fetch` (a page from a scan of at most
//!   `FETCH_SCAN_MAX` records, its `Mark` naming the oldest offset: 0, for a
//!   log nothing compacts), `Commit` (a hidden record; a group resumes at
//!   `upto + 1`, max-folded), `Subscribe` and `SubscribeGroup` (every
//!   matching record from the start, in order, then live; a refused one ends
//!   its connection), `IdleMarks`, `Keepalive`, `Budget`;
//! * a name past `MAX_NAME_LEN` refused before anything reads it.
//!
//! Every `Mark` carries the durable head. No capability is checked: the
//! simulated broker is an unguarded one, as a test's broker is.
//!
//! ## Held to astream's broker
//!
//! A DIFFERENTIAL TEST plays scripts on this model and on a real
//! `astream_broker::Broker` opened as aterm opens its own, and compares the
//! answers request by request (`super::conform_broker`, sim item B01): one
//! script per verb and edge, and the door's journal of seeded worlds
//! ([`Event`], [`State::journal`]) — what real bridges sent. Each pin bump of
//! `vendor/astream` re-runs it. What it leaves out is listed there.
//!
//! ## What a restart may lose: astream's power-loss law, P2-P7
//!
//! Every request this model answers is ACKED when it is answered, and the
//! durable head ([`State::durable`]) is what the tier has flushed in full:
//! everything on Strict; on Ordered what the horizon's flush or an escalation
//! covered; on Relaxed what an escalation covered. An ESCALATED record — the
//! table aterm hands its broker (`crate::durability::escalation`) — is
//! durable at its ack on every tier (P3e). A restart ([`Broker::stop`]) is one
//! of:
//!
//! * [`Stop::Clean`]: nothing lost, nothing moved (P5);
//! * [`Stop::Crash`] — a process crash in the same boot: nothing acked lost,
//!   nothing moved (P6);
//! * [`Stop::PowerLoss`]: on Strict nothing acked is lost and nothing moves;
//!   on Ordered and Relaxed the log keeps a PREFIX (P2) of at least the
//!   durable head (P3, P3e) — any suffix of unescalated acked records may go —
//!   and the generation moves by one with lineage `(g + 1, kept)`, every older
//!   step clamped to it (P4, P7). The cut is the seed's, drawn from exactly
//!   the cuts [`State::allowed_cuts`] lists, and [`law_check`] holds every
//!   restart to the law independently of how the cut was drawn.
//!
//! WHAT IT DOES NOT COVER. P1 (no refusal with a known floor) holds by
//! construction: the model always opens. P2 is checked as a prefix no longer
//! than the log, not at a batch boundary: the model has no batches. P8 (rot)
//! is not modelled: no byte of this log is ever damaged, so neither its
//! refusal nor its cut with a move is drawn. Nor is a move after a process
//! crash: astream moves a weak tier's generation after an unclean stop of a
//! log off the boot volume, whose page cache it cannot vouch for, and here a
//! crash never moves. A will live at a stop fires at the next open (its
//! producer's latest on the log), where astream's connection thread may fire
//! it at the stop instead, racing the shutdown: one of the two outcomes the
//! race allows. The table's principal rule (`h-`) escalates by the world's
//! producer names ([`Broker::name_producer`]); astream applies it only to a
//! producer a bound grant attached (`/a/bind`), which an unguarded broker
//! never has.
//!
//! ## Held acks: the answer behind the record (aterm-0)
//!
//! A real broker's writer appends a batch, wakes its tails and writes the
//! batch's acks, and a subscriber can read a record before its publisher
//! reads the ack — or the other way round. With [`State::holds`] set, an ack
//! is HELD at the seed's choice: the record is on the log and readable, and
//! the answer waits ([`ConnState::held`]), with every answer behind it on that
//! connection. A read that may wait gets it (the wait is the batch's flush);
//! a read that must not wait — the bridge's try ([`crate::fx::Fx::unwaited`])
//! — finds it not yet there; the world's [`Broker::answer_held`] sends it, as
//! a flush landing between two of the bridge's passes. So the bridge's
//! non-blocking loop meets every order of a delivery and its own post's ack,
//! a reply and the commit before it, and the counts say it did
//! ([`Broker::held_counts`]).
//!
//! ## Deterministic by construction
//!
//! A request is processed inside the client's own `write`, on the bridge's
//! thread, and its reply queued for the client's next `read` — or held, at
//! the seed's choice, until a read that waits or the world releases it; a
//! subscription's deliveries are produced only when the scheduler's pump
//! reads it ([`crate::sim::Core::pump`]). A connection a client drops or
//! closes ends at that call, and its will fires then.

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read};
use std::sync::{Arc, Mutex};

use astream_broker::proto::{decode_request, encode_response, Request, Response};
use astream_broker::{LogIdentity, NonceExt, ServedInfo, ServedReason, Tier as WireTier};
use astream_wire::Filter;

use super::{lock, Process, Rng};
use crate::fx::BusCloser;
use crate::transport::Conn;

/// The tier the simulated broker serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tier {
    Strict,
    Ordered,
    Relaxed,
}

/// How the broker stops ([`Broker::stop`]): the module doc's three restarts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Clean,
    Crash,
    PowerLoss,
}

/// What a record is, beside its subject and body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A publish (or a will's firing, or a world producer's write).
    Plain,
    /// A group commit (hidden).
    Commit { group: String, upto: u64 },
    /// A will's registration (hidden): the will it registered, by index.
    WillReg(usize),
    /// A subject retire (hidden).
    Retire,
}

/// One record of the log.
#[derive(Clone, Debug)]
pub(crate) struct Rec {
    pub(crate) subject: String,
    pub(crate) body: Vec<u8>,
    /// `(producer id, producer seq)` for a deduped write.
    pub(crate) key: Option<(u64, u64)>,
    pub(crate) kind: Kind,
    /// Whether the broker's own escalation table named it
    /// ([`Escalation::escalates`]): it was acked only once flushed in full.
    pub(crate) escalated: bool,
    /// The principal of the producer that wrote it, as the world named it
    /// ([`Broker::name_producer`]): what the oracle's principal rule reads.
    pub(crate) principal: Option<String>,
}

impl Rec {
    fn hidden(&self) -> bool {
        !matches!(self.kind, Kind::Plain)
    }
}

/// THE ESCALATION TABLE the model applies: aterm's, READ OFF the table
/// `crate::durability::escalation` hands astream ([`Escalation::aterm`]), rule
/// by rule, so a change to the shipped table is the change the simulated
/// broker escalates — never a copy of it that a change leaves behind. The
/// model reads a producer's principal from the world's own map
/// ([`Broker::name_producer`]), as astream's crash simulation keeps its own
/// oracle of what is escalated.
#[derive(Clone, Debug)]
pub(crate) struct Escalation {
    pub(crate) subjects: Vec<Filter>,
    pub(crate) principal_prefixes: Vec<String>,
    pub(crate) groups: Vec<Filter>,
}

impl Escalation {
    /// aterm's table, the one it opens its broker with
    /// (`crate::durability::escalation`).
    pub(crate) fn aterm() -> Self {
        Self::of(&crate::durability::escalation())
    }

    /// The model of `table`: each rule's filter parsed as astream parses it
    /// at the open. A rule kind this model has no semantics for stops the
    /// world rather than escalate nothing in its place.
    pub(crate) fn of(table: &astream_broker::Escalation) -> Self {
        use astream_broker::Escalate;
        let f = |s: &str| Filter::new(s).expect("an escalation filter astream opens with");
        let mut model = Self::none();
        for rule in table.rules() {
            match rule {
                Escalate::Subject(s) => model.subjects.push(f(s)),
                Escalate::PrincipalPrefix(p) => model.principal_prefixes.push(p.clone()),
                Escalate::Group(g) => model.groups.push(f(g)),
                other => panic!("the simulated broker has no model of escalation rule {other:?}"),
            }
        }
        model
    }

    /// No rule at all: what a broker handed no table escalates.
    pub(crate) fn none() -> Self {
        Self {
            subjects: Vec::new(),
            principal_prefixes: Vec::new(),
            groups: Vec::new(),
        }
    }

    fn rules(&self) -> u16 {
        u16::try_from(self.subjects.len() + self.principal_prefixes.len() + self.groups.len())
            .unwrap_or(u16::MAX)
    }

    /// Whether `rec`, written by `principal`, is escalated.
    pub(crate) fn escalates(
        &self,
        rec_subject: &str,
        kind: &Kind,
        principal: Option<&str>,
    ) -> bool {
        match kind {
            Kind::Commit { group, .. } => self.groups.iter().any(|g| g.matches_str(group)),
            Kind::Plain => {
                self.subjects.iter().any(|s| s.matches_str(rec_subject))
                    || principal.is_some_and(|p| {
                        self.principal_prefixes.iter().any(|pre| p.starts_with(pre))
                    })
            }
            // Hidden records escape the principal rule (astream's own claim).
            Kind::WillReg(_) | Kind::Retire => false,
        }
    }
}

/// One will, registered on a connection.
#[derive(Clone, Debug)]
struct Will {
    producer: u64,
    seq: u64,
    subject: String,
    body: Vec<u8>,
    /// The offset of its registration record: the log holds the will only
    /// while it holds this.
    reg_off: u64,
    /// The connection it was registered on, while that connection lives.
    conn: Option<u64>,
    /// Where it fired, if it has (and the log still holds that).
    fired_at: Option<u64>,
    /// Replaced by a later will on the same connection.
    replaced: bool,
}

/// One connection's state.
#[derive(Debug)]
struct ConnState {
    pid: u64,
    inbound: Vec<u8>,
    out: VecDeque<u8>,
    /// Answers the broker has made and not yet SENT, oldest first: a write
    /// whose batch is still in its flush ([`State::answer`]). Released whole
    /// by a read that may wait (the wait is the flush) or by the world's
    /// [`Broker::answer_held`]; a read that must not wait finds them not yet
    /// there ([`crate::fx::Fx::unwaited`]).
    held: VecDeque<Held>,
    open: bool,
    /// `Some((cursor, filter))` once it is a subscription.
    sub: Option<(u64, Filter)>,
}

/// One answer the broker holds ([`ConnState::held`]).
#[derive(Debug)]
struct Held {
    /// The record a `PublishAck` names: what the reorder count reads
    /// ([`State::seen_before_ack`]).
    ack_of: Option<u64>,
    /// The framed answer.
    frame: Vec<u8>,
}

/// The simulated broker's state.
#[derive(Debug)]
pub(crate) struct State {
    pub(crate) up: bool,
    pub(crate) tier: Tier,
    pub(crate) table: Escalation,
    /// What the ORACLE escalates — aterm's table always, so a broker run
    /// with a weaker table (a negative control) is caught losing a record
    /// the law protects.
    pub(crate) oracle: Escalation,
    pub(crate) id: [u8; 16],
    pub(crate) generation: u64,
    /// `(generation, common)`, oldest first, at most [`astream_broker::Lineage::MAX`].
    pub(crate) lineage: Vec<(u64, u64)>,
    pub(crate) log: Vec<Rec>,
    /// Every record below it is on stable media.
    pub(crate) durable: u64,
    dedup: BTreeMap<(u64, u64), u64>,
    groups: BTreeMap<String, u64>,
    /// subject -> (offset of its newest record, the producer charged for it).
    index: BTreeMap<String, (u64, u64)>,
    /// producer -> its highest sequence below the ack half.
    max_seq: BTreeMap<u64, u64>,
    /// producer -> the subjects its registered wills hold against its subject
    /// budget until a record of its lands on them ([`State::reserve`]):
    /// astream's `will_reserved`.
    reserved: BTreeMap<u64, std::collections::BTreeSet<String>>,
    wills: Vec<Will>,
    conns: BTreeMap<u64, ConnState>,
    next_conn: u64,
    /// Producer id -> principal, for the principal rule.
    principals: BTreeMap<u64, String>,
    /// Refuse the next this-many publishes by a NODE producer, and group
    /// commits, `ERR_REFUSED` (a batch rolled back), appending nothing.
    pub(crate) refuse_next: u32,
    /// HELD ACKS (aterm-0): the seed's stream that decides which of a
    /// connection's acks the broker answers late — its batch still in its
    /// flush while the record is already readable — or `None` for a broker
    /// that answers every write as it serves it ([`State::answer`]).
    pub(crate) holds: Option<Rng>,
    /// How many answers were held, how many reads that must not wait found
    /// one not yet there, and how many records a subscription handed over
    /// while their publish's ack was still held — the reorder the non-blocking
    /// loop must survive: the bridge can take its own post's delivery before
    /// its ack.
    pub(crate) held_total: u64,
    pub(crate) tries_not_yet: u64,
    pub(crate) seen_before_ack: u64,
    /// NEGATIVE CONTROLS of the arm itself ([`law_check`]'s): lose below the
    /// durable head; lose with no move.
    pub(crate) mutant: ArmMutant,
    /// A SIBLING RACING AN ATTACH ([`Race`]), in the worlds that have one.
    pub(crate) race: Option<Race>,
    /// THE DOOR'S JOURNAL ([`Event`]), in a world that keeps one: every
    /// connection, served request, delivery, connection end, world write and
    /// restart, in order.
    pub(crate) journal: Option<Vec<Event>>,
    notes: Vec<String>,
}

/// A SIBLING THAT RACES ONE OF THE NODE'S ATTACHES (
/// `super::world::Opts::race`): a copy of the state dir, under the node's id
/// and so its producer, whose bridge read the node face before this node's
/// attach presented anything and takes the incarnation above it — one this
/// node's state dir took for an attach the broker refused, which presented
/// nothing. The broker refuses the node's [`Race::refuse_will`]-th will
/// request, as a batch whose commit failed; inside a later one, while
/// nothing of the refused incarnation is on the log, the sibling's node row
/// and its session's row land at that incarnation's sequences, flushed, and
/// it never dies. Inside the request, so the attach that sent it read the
/// face before them: the interleaving a sibling acting between two of the
/// scheduler's steps never reaches.
#[derive(Debug, Default)]
pub(crate) struct Race {
    /// Which of the node's will requests is refused, counting from 1.
    pub(crate) refuse_will: u32,
    /// The node's will requests so far.
    pub(crate) wills: u32,
    /// The refused will's incarnation.
    pub(crate) refused: Option<u64>,
    /// The incarnation the sibling attached under, once it has.
    pub(crate) raced: Option<u64>,
    /// The sibling's node face and session face.
    pub(crate) node_face: String,
    pub(crate) session_face: String,
    /// What the sibling's rows carry past the fields every row has, each
    /// token led by its space.
    pub(crate) extra: String,
    /// Instead of a previously refused incarnation, take the attach's current
    /// incarnation after its Last read, before its will is registered. Its
    /// first node publish then collides and its own will fires on disconnect.
    pub(crate) alias: bool,
    /// The actual colliding first node publish and sibling record offset.
    pub(crate) collision: Option<(u64, u64)>,
}

/// One defect of the simulated arm, for the law check's negative controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ArmMutant {
    #[default]
    None,
    /// A power loss may cut below the durable head (breaks P3/P3e).
    CutBelowDurable,
    /// A power loss that loses acked records moves nothing (breaks P4).
    LoseWithoutMove,
    /// A move whose lineage claims the whole old log (breaks P7).
    LineageAtHead,
    /// A resent `(producer, seq)` is appended again under a fresh offset,
    /// never deduped: the differential test's negative control
    /// (`super::conform_broker`).
    FreshDedup,
    /// A will fires whatever its producer published since: the differential
    /// test's other negative control.
    NoWillFence,
    /// Corrupt each published payload byte: the differential must compare bytes,
    /// including distinct malformed UTF-8 sequences.
    AlterPayload,
}

/// One visible record as the oracle reads it: `(offset, subject, body,
/// producer key)`.
pub(crate) type Visible = (u64, String, Vec<u8>, Option<(u64, u64)>);

/// ONE THING THAT HAPPENED AT THE BROKER'S DOOR, as a world's broker that keeps
/// a journal ([`State::journal`]) records it: what the differential test
/// (`super::conform_broker`) replays against astream's own broker, so the
/// requests it holds the model to are the ones a real bridge sent.
#[derive(Clone, Debug)]
pub(crate) enum Event {
    /// A connection opened ([`dial_stream`]).
    Dial(u64),
    /// A request on a connection reached the broker and was served.
    Request(u64, Request),
    /// A subscription's next delivery was handed to its reader.
    Deliver(u64),
    /// A connection ended while the broker ran (its will fires).
    End(u64),
    /// A world producer's write ([`Broker::publish_as`]).
    Publish {
        producer: u64,
        seq: u64,
        subject: String,
        body: Vec<u8>,
    },
    /// The broker stopped ([`Broker::stop`]).
    Stop(Stop),
    /// The broker opened again ([`Broker::start`]).
    Start,
}

/// The simulated broker every connection of a world reaches.
#[derive(Debug)]
pub(crate) struct Broker {
    pub(crate) state: Mutex<State>,
}

/// What [`Broker::stop`] did, for the trace and the law check.
#[derive(Clone, Debug)]
pub(crate) struct Restart {
    pub(crate) stop: Stop,
    pub(crate) tier: Tier,
    pub(crate) before: Vec<Rec>,
    pub(crate) durable_before: u64,
    pub(crate) kept: u64,
    pub(crate) generation_before: u64,
    pub(crate) generation_after: u64,
    pub(crate) lineage_after: Vec<(u64, u64)>,
}

impl Broker {
    pub(crate) fn new(tier: Tier, table: Escalation) -> Self {
        Self {
            state: Mutex::new(State {
                up: true,
                tier,
                table,
                oracle: Escalation::aterm(),
                id: [0x5a; 16],
                generation: 0,
                lineage: Vec::new(),
                log: Vec::new(),
                durable: 0,
                dedup: BTreeMap::new(),
                groups: BTreeMap::new(),
                index: BTreeMap::new(),
                max_seq: BTreeMap::new(),
                reserved: BTreeMap::new(),
                wills: Vec::new(),
                conns: BTreeMap::new(),
                next_conn: 1,
                principals: BTreeMap::new(),
                refuse_next: 0,
                holds: None,
                held_total: 0,
                tries_not_yet: 0,
                seen_before_ack: 0,
                mutant: ArmMutant::None,
                race: None,
                journal: None,
                notes: Vec::new(),
            }),
        }
    }

    /// Keep a journal of the door from now on ([`State::journal`]).
    pub(crate) fn keep_journal(&self) {
        lock(&self.state).journal.get_or_insert_with(Vec::new);
    }

    /// The journal so far, emptied.
    pub(crate) fn take_journal(&self) -> Vec<Event> {
        lock(&self.state)
            .journal
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Tell the model which principal a producer id is (the principal rule).
    pub(crate) fn name_producer(&self, producer: u64, principal: &str) {
        lock(&self.state)
            .principals
            .insert(producer, principal.to_string());
    }

    /// The trace lines the broker noted since the last take.
    pub(crate) fn take_notes(&self) -> Vec<String> {
        std::mem::take(&mut lock(&self.state).notes)
    }

    /// A WORLD PRODUCER's write (a peer node, a person): appended as a
    /// publish from no connection, deduped by its key. `None` while the
    /// broker is down.
    pub(crate) fn publish_as(
        &self,
        producer: u64,
        seq: u64,
        subject: &str,
        body: &[u8],
    ) -> Option<(u64, bool)> {
        let mut s = lock(&self.state);
        if !s.up {
            return None;
        }
        s.note_event(|| Event::Publish {
            producer,
            seq,
            subject: subject.to_string(),
            body: body.to_vec(),
        });
        s.publish(producer, seq, subject, body).ok()
    }

    /// HOLD ACKS from now on, `rng` deciding which ([`State::answer`]).
    pub(crate) fn hold_acks(&self, rng: Rng) {
        lock(&self.state).holds = Some(rng);
    }

    /// THE BROKER ANSWERS WHAT IT HELD: every held answer on every connection
    /// is sent, as a batch's flush landing does; and with `and_stop`, none is
    /// held from now on. How many were sent.
    pub(crate) fn answer_held(&self, and_stop: bool) -> usize {
        let mut s = lock(&self.state);
        if and_stop {
            s.holds = None;
        }
        let mut sent = 0;
        // Only a connection still open: a dead one's answers died with it.
        for c in s.conns.values_mut().filter(|c| c.open) {
            sent += c.held.len();
            for h in c.held.drain(..) {
                c.out.extend(h.frame);
            }
        }
        if sent > 0 {
            s.notes.push(format!("broker answers {sent} held"));
        }
        sent
    }

    /// The held-ack counts ([`State::held_total`]): held, tries that found
    /// one not yet there, records handed over before their ack.
    pub(crate) fn held_counts(&self) -> (u64, u64, u64) {
        let s = lock(&self.state);
        (s.held_total, s.tries_not_yet, s.seen_before_ack)
    }

    /// THE RECORDS WHOSE ACKS THIS BROKER HOLDS for process `pid` on its open
    /// connections ([`ConnState::held`]): `(offset, subject, body)`, what a
    /// writer still waits to be told landed — the oracle's idle law reads it
    /// when the process parks ([`crate::sim::Process::idle`]). A record a
    /// restart cut is gone from the log and named no more.
    pub(crate) fn held_for(&self, pid: u64) -> Vec<(u64, String, Vec<u8>)> {
        let s = lock(&self.state);
        s.conns
            .values()
            .filter(|c| c.open && c.pid == pid)
            .flat_map(|c| c.held.iter().filter_map(|h| h.ack_of))
            .filter_map(|off| {
                let r = s.log.get(usize::try_from(off).ok()?)?;
                Some((off, r.subject.clone(), r.body.clone()))
            })
            .collect()
    }

    /// The Ordered horizon's full flush (and nothing on another tier).
    pub(crate) fn horizon(&self) {
        let mut s = lock(&self.state);
        if s.up && s.tier == Tier::Ordered {
            s.durable = s.log.len() as u64;
            let line = format!("horizon durable={}", s.durable);
            s.notes.push(line);
        }
    }

    /// Whether a subscription's next read has something to answer: a frame
    /// owed, or its end — a connection the broker no longer has (a restart
    /// ended it) reads as its end, as a dead broker's socket does.
    pub(crate) fn readable(&self, conn: u64) -> bool {
        let s = lock(&self.state);
        let Some(c) = s.conns.get(&conn) else {
            return true;
        };
        if !c.out.is_empty() || !c.open {
            return true;
        }
        match &c.sub {
            Some((cursor, filter)) => s.next_match(*cursor, filter).is_some(),
            None => false,
        }
    }

    /// The process `pid` died: every connection it held ends.
    pub(crate) fn process_died(&self, pid: u64) {
        let mut s = lock(&self.state);
        let conns: Vec<u64> = s
            .conns
            .iter()
            .filter(|(_, c)| c.pid == pid && c.open)
            .map(|(id, _)| *id)
            .collect();
        for conn in conns {
            s.end(conn);
        }
    }

    /// STOP THE BROKER (`stop`), choosing what the restart keeps with `rng`
    /// among exactly the cuts the law allows ([`State::allowed_cuts`]). It
    /// stays down until [`Broker::start`].
    pub(crate) fn stop(&self, stop: Stop, rng: &mut Rng) -> Restart {
        self.stop_choosing(stop, rng, |cuts, rng| {
            let pick = usize::try_from(rng.below(cuts.len() as u64)).unwrap_or(0);
            cuts.get(pick).copied()
        })
    }

    /// [`Broker::stop`], KEEPING THE WHOLE LOG whatever the law would allow it to
    /// cut: a power loss after which every record survived, as a reboot after the
    /// page cache reached the disk leaves it — what the differential test holds
    /// to astream's reopen in a boot it cannot name.
    pub(crate) fn stop_keeping_all(&self, stop: Stop) -> Restart {
        self.stop_choosing(stop, &mut Rng::new(0), |cuts, _| cuts.last().copied())
    }

    fn stop_choosing(
        &self,
        stop: Stop,
        rng: &mut Rng,
        choose: impl FnOnce(&[u64], &mut Rng) -> Option<u64>,
    ) -> Restart {
        let mut s = lock(&self.state);
        s.note_event(|| Event::Stop(stop));
        let before = s.log.clone();
        let durable_before = s.durable;
        let generation_before = s.generation;
        let tier = s.tier;
        // Every connection ends with the process; no will fires until the
        // next open re-fires every will the log still holds.
        s.up = false;
        // What it held unanswered dies with it: a write it took and never
        // answered is one whose fate the writer must learn by its retry.
        for c in s.conns.values_mut() {
            c.open = false;
            c.held.clear();
        }
        let cuts = s.allowed_cuts(stop);
        let mut kept = choose(&cuts, rng).unwrap_or(s.log.len() as u64);
        if s.mutant == ArmMutant::CutBelowDurable
            && stop == Stop::PowerLoss
            && s.tier != Tier::Strict
        {
            kept = rng.below(s.log.len() as u64 + 1);
        }
        let moves = stop == Stop::PowerLoss && s.tier != Tier::Strict;
        let moves = moves && !(s.mutant == ArmMutant::LoseWithoutMove);
        s.log.truncate(usize::try_from(kept).unwrap_or(usize::MAX));
        // What the kept log says is what it says from now on: its group cursors,
        // its index, its dedup — read off it now, not at the next open.
        s.rebuild();
        if moves {
            s.generation += 1;
            let common = if s.mutant == ArmMutant::LineageAtHead {
                before.len() as u64
            } else {
                kept
            };
            for step in &mut s.lineage {
                step.1 = step.1.min(common);
            }
            let g = s.generation;
            s.lineage.push((g, common));
            let max = astream_broker::Lineage::MAX;
            if s.lineage.len() > max {
                let over = s.lineage.len() - max;
                s.lineage.drain(..over);
            }
        }
        let line = format!(
            "broker stop {stop:?} tier={tier:?} head={} durable={durable_before} kept={kept} \
             gen={}",
            before.len(),
            s.generation
        );
        s.notes.push(line);
        Restart {
            stop,
            tier,
            before,
            durable_before,
            kept,
            generation_before,
            generation_after: s.generation,
            lineage_after: s.lineage.clone(),
        }
    }

    /// OPEN AGAIN: rebuild every index from the log that survived, call it
    /// durable (the open's flush), and re-fire the wills it still holds as
    /// astream's open does: each producer's LATEST registration on the log
    /// (`BrokerLog::pending_wills`), in log order, whatever became of it — one
    /// that fired dedups, one a later incarnation fenced appends nothing.
    pub(crate) fn start(&self) {
        let mut s = lock(&self.state);
        s.note_event(|| Event::Start);
        s.rebuild();
        s.durable = s.log.len() as u64;
        s.up = true;
        let mut latest: Vec<usize> = s.latest_wills().into_values().collect();
        latest.sort_by_key(|&i| s.wills[i].reg_off);
        for i in latest {
            s.fire_will(i);
        }
        let line = format!("broker start head={} gen={}", s.log.len(), s.generation);
        s.notes.push(line);
    }

    /// Where the inbox group `group` resumes: one past its committed offset,
    /// or 0 with no commit on the log.
    pub(crate) fn group_start(&self, group: &str) -> u64 {
        lock(&self.state)
            .groups
            .get(group)
            .map_or(0, |upto| upto + 1)
    }

    /// Whether a will of `producer` at `seq` firing now would be FENCED — a
    /// record of the producer at a higher sequence is on the log — as
    /// [`State::fire`] judges one.
    pub(crate) fn fences(&self, producer: u64, seq: u64) -> bool {
        lock(&self.state)
            .max_seq
            .get(&producer)
            .is_some_and(|&high_water| astream_broker::proto::will_is_fenced(high_water, seq))
    }

    /// The highest incarnation `producer` ever put anything on the log under,
    /// in astream's layout — a record, hidden or not, or a will's
    /// registration — or 0: what a power cut's next boot must take its
    /// incarnations above, for the oracle.
    pub(crate) fn max_incarnation(&self, producer: u64) -> u64 {
        lock(&self.state).max_incarnation(producer)
    }

    /// The finite alias actually registered two wills at one incarnation,
    /// collided on the first node publish, and fired only the loser's will.
    pub(crate) fn alias_collision_completed(&self) -> bool {
        let s = lock(&self.state);
        let Some(race) = s
            .race
            .as_ref()
            .filter(|race| race.alias && race.collision.is_some())
        else {
            return false;
        };
        let Some(seq) = race.raced.and_then(astream_broker::will_seq) else {
            return false;
        };
        let wills: Vec<&Will> = s.wills.iter().filter(|will| will.seq == seq).collect();
        wills.len() == 2
            && wills
                .iter()
                .any(|will| will.conn.is_some() && will.fired_at.is_none())
            && wills.iter().any(|will| will.fired_at.is_some())
    }

    /// Every visible record, for the oracle.
    pub(crate) fn records(&self) -> Vec<Visible> {
        let s = lock(&self.state);
        s.log
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.hidden())
            .map(|(i, r)| (i as u64, r.subject.clone(), r.body.clone(), r.key))
            .collect()
    }
}

impl State {
    /// EXACTLY THE CUTS THE LAW ALLOWS a restart of this kind to keep: the
    /// whole log, or — a power loss on Ordered or Relaxed — any prefix at or
    /// above the durable head (every escalated record is below it: its full
    /// flush raised it).
    pub(crate) fn allowed_cuts(&self, stop: Stop) -> Vec<u64> {
        let head = self.log.len() as u64;
        match (stop, self.tier) {
            (Stop::PowerLoss, Tier::Ordered | Tier::Relaxed) => (self.durable..=head).collect(),
            _ => vec![head],
        }
    }

    /// Rebuild the dedup map, the index, the group cursors and the fence
    /// from the log, as an open does; forget the firing of a will whose
    /// firing record the log no longer holds.
    fn rebuild(&mut self) {
        self.dedup.clear();
        self.index.clear();
        self.groups.clear();
        self.max_seq.clear();
        let head = self.log.len() as u64;
        for (i, rec) in self.log.iter().enumerate() {
            let off = i as u64;
            if let Some(key) = rec.key {
                self.dedup.entry(key).or_insert(off);
                if key.1 < astream_broker::ACK_SEQ_BASE {
                    let m = self.max_seq.entry(key.0).or_insert(0);
                    *m = astream_broker::proto::high_water_monotone(*m, key.1);
                }
            }
            match &rec.kind {
                Kind::Plain => {
                    let charged = self
                        .index
                        .get(&rec.subject)
                        .map_or_else(|| rec.key.map_or(0, |k| k.0), |(_, c)| *c);
                    self.index.insert(rec.subject.clone(), (off, charged));
                }
                Kind::Commit { group, upto } => {
                    let e = self.groups.entry(group.clone()).or_insert(*upto);
                    *e = astream_broker::proto::group_commit_monotone(*e, *upto);
                }
                Kind::Retire => {
                    let subject = String::from_utf8_lossy(&rec.body).into_owned();
                    self.index.remove(&subject);
                }
                Kind::WillReg(_) => {}
            }
        }
        for w in &mut self.wills {
            if w.fired_at.is_some_and(|at| at >= head) {
                w.fired_at = None;
            }
            w.conn = None;
        }
        self.conns.retain(|_, c| c.open);
        // THE RESERVATIONS an open rebuilds (`reserve_pending_will_subjects`):
        // each producer's latest will on the log that nothing of its producer
        // above it fenced or said, on a subject not in the index.
        self.reserved.clear();
        for i in self.latest_wills().into_values() {
            if !self.settled_above(i) {
                let w = &self.wills[i];
                let (producer, subject) = (w.producer, w.subject.clone());
                self.reserve(producer, subject);
            }
        }
    }

    /// Hold `subject` against `producer`'s budget for its will, unless the
    /// index holds it already (`reserve_will_subject`).
    fn reserve(&mut self, producer: u64, subject: String) {
        if !astream_broker::store::is_hidden_subject(&subject) && !self.index.contains_key(&subject)
        {
            self.reserved.entry(producer).or_default().insert(subject);
        }
    }

    /// Whether the log holds will `i`: its registration record is on it (a
    /// power loss may have cut it, and a later record taken its offset).
    fn holds_will(&self, i: usize) -> bool {
        self.wills.get(i).is_some_and(|w| {
            usize::try_from(w.reg_off)
                .ok()
                .and_then(|at| self.log.get(at))
                .is_some_and(|r| r.kind == Kind::WillReg(i))
        })
    }

    /// Each producer's LATEST will on the log, by its registration
    /// (`BrokerLog::latest_wills`): producer -> will.
    fn latest_wills(&self) -> BTreeMap<u64, usize> {
        let mut latest: BTreeMap<u64, usize> = BTreeMap::new();
        for i in (0..self.wills.len()).filter(|&i| self.holds_will(i)) {
            let w = &self.wills[i];
            let newer = latest
                .get(&w.producer)
                .is_none_or(|&j| self.wills[j].reg_off < w.reg_off);
            if newer {
                latest.insert(w.producer, i);
            }
        }
        latest
    }

    /// Whether a record of will `i`'s producer ABOVE its registration says
    /// its key or outranks it (astream's `settled_above`).
    fn settled_above(&self, i: usize) -> bool {
        let w = &self.wills[i];
        self.log
            .iter()
            .skip(
                usize::try_from(w.reg_off)
                    .unwrap_or(usize::MAX)
                    .saturating_add(1),
            )
            .filter_map(|r| r.key.filter(|_| matches!(r.kind, Kind::Plain)))
            .any(|(p, seq)| {
                p == w.producer
                    && (seq == w.seq
                        || (seq < astream_broker::ACK_SEQ_BASE
                            && astream_broker::proto::will_is_fenced(seq, w.seq)))
            })
    }

    /// Whether will `w` could still append: no record of its producer
    /// outranks it, and its own key is not on the log
    /// (`will_can_still_append`).
    fn can_still_append(&self, w: &Will) -> bool {
        !self
            .max_seq
            .get(&w.producer)
            .is_some_and(|&high_water| astream_broker::proto::will_is_fenced(high_water, w.seq))
            && !self.dedup.contains_key(&(w.producer, w.seq))
    }

    /// Whether a will that can still append targets `subject`: one a live
    /// connection holds, or a producer's latest on the log
    /// (`will_targets_subject`) — what refuses a retire of it.
    fn will_targets(&self, subject: &str) -> bool {
        let latest = self.latest_wills();
        self.wills.iter().enumerate().any(|(i, w)| {
            w.subject == subject
                && ((w.conn.is_some() && !w.replaced) || latest.get(&w.producer) == Some(&i))
                && self.can_still_append(w)
        })
    }

    /// How many index entries `producer` is charged for.
    fn charged_to(&self, producer: u64) -> usize {
        self.index.values().filter(|(_, c)| *c == producer).count()
    }

    /// Whether `producer`'s record on `subject` would open a subject past its
    /// budget: the index entries charged to it and its wills' reservations
    /// but this subject's (`subject_bound_exceeded`).
    fn past_the_budget(&self, producer: u64, subject: &str) -> bool {
        if self.index.contains_key(subject) {
            return false;
        }
        let held = self
            .reserved
            .get(&producer)
            .map_or(0, |held| held.len() - usize::from(held.contains(subject)));
        self.charged_to(producer) + held >= astream_broker::store::MAX_SUBJECTS_PER_PRODUCER
    }

    fn append(&mut self, rec: Rec) -> u64 {
        let off = self.log.len() as u64;
        if let Some(key) = rec.key {
            self.dedup.insert(key, off);
            if key.1 < astream_broker::ACK_SEQ_BASE {
                let m = self.max_seq.entry(key.0).or_insert(0);
                *m = astream_broker::proto::high_water_monotone(*m, key.1);
            }
        }
        if let Kind::Plain = rec.kind {
            let charged = self
                .index
                .get(&rec.subject)
                .map_or_else(|| rec.key.map_or(0, |k| k.0), |(_, c)| *c);
            self.index.insert(rec.subject.clone(), (off, charged));
            // The subject landed: its producer's reservation of it is spent.
            if let Some((producer, _)) = rec.key {
                if let Some(held) = self.reserved.get_mut(&producer) {
                    if held.remove(&rec.subject) && held.is_empty() {
                        self.reserved.remove(&producer);
                    }
                }
            }
        }
        let escalated = rec.escalated;
        self.log.push(rec);
        let head = self.log.len() as u64;
        match self.tier {
            Tier::Strict => self.durable = head,
            // AN ESCALATED RECORD IS ACKED ONLY ONCE A FULL FLUSH COVERED IT,
            // and a full flush covers everything before it too.
            Tier::Ordered | Tier::Relaxed if escalated => self.durable = head,
            _ => {}
        }
        off
    }

    fn principal(&self, producer: u64) -> Option<&str> {
        self.principals.get(&producer).map(String::as_str)
    }

    /// [`Broker::max_incarnation`], under the lock already held.
    fn max_incarnation(&self, producer: u64) -> u64 {
        let records = self
            .log
            .iter()
            .filter_map(|r| r.key)
            .filter(|(p, _)| *p == producer)
            .map(|(_, seq)| seq);
        let wills = self
            .wills
            .iter()
            .filter(|w| w.producer == producer)
            .map(|w| w.seq);
        records
            .chain(wills)
            .filter_map(astream_broker::incarnation_of)
            .max()
            .unwrap_or(0)
    }

    /// THE RACE ([`Race`]) at one of a NODE producer's will requests: the
    /// one it refuses, and the later one inside which the sibling attaches at
    /// the refused incarnation — while nothing of that incarnation is on the
    /// log, so the sibling takes it as a bridge takes `max(local, Last) + 1`.
    /// The refusal's answer, or `None` for a will to register as usual.
    fn race_at_will(&mut self, producer: u64, seq: u64) -> Option<Vec<Response>> {
        if !self
            .principal(producer)
            .is_some_and(|p| p.starts_with("n-"))
        {
            return None;
        }
        let mut race = self.race.take()?;
        race.wills += 1;
        let at = astream_broker::incarnation_of(seq);
        let mut refusal = None;
        if race.alias && race.raced.is_none() && race.wills == race.refuse_will {
            if let Some(n) = at {
                if let Some(base) = astream_broker::incarnation_base(n) {
                    let refuse_next = std::mem::take(&mut self.refuse_next);
                    // A distinct live connection takes the SAME will through
                    // the normal broker handler. The race is taken out of
                    // self during this call, so it cannot recurse into itself.
                    let sibling_conn = self.next_conn;
                    self.next_conn += 1;
                    self.conns.insert(
                        sibling_conn,
                        ConnState {
                            pid: u64::MAX,
                            inbound: Vec::new(),
                            out: VecDeque::new(),
                            held: VecDeque::new(),
                            open: true,
                            sub: None,
                        },
                    );
                    let _ = self.handle(
                        sibling_conn,
                        Request::Will {
                            producer_id: producer,
                            producer_seq: astream_broker::will_seq(n)
                                .expect("the current incarnation"),
                            subject: race.node_face.clone(),
                            body: format!(
                                "v=1 t=1 state=away inc={n} fabric=disconnected{}",
                                race.extra
                            )
                            .into_bytes(),
                        },
                    );
                    for (k, (subject, body)) in (1u64..).zip([
                        (
                            race.node_face.clone(),
                            format!("v=1 t=1 state=live inc={n} fabric=connected{}", race.extra),
                        ),
                        (
                            race.session_face.clone(),
                            format!(
                                "v=1 t=1 inc={n} epoch=- gen=- state=live hold=0 attention=-{}",
                                race.extra
                            ),
                        ),
                    ]) {
                        self.publish(producer, base + k, &subject, body.as_bytes())
                            .expect("the live sibling's rows before this attach's node publish");
                    }
                    self.refuse_next = refuse_next;
                    self.durable = self.log.len() as u64;
                    race.raced = Some(n);
                    self.notes
                        .push(format!("PROBE sibling aliases current inc={n}"));
                }
            }
        } else if !race.alias && race.wills == race.refuse_will {
            race.refused = at;
            self.notes.push(format!("PROBE will seq={seq} refused"));
            refusal = Some(vec![Response::Error {
                code: astream_broker::proto::ERR_REFUSED,
                msg: "commit failed: the will was rolled back (simulated)".to_string(),
            }]);
        } else if let (false, Some(n), None, Some(at)) = (race.alias, race.refused, race.raced, at)
        {
            let base = astream_broker::incarnation_base(n);
            if let Some(base) = base.filter(|_| at > n && self.max_incarnation(producer) + 1 == n) {
                // ANOTHER BRIDGE'S BATCH: the node's rolled-back batches are
                // this bridge's, not the sibling's.
                let refuse_next = std::mem::take(&mut self.refuse_next);
                let rows = [
                    (
                        race.node_face.clone(),
                        format!("v=1 t=1 state=live inc={n} fabric=connected{}", race.extra),
                    ),
                    (
                        race.session_face.clone(),
                        format!(
                            "v=1 t=1 inc={n} epoch=- gen=- state=live hold=0 attention=-{}",
                            race.extra
                        ),
                    ),
                ];
                for (k, (subject, body)) in (1u64..).zip(&rows) {
                    let _ = self.publish(producer, base + k, subject, body.as_bytes());
                }
                self.refuse_next = refuse_next;
                self.durable = self.log.len() as u64;
                race.raced = Some(n);
                self.notes.push(format!(
                    "PROBE sibling attaches inc={n} inside the attach at {at}"
                ));
            }
        }
        self.race = Some(race);
        refusal
    }

    /// QUEUE `responses` ON `conn`'S STREAM, in order: sent at once, or HELD
    /// (aterm-0) — an ack the broker answers late, its batch still in its
    /// flush, while the record it names is already readable, as a real
    /// broker's writer wakes its tails and writes its acks in no fixed order
    /// — when this broker holds acks ([`State::holds`]) and the seed says so;
    /// and always behind an answer already held, because a connection is
    /// answered in the order it wrote. Only an ack starts a hold: every other
    /// answer is a request's own reply, read by a read that waits.
    fn answer(&mut self, conn: u64, responses: &[Response]) {
        let State {
            conns,
            holds,
            held_total,
            ..
        } = self;
        let Some(c) = conns.get_mut(&conn) else {
            return;
        };
        for r in responses {
            let ack_of = match r {
                Response::PublishAck { offset, .. } => Some(*offset),
                _ => None,
            };
            let hold = !c.held.is_empty()
                || (ack_of.is_some() && holds.as_mut().is_some_and(|rng| rng.chance(1, 3)));
            if hold {
                let mut frame = VecDeque::new();
                put(&mut frame, r);
                c.held.push_back(Held {
                    ack_of,
                    frame: Vec::from(frame),
                });
                *held_total += 1;
            } else {
                put(&mut c.out, r);
            }
        }
    }

    /// One publish, as astream's `serve_publish` and the writer's
    /// `stage_publish` take it: `/a/retire` is a retire ([`State::retire`]);
    /// a malformed subject or one of the broker's own is refused; a resent
    /// key dedups; a new subject past the producer's budget is refused; the
    /// append.
    fn publish(
        &mut self,
        producer: u64,
        seq: u64,
        subject: &str,
        body: &[u8],
    ) -> Result<(u64, bool), Box<Response>> {
        self.publish_bounded(producer, seq, subject, body, true)
    }

    /// [`State::publish`], held to the subject budget or not: a will's
    /// firing is exempt (its registration was held to it, and reserved it).
    fn publish_bounded(
        &mut self,
        producer: u64,
        seq: u64,
        subject: &str,
        body: &[u8],
        bounded: bool,
    ) -> Result<(u64, bool), Box<Response>> {
        if subject == astream_broker::RETIRE_SUBJECT {
            return self.retire(producer, body);
        }
        if let Some(refusal) = subject_error(subject) {
            return Err(refusal);
        }
        if let Some(&off) = self
            .dedup
            .get(&(producer, seq))
            .filter(|_| self.mutant != ArmMutant::FreshDedup)
        {
            if let Some(race) = self.race.as_mut().filter(|race| race.alias) {
                if race
                    .raced
                    .and_then(astream_broker::incarnation_base)
                    .is_some_and(|base| seq == base + 1 && subject == race.node_face)
                {
                    race.collision.get_or_insert((seq, off));
                }
            }
            return Ok((off, true));
        }
        if self.refuse_next > 0
            && self
                .principal(producer)
                .is_some_and(|p| p.starts_with("n-"))
        {
            self.refuse_next -= 1;
            self.notes.push(format!(
                "refuse publish seq={seq} {subject} (batch rolled back)"
            ));
            return Err(refused(
                astream_broker::proto::ERR_REFUSED,
                "commit failed: the batch was rolled back (simulated)".to_string(),
            ));
        }
        if bounded && self.past_the_budget(producer, subject) {
            return Err(refused(
                astream_broker::proto::ERR_SUBJECT_BOUND,
                format!("producer {producer} holds its whole subject budget"),
            ));
        }
        let principal = self.principal(producer).map(str::to_string);
        let escalated = self
            .table
            .escalates(subject, &Kind::Plain, principal.as_deref());
        let off = self.append(Rec {
            subject: subject.to_string(),
            body: if self.mutant == ArmMutant::AlterPayload {
                body.iter().map(|&byte| byte ^ 1).collect()
            } else {
                body.to_vec()
            },
            key: Some((producer, seq)),
            kind: Kind::Plain,
            escalated,
            principal,
        });
        Ok((off, false))
    }

    /// A SUBJECT RETIRE (`/a/retire`, its body the target), as astream's
    /// `serve_publish` and `stage_retire` take one: a target that is not a
    /// subject, or is the broker's own (`/a/...`), is refused; an absent one
    /// is moot (the head, deduped); one charged to another producer is
    /// refused `charged: `, one a will can still append to `will: `;
    /// otherwise the index forgets it and a hidden record says so.
    fn retire(&mut self, producer: u64, body: &[u8]) -> Result<(u64, bool), Box<Response>> {
        use astream_broker::proto::{
            ERR_BAD_SUBJECT, ERR_RECORD_REFUSED, ERR_RETIRE_REFUSED, RETIRE_CHARGED_PREFIX,
            RETIRE_WILL_PREFIX,
        };
        let Ok(target) = std::str::from_utf8(body) else {
            return Err(refused(
                ERR_RECORD_REFUSED,
                "retire body must be a valid UTF-8 subject".to_string(),
            ));
        };
        if target.len() > astream_broker::broker::MAX_NAME_LEN {
            return Err(refused(
                ERR_RECORD_REFUSED,
                format!("subject too long: {} bytes", target.len()),
            ));
        }
        if astream_broker::store::is_hidden_subject(target)
            || target.starts_with("/a/")
            || target == "/a"
        {
            return Err(refused(
                ERR_RECORD_REFUSED,
                format!("reserved subject {target}"),
            ));
        }
        if astream_wire::Subject::new(target).is_err() {
            return Err(refused(ERR_BAD_SUBJECT, format!("bad subject {target:?}")));
        }
        let Some(&(_, charged)) = self.index.get(target) else {
            return Ok((self.log.len() as u64, true));
        };
        if charged != producer {
            return Err(refused(
                ERR_RETIRE_REFUSED,
                format!("{RETIRE_CHARGED_PREFIX}{target} is charged to producer {charged}"),
            ));
        }
        if self.will_targets(target) {
            return Err(refused(
                ERR_RETIRE_REFUSED,
                format!("{RETIRE_WILL_PREFIX}{target} is targeted by a will that can still append"),
            ));
        }
        self.index.remove(target);
        let off = self.append(Rec {
            subject: astream_broker::RETIRE_SUBJECT.to_string(),
            body: body.to_vec(),
            key: None,
            kind: Kind::Retire,
            escalated: false,
            principal: self.principal(producer).map(str::to_string),
        });
        Ok((off, false))
    }

    /// Will `i`'s connection ended: it fires, unless a later will on that
    /// connection replaced it or it fired already.
    fn fire(&mut self, i: usize) {
        if self
            .wills
            .get(i)
            .is_some_and(|w| !w.replaced && w.fired_at.is_none())
        {
            self.fire_will(i);
        }
    }

    /// FIRE will `i` (astream's `stage_will_fire`): FENCED — nothing appended —
    /// when a record of its producer outranks it; otherwise an ordinary
    /// publish under its key, so one already said dedups, exempt from the
    /// subject budget.
    fn fire_will(&mut self, i: usize) {
        let Some(w) = self.wills.get(i).cloned() else {
            return;
        };
        let fenced =
            self.max_seq.get(&w.producer).is_some_and(|&high_water| {
                astream_broker::proto::will_is_fenced(high_water, w.seq)
            }) && self.mutant != ArmMutant::NoWillFence;
        let at = if fenced {
            self.notes.push(format!("will seq={} fenced", w.seq));
            self.log.len() as u64
        } else {
            match self.publish_bounded(w.producer, w.seq, &w.subject, &w.body, false) {
                Ok((off, deduped)) => {
                    self.notes.push(format!(
                        "will seq={} fired @{off}{}",
                        w.seq,
                        if deduped { " (deduped)" } else { "" }
                    ));
                    off
                }
                Err(_) => self.log.len() as u64,
            }
        };
        if let Some(w) = self.wills.get_mut(i) {
            w.fired_at = Some(at);
            w.conn = None;
        }
    }

    /// Record `event` in the journal, in a world that keeps one.
    fn note_event(&mut self, event: impl FnOnce() -> Event) {
        if let Some(journal) = self.journal.as_mut() {
            journal.push(event());
        }
    }

    /// Connection `conn` ended: its will fires (while the broker is up).
    fn end(&mut self, conn: u64) {
        let was_open = self.conns.get(&conn).is_some_and(|c| c.open);
        if let Some(c) = self.conns.get_mut(&conn) {
            c.open = false;
        }
        if !self.up {
            return;
        }
        if was_open {
            self.note_event(|| Event::End(conn));
        }
        let mine: Vec<usize> = self
            .wills
            .iter()
            .enumerate()
            .filter(|(_, w)| w.conn == Some(conn) && !w.replaced)
            .map(|(i, _)| i)
            .collect();
        for i in mine {
            self.fire(i);
        }
    }

    /// The next record at or above `from` a subscription with `filter` is owed.
    fn next_match(&self, from: u64, filter: &Filter) -> Option<u64> {
        let from = usize::try_from(from).ok()?;
        self.log
            .iter()
            .enumerate()
            .skip(from)
            .find(|(_, r)| !r.hidden() && filter.matches_str(&r.subject))
            .map(|(i, _)| i as u64)
    }

    fn mark(&self, next: u64, resume: String) -> Response {
        Response::Mark {
            next,
            head: self.log.len() as u64,
            resume,
            durable: Some(self.durable),
            oldest: None,
        }
    }

    fn hello(&self) -> Response {
        let tier = match self.tier {
            Tier::Strict => WireTier::Strict,
            Tier::Ordered => WireTier::Ordered,
            Tier::Relaxed => WireTier::Relaxed,
        };
        Response::Nonce {
            nonce: vec![0; 32],
            log: Some(LogIdentity {
                id: self.id,
                generation: self.generation,
            }),
            ext: NonceExt {
                durability: Some(ServedInfo {
                    requested: tier,
                    served: tier,
                    reason: ServedReason::None,
                    horizon_ms: (self.tier == Tier::Ordered).then(|| {
                        u32::try_from(crate::durability::ORDERED_HORIZON.as_millis())
                            .unwrap_or(u32::MAX - 1)
                    }),
                    rules: self.table.rules(),
                }),
                // Every open log has a lineage, empty until its first move.
                lineage: astream_broker::Lineage::from_steps(&self.lineage),
                requests: Some(astream_broker::REQUEST_RETIRE),
                health: None,
                retention: None,
            },
        }
    }

    /// Answer one request on `conn`: the frames it writes back.
    fn handle(&mut self, conn: u64, req: Request) -> Vec<Response> {
        let head = self.log.len() as u64;
        // A NAME PAST astream's BOUND is refused before anything parses it
        // (`oversized_name`).
        if let Some(msg) = oversized_name(&req) {
            return vec![Response::Error {
                code: astream_broker::proto::ERR_RECORD_REFUSED,
                msg,
            }];
        }
        match req {
            Request::Hello => vec![self.hello()],
            Request::Attach { .. } | Request::IdleMarks | Request::Keepalive => {
                vec![self.mark(head, String::new())]
            }
            Request::Publish {
                producer_id,
                producer_seq,
                subject,
                body,
            } => match self.publish(producer_id, producer_seq, &subject, &body) {
                Ok((offset, deduped)) => {
                    self.notes.push(format!(
                        "publish c{conn} seq={producer_seq} {subject} @{offset}{}",
                        if deduped { " dup" } else { "" }
                    ));
                    vec![Response::PublishAck { offset, deduped }]
                }
                Err(e) => vec![*e],
            },
            Request::Will {
                producer_id,
                producer_seq,
                subject,
                body,
            } => self.register(conn, producer_id, producer_seq, subject, body),
            Request::Commit { group, upto } => {
                // A BATCH ROLLED BACK refuses the group commit in it too: the
                // bridge's commit rides the pipeline beside its publishes
                // (aterm-0), and only a bridge's connection commits here.
                if self.refuse_next > 0 {
                    self.refuse_next -= 1;
                    self.notes.push(format!(
                        "refuse commit {group} upto={upto} (batch rolled back)"
                    ));
                    return vec![Response::Error {
                        code: astream_broker::proto::ERR_REFUSED,
                        msg: "commit failed: the batch was rolled back (simulated)".to_string(),
                    }];
                }
                let kind = Kind::Commit {
                    group: group.clone(),
                    upto,
                };
                let escalated = self.table.escalates("", &kind, None);
                let off = self.append(Rec {
                    subject: astream_broker::store::COMMIT_SUBJECT.to_string(),
                    body: Vec::new(),
                    key: None,
                    kind,
                    escalated,
                    principal: None,
                });
                let e = self.groups.entry(group.clone()).or_insert(upto);
                *e = astream_broker::proto::group_commit_monotone(*e, upto);
                self.notes
                    .push(format!("commit {group} upto={upto} @{off}"));
                vec![Response::PublishAck {
                    offset: off,
                    deduped: false,
                }]
            }
            Request::Last { filter, after, max } => {
                let Ok(filt) = Filter::new(filter.as_str()) else {
                    return vec![Response::Error {
                        code: astream_broker::proto::ERR_BAD_FILTER,
                        msg: format!("bad filter {filter:?}"),
                    }];
                };
                // astream's page (`last_matching`): at most `max` rows after
                // `after`, in subject order; a page that reached its max
                // resumes after its last row, whether or not more follow; a
                // head query (`max` 0) is a page of none with nothing to
                // resume. Its per-round scan bound is invisible below
                // `LAST_RESUME_ROUNDS` x `LAST_SCAN_MAX` index entries
                // (module doc).
                let max = max.min(astream_broker::broker::LAST_PAGE_MAX) as usize;
                let mut out = Vec::new();
                let mut resume = String::new();
                if max > 0 {
                    let rows = self
                        .index
                        .range::<str, _>((
                            std::ops::Bound::Excluded(after.as_str()),
                            std::ops::Bound::Unbounded,
                        ))
                        .filter(|(subject, _)| filt.matches_str(subject));
                    for (subject, (off, _)) in rows {
                        out.push(Response::Delivery {
                            offset: *off,
                            subject: subject.clone(),
                            body: self.log[usize::try_from(*off).unwrap_or(0)].body.clone(),
                        });
                        if out.len() >= max {
                            resume.clone_from(subject);
                            break;
                        }
                    }
                }
                self.notes.push(format!(
                    "last c{conn} {filter} after={after:?}: {} rows",
                    out.len()
                ));
                out.push(self.mark(head, resume));
                out
            }
            Request::Fetch {
                from_offset,
                filter,
                max,
            } => {
                let Ok(filt) = Filter::new(filter.as_str()) else {
                    return vec![Response::Error {
                        code: astream_broker::proto::ERR_BAD_FILTER,
                        msg: format!("bad filter {filter:?}"),
                    }];
                };
                // astream's page (`fetch_from`): at most `max` records, from a
                // scan of at most `FETCH_SCAN_MAX` records — a sparse filter's
                // page may end with none, its cursor moved on — and a `Mark`
                // that names the oldest offset the filter reaches: 0, on a log
                // nothing ever compacted.
                let mut out = Vec::new();
                let mut next = from_offset;
                if max > 0 {
                    let scan = astream_broker::broker::FETCH_SCAN_MAX as u64;
                    let limit = from_offset.saturating_add(scan).min(head);
                    let mut off = from_offset;
                    while off < limit {
                        next = off + 1;
                        let rec = &self.log[usize::try_from(off).unwrap_or(0)];
                        if !rec.hidden() && filt.matches_str(&rec.subject) {
                            out.push(Response::Delivery {
                                offset: off,
                                subject: rec.subject.clone(),
                                body: rec.body.clone(),
                            });
                            if out.len() >= max as usize {
                                break;
                            }
                        }
                        off = next;
                    }
                }
                let mut mark = self.mark(next, String::new());
                if let Response::Mark { oldest, .. } = &mut mark {
                    *oldest = Some(0);
                }
                out.push(mark);
                out
            }
            Request::Subscribe {
                from_offset,
                filter,
            } => self.subscribe(conn, from_offset, &filter),
            Request::SubscribeGroup { group, filter } => {
                let from = self.groups.get(&group).map_or(0, |upto| upto + 1);
                self.subscribe(conn, from, &filter)
            }
            Request::Budget { producer_id } => vec![Response::Budget {
                subjects: self.charged_to(producer_id) as u64,
                reserved: self.reserved.get(&producer_id).map_or(0, |held| held.len()) as u64,
                max: astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64,
            }],
            other => vec![Response::Error {
                code: astream_broker::proto::ERR_REFUSED,
                msg: format!("malformed request (not simulated): {other:?}"),
            }],
        }
    }

    /// REGISTER A WILL on `conn`, as astream's `serve_will` and the writer's
    /// `stage_will_register` take one: a subject a publish could not take is
    /// refused; so is a second producer's on a connection that holds one; so
    /// is a sequence in the ack half, which no later record could fence; and
    /// a subject new to the index past its producer's budget. Otherwise a
    /// hidden record holds it, it replaces the connection's earlier will,
    /// and its subject is reserved against the budget until it lands.
    fn register(
        &mut self,
        conn: u64,
        producer_id: u64,
        producer_seq: u64,
        subject: String,
        body: Vec<u8>,
    ) -> Vec<Response> {
        use astream_broker::proto::{ERR_RECORD_REFUSED, ERR_SUBJECT_BOUND};
        if let Some(refusal) = subject_error(&subject) {
            return vec![*refusal];
        }
        let held = self
            .wills
            .iter()
            .find(|w| w.conn == Some(conn) && !w.replaced)
            .map(|w| w.producer);
        if let Some(held) = held.filter(|&held| held != producer_id) {
            return vec![Response::Error {
                code: ERR_RECORD_REFUSED,
                msg: format!(
                    "this connection already holds a will for producer {held} (use another \
                     connection for producer {producer_id})"
                ),
            }];
        }
        if producer_seq >= astream_broker::ACK_SEQ_BASE {
            return vec![Response::Error {
                code: ERR_RECORD_REFUSED,
                msg: "will was not persisted: a will's sequence in the ack half".to_string(),
            }];
        }
        if let Some(refusal) = self.race_at_will(producer_id, producer_seq) {
            return refusal;
        }
        if self.past_the_budget(producer_id, &subject) {
            return vec![Response::Error {
                code: ERR_SUBJECT_BOUND,
                msg: format!(
                    "will was not persisted: producer {producer_id} holds its whole subject budget"
                ),
            }];
        }
        for w in &mut self.wills {
            if w.conn == Some(conn) {
                w.replaced = true;
            }
        }
        let i = self.wills.len();
        let reg_off = self.append(Rec {
            subject: astream_broker::store::WILL_SUBJECT.to_string(),
            body: Vec::new(),
            key: None,
            kind: Kind::WillReg(i),
            escalated: false,
            principal: None,
        });
        self.reserve(producer_id, subject.clone());
        self.wills.push(Will {
            producer: producer_id,
            seq: producer_seq,
            subject,
            body,
            reg_off,
            conn: Some(conn),
            fired_at: None,
            replaced: false,
        });
        self.notes
            .push(format!("will c{conn} seq={producer_seq} @{reg_off}"));
        vec![self.mark(self.log.len() as u64, String::new())]
    }

    fn subscribe(&mut self, conn: u64, from: u64, filter: &str) -> Vec<Response> {
        let Ok(filt) = Filter::new(filter) else {
            return vec![Response::Error {
                code: astream_broker::proto::ERR_BAD_FILTER,
                msg: format!("bad filter {filter:?}"),
            }];
        };
        self.notes
            .push(format!("subscribe c{conn} from={from} {filter}"));
        if let Some(c) = self.conns.get_mut(&conn) {
            c.sub = Some((from, filt));
        }
        Vec::new()
    }
}

/// Why a publish (or a will) may not name `subject`, as astream's
/// `publish_subject_error` says: not a subject, or one of the broker's own.
fn subject_error(subject: &str) -> Option<Box<Response>> {
    if astream_wire::Subject::new(subject).is_err() {
        Some(refused(
            astream_broker::proto::ERR_BAD_SUBJECT,
            format!("bad subject {subject:?}"),
        ))
    } else if astream_broker::store::is_hidden_subject(subject) {
        Some(refused(
            astream_broker::proto::ERR_RECORD_REFUSED,
            format!("reserved subject {subject}"),
        ))
    } else {
        None
    }
}

/// A name in `req` past astream's bound (`MAX_NAME_LEN`), as its
/// `oversized_name` refuses one before anything parses it.
fn oversized_name(req: &Request) -> Option<String> {
    let names: Vec<(&str, &str)> = match req {
        Request::Publish { subject, .. } | Request::Will { subject, .. } => {
            vec![("subject", subject)]
        }
        Request::Subscribe { filter, .. } | Request::Fetch { filter, .. } => {
            vec![("filter", filter)]
        }
        Request::Commit { group, .. } => vec![("group", group)],
        Request::SubscribeGroup { group, filter } => vec![("group", group), ("filter", filter)],
        Request::Attach { grant, .. } => vec![("grant", grant)],
        Request::Last { filter, after, .. } => vec![("filter", filter), ("cursor", after)],
        _ => Vec::new(),
    };
    names
        .into_iter()
        .find(|(_, name)| name.len() > astream_broker::broker::MAX_NAME_LEN)
        .map(|(what, name)| format!("{what} too long: {} bytes", name.len()))
}

/// A refusal, as the broker answers one.
fn refused(code: u8, msg: String) -> Box<Response> {
    Box::new(Response::Error { code, msg })
}

/// Frame `resp` onto `out`.
fn put(out: &mut VecDeque<u8>, resp: &Response) {
    let mut bytes = Vec::new();
    astream_broker::proto::write_frame(&mut bytes, &encode_response(resp))
        .expect("a frame to memory");
    out.extend(bytes);
}

/// DIAL the world's broker for process `p`: a connection over a stream into
/// the model, and its closer. Refused while the broker is down, as a socket
/// nobody listens on is.
pub(crate) fn dial(p: &Arc<Process>) -> io::Result<(Conn, BusCloser)> {
    let stream = dial_stream(p)?;
    let conn = stream.conn;
    Ok((
        Conn::from_stream(Box::new(stream)),
        BusCloser::Sim(SimCloser {
            conn,
            process: Arc::clone(p),
        }),
    ))
}

/// [`dial`]'s connection as the raw byte stream astream's client would wrap:
/// what the differential test (`super::conform_broker`) writes frames to.
pub(crate) fn dial_stream(p: &Arc<Process>) -> io::Result<SimStream> {
    let conn = {
        let mut s = lock(&p.core.broker.state);
        if !s.up {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "simulated: the broker is down",
            ));
        }
        let id = s.next_conn;
        s.next_conn += 1;
        s.conns.insert(
            id,
            ConnState {
                pid: p.pid,
                inbound: Vec::new(),
                out: VecDeque::new(),
                held: VecDeque::new(),
                open: true,
                sub: None,
            },
        );
        s.note_event(|| Event::Dial(id));
        id
    };
    p.core.note(format!("dial pid={} conn={conn}", p.pid));
    Ok(SimStream {
        conn,
        process: Arc::clone(p),
    })
}

/// One connection's stream, as astream's client reads and writes it.
#[derive(Debug)]
pub(crate) struct SimStream {
    conn: u64,
    process: Arc<Process>,
}

impl Read for SimStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let broker = &self.process.core.broker;
        let mut s = lock(&broker.state);
        let up = s.up;
        let (mut out_empty, open, sub) = match s.conns.get(&self.conn) {
            Some(c) => (c.out.is_empty(), c.open, c.sub.clone()),
            None => (true, false, None),
        };
        if out_empty && open && up {
            if let Some((cursor, filter)) = sub {
                if let Some(off) = s.next_match(cursor, &filter) {
                    // THE REORDER aterm-0 must survive: a record handed to a
                    // reader while its publisher's ack is still held.
                    if s.conns
                        .values()
                        .any(|c| c.held.iter().any(|h| h.ack_of == Some(off)))
                    {
                        s.seen_before_ack += 1;
                    }
                    let rec = s.log[usize::try_from(off).unwrap_or(0)].clone();
                    s.note_event(|| Event::Deliver(self.conn));
                    if let Some(c) = s.conns.get_mut(&self.conn) {
                        c.sub = Some((off + 1, filter));
                        put(
                            &mut c.out,
                            &Response::Delivery {
                                offset: off,
                                subject: rec.subject,
                                body: rec.body,
                            },
                        );
                    }
                    out_empty = false;
                }
            }
        }
        // AN ANSWER THE BROKER HOLDS (aterm-0, [`State::answer`]): a read that
        // may wait waits for its batch's flush, and gets every held answer; a
        // read that must not wait ([`crate::fx::Fx::unwaited`]) finds it not
        // yet there.
        if out_empty && open && up {
            let trying = self.process.is_trying();
            let State {
                conns,
                tries_not_yet,
                ..
            } = &mut *s;
            if let Some(c) = conns.get_mut(&self.conn) {
                if !c.held.is_empty() {
                    if trying {
                        *tries_not_yet += 1;
                    } else {
                        for h in c.held.drain(..) {
                            c.out.extend(h.frame);
                        }
                        out_empty = false;
                    }
                }
            }
        }
        if out_empty {
            if !open {
                return Ok(0);
            }
            // Nothing owed and nothing queued: a live socket would park here.
            // Nothing in the simulation reads a quiet connection.
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "simulated: nothing to read",
            ));
        }
        let Some(c) = s.conns.get_mut(&self.conn) else {
            return Ok(0);
        };
        let n = buf.len().min(c.out.len());
        for (slot, byte) in buf.iter_mut().zip(c.out.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }
}

impl std::io::Write for SimStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.process.alive() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "simulated: the process is dead",
            ));
        }
        let frames = {
            let mut s = lock(&self.process.core.broker.state);
            let up = s.up;
            let Some(c) = s.conns.get_mut(&self.conn) else {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "simulated: no connection",
                ));
            };
            if !c.open || !up {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "simulated: the connection is closed",
                ));
            }
            c.inbound.extend_from_slice(buf);
            // THE STREAM IS FRAMED BY ASTREAM'S OWN CODEC (`Frame::decode`, the
            // one `read_frame` judges a header by): magic, version, flags and
            // the length cap checked as the broker checks them, so a header the
            // broker would refuse ends this connection as it ends the broker's,
            // and a change to the layout is astream's to make here too.
            let mut frames = Vec::new();
            let mut broken = false;
            loop {
                match astream_wire::Frame::decode(&c.inbound) {
                    Ok(None) => break,
                    Ok(Some(d)) => {
                        c.inbound.drain(..d.consumed);
                        frames.push(d.frame.payload);
                    }
                    Err(_) => {
                        c.inbound.clear();
                        broken = true;
                        break;
                    }
                }
            }
            (frames, broken)
        };
        let (frames, broken) = frames;
        for payload in frames {
            // A kill BEFORE the broker sees the request: written, never served.
            self.process.step("bus-before");
            let req = decode_request(&payload);
            {
                let mut s = lock(&self.process.core.broker.state);
                let subscribing = matches!(
                    req,
                    Some(Request::Subscribe { .. } | Request::SubscribeGroup { .. })
                );
                let responses = match req {
                    Some(req) => {
                        s.note_event(|| Event::Request(self.conn, req.clone()));
                        s.handle(self.conn, req)
                    }
                    None => vec![Response::Error {
                        code: astream_broker::proto::ERR_REFUSED,
                        msg: "malformed request".to_string(),
                    }],
                };
                s.answer(self.conn, &responses);
                // A REFUSED SUBSCRIBE ENDS ITS CONNECTION, as astream's does
                // once the refusal is written: the answer is still read, then
                // the end; its will fires.
                if subscribing && !responses.is_empty() {
                    s.end(self.conn);
                }
            }
            // A kill AFTER: served and answered, the answer never read.
            self.process.step("bus-after");
        }
        // A FRAME THE BROKER CANNOT READ ends the connection, as the broker's
        // session loop ends on `read_frame`'s error: its will fires.
        if broken {
            lock(&self.process.core.broker.state).end(self.conn);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for SimStream {
    fn drop(&mut self) {
        let mut s = lock(&self.process.core.broker.state);
        s.end(self.conn);
    }
}

/// The closer of a simulated connection ([`BusCloser::Sim`]).
#[derive(Debug)]
pub(crate) struct SimCloser {
    conn: u64,
    process: Arc<Process>,
}

impl SimCloser {
    pub(crate) fn conn(&self) -> u64 {
        self.conn
    }

    /// End the connection now: its will fires, its reads end.
    pub(crate) fn close(&self) {
        let mut s = lock(&self.process.core.broker.state);
        if let Some(c) = s.conns.get_mut(&self.conn) {
            c.out.clear();
            c.held.clear();
        }
        s.end(self.conn);
    }
}

/// THE LAW CHECK of one restart: what the model kept against what astream's
/// power-loss law allows (the module doc), judged from the records alone —
/// which records were acked is all of them, which were escalated is the
/// ORACLE's table. Each violation is named by the property it breaks.
pub(crate) fn law_check(r: &Restart, oracle: &Escalation) -> Vec<String> {
    let mut bad = Vec::new();
    let head = r.before.len() as u64;
    if r.kept > head {
        bad.push(format!("P2: kept {} of a {head}-record log", r.kept));
    }
    let lost_acked = r.kept < head;
    match r.stop {
        Stop::Clean | Stop::Crash => {
            if lost_acked {
                bad.push(format!(
                    "{}: a {:?} lost acked records {}..{head}",
                    if r.stop == Stop::Clean { "P5" } else { "P6" },
                    r.stop,
                    r.kept
                ));
            }
            if r.generation_after != r.generation_before {
                bad.push(format!("P5/P6: a {:?} moved the generation", r.stop));
            }
        }
        Stop::PowerLoss => {
            if r.tier == Tier::Strict && lost_acked {
                bad.push("P3: a Strict power loss lost acked records".to_string());
            }
            if r.kept < r.durable_before {
                bad.push(format!(
                    "P3: kept {} below the durable head {}",
                    r.kept, r.durable_before
                ));
            }
            for (i, rec) in r
                .before
                .iter()
                .enumerate()
                .skip(usize::try_from(r.kept).unwrap_or(0))
            {
                if oracle.escalates(&rec.subject, &rec.kind, rec.principal.as_deref()) {
                    bad.push(format!("P3e: escalated record @{i} {} lost", rec.subject));
                }
            }
            if lost_acked && r.generation_after == r.generation_before {
                bad.push("P4: acked records lost and the identity did not move".to_string());
            }
            if r.generation_after != r.generation_before {
                match r.lineage_after.last() {
                    Some(&(g, common)) if g == r.generation_after => {
                        if common > r.kept {
                            bad.push(format!(
                                "P7: lineage says generation {g} continues to {common}, past \
                                 the {} records kept",
                                r.kept
                            ));
                        }
                    }
                    _ => bad.push("P7: a move with no lineage step".to_string()),
                }
                let newest = r.lineage_after.last().map_or(0, |s| s.1);
                if r.lineage_after.iter().any(|&(_, common)| common > newest) {
                    bad.push(format!(
                        "P7: an older step continues past the newest's common {newest}: {:?}",
                        r.lineage_after
                    ));
                }
            }
        }
    }
    bad
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
