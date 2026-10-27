// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUS DOOR'S SIMULATED ARM: an in-memory model of astream's broker,
//! served over astream's own wire (`astream_broker::proto`'s frames and codec)
//! to astream's own client, so the bridge reaches it through exactly the calls
//! it makes on a real broker.
//!
//! ## What it serves
//!
//! The requests the bridge makes, with astream's semantics:
//!
//! * `Hello`: the log's identity, the tier served and the table's rule count,
//!   the lineage (empty for a log that never moved), the request set, and on a
//!   retained broker ([`State::retained`]) the rules aterm retains every
//!   broker it opens by ([`crate::broker_rules::default_rules`]); unretained
//!   is what `aterm link broker` serves when setting them fails;
//! * `Publish`: deduped by `(producer, seq)`; a malformed subject or one of
//!   the broker's own four refused; a subject new to the index refused past
//!   its producer's budget (`ERR_SUBJECT_BOUND`: the subjects charged to it
//!   and its wills' reservations);
//! * `/a/retire` (a publish, any sequence): moot for an absent target, refused
//!   for a reserved or malformed one, one charged to another producer
//!   (`charged: `), or one a will can still append to (`will: `);
//! * `Retire`, the conditional one (`proto::Request::Retire`): its subject's
//!   newest record must be the one it names on this log. One it names on
//!   another log or generation, above the subject's newest record, or (for an
//!   absent subject) at or above the head is refused (`log: `); one newer
//!   than it names, or an absent subject, makes it moot (deduped, at that
//!   record or the head); otherwise it is the `/a/retire` above, with the
//!   same refusals;
//! * `Will`: refused for a subject a publish could not take, a second producer
//!   on a connection that holds one, a sequence in the ack half
//!   (`ERR_RECORD_REFUSED`), and a new subject past the budget; otherwise a
//!   hidden registration record that reserves its subject until a record of
//!   its producer lands there. It fires when its connection ends, and at every
//!   open each producer's latest will on the log fires again; fenced by any
//!   later sequence of its producer below [`astream_broker::ACK_SEQ_BASE`]
//!   (`proto::will_is_fenced`), and exempt from the budget;
//! * `Last` (a page in subject order of at most `LAST_PAGE_MAX` rows, which
//!   resumes after its last row once it reached its max), `Fetch` (a scan of
//!   at most `FETCH_SCAN_MAX` records from the held ones up, its `Mark` naming
//!   the oldest offset its filter is complete from; below it, refused
//!   `retained-from=`, or told, for a reader that asked for gap notices, while
//!   it still holds records there), `Commit` (a hidden record; a group resumes at
//!   `upto + 1`, folded by `proto::group_commit_monotone`), `Subscribe` and
//!   `SubscribeGroup` (every matching record it can still read from its start,
//!   the held ones first, then live, told its gaps first when it asked; a
//!   refused one ends its connection), `IdleMarks`, `GapNotices`, `Keepalive`,
//!   `Budget`; a name past `MAX_NAME_LEN` refused.
//!
//! Unguarded: no capability is checked.
//!
//! ## Retention: astream's own planner (sim item B05)
//!
//! A retained world's broker sets aterm's rules at every open
//! ([`State::retain`]: [`crate::broker_rules::default_rules`], or the same rules
//! at bytes a few records reach, `rules_scaled`), as `aterm link broker` does, and
//! cuts its log as astream's does: each tick ([`Broker::tick`]) and each forced
//! cut ([`Broker::cut_to`], `retain_before_carrying_state`'s, standing in for a
//! byte bound the fleet's traffic reaches) is planned by
//! `astream_broker::retention::plan` on the records from the base, the tick's
//! stamps on the world's wall clock, the group commits and the held store, and
//! applied as astream's held store applies a plan ([`State::apply`]): the records
//! a hold rule still wants are carried at their absolute offsets, the ones given
//! up raise their subjects' floors and are named in the ledger, the base rises
//! to the cut, the dedup keys of the records below it are dropped, and the
//! compaction leaves the whole log flushed. A `Fetch` below its filter's oldest
//! (`retention::oldest`) is refused `ERR_RECORD_REFUSED retained-from=<oldest>`
//! (a head query never), unless its reader asked for gap notices and its filter
//! still holds records there (`BrokerLog::fetch_told`): then its page reads them
//! from its `from`, the gap notice of that range ahead of it. A tail reads the
//! held records below the base and is told what it can no longer read by
//! astream's gap notice, counted from the ledger where it names every record
//! (`retention::counted_from`). The last-value
//! index, the group commits, the wills, the producers' high water and the
//! subjects' charges are carried across every cut, as astream's carried state
//! keeps them. An open in a retained world may set no rules
//! ([`Broker::start_unretained`]): it reads by the rules the held store kept,
//! advertises none and cuts nothing until an open sets them again. The ledger is
//! in memory, so an open starts it whole from the base; the stamps are cut back
//! to the head an open finds (`retention::clamp_stamps`).
//!
//! Before a retention lands every tail is caught up as astream's tail thread is
//! when it is not writing, past what it does not match and never past a gap it is
//! owed; a world's tails have yet to read what they match (they are told its gap
//! once a cut takes it), the differential's write it first
//! ([`State::eager_tails`]), as that thread does the moment a record lands. A
//! world's open ticks by the world's clock; the differential's model takes the
//! tick astream's open asks for at once at the last stamp's time
//! ([`Broker::open_tick`]), as the real arm's wall clock does. The oracle reads
//! what retention gave up ([`Retained::gone`]), the keys a cut dropped and what
//! landed while no rules were set.
//!
//! Not modelled: the held store's file, its frames and rewrites, and its floor
//! bound (past 65,536 subjects astream folds floors into a spill; a world names
//! a few dozen); a tick's room and the writer asking for one (the scheduler draws
//! ticks, and every minute of the world's clock runs one); the writer's caps
//! (aterm sets none); an uncarried cut (aterm makes none on a retained log).
//!
//! ## Held to astream's broker
//!
//! `super::conform_broker` (sim item B01) plays scripts and the door's
//! journal of seeded worlds ([`Event`]) on this model and on a real broker
//! opened as aterm opens its own, retained as the world's was, and compares
//! every answer. Each drift it found has an [`ArmMutant`] it must catch; what
//! it leaves out is listed there.
//!
//! ## What a restart may lose: astream's power-loss law, P2-P7
//!
//! An answered request is acked; the durable head ([`State::durable`]) is
//! what the tier has flushed: everything on Strict; on Ordered what the
//! horizon's full flush ([`Broker::horizon`]) or an escalation covered; on
//! Relaxed what an escalation covered (P3e); and the whole log at an open.
//! [`Stop::Clean`] and [`Stop::Crash`] lose and move nothing (P5, P6).
//! [`Stop::PowerLoss`] on a weak tier keeps a prefix of at least the durable
//! head (P2, P3) drawn from [`State::allowed_cuts`], and a loss moves the
//! generation with lineage `(g + 1, kept)` (P4, P7). [`law_check`] holds every
//! restart to the law. Not modelled: P1 (the model always opens), P8 (no byte
//! rots), batch boundaries, and a move after a crash on a log off the boot
//! volume.
//!
//! ## Held acks, rolled-back batches and determinism
//!
//! With [`State::holds`] set an ack may be HELD at the seed's choice: the
//! record is readable and the answer waits, so the bridge meets every order
//! of a delivery and its own post's ack ([`Broker::held_counts`]). With
//! [`State::refuse_next`] set the next of a node's publishes and group commits
//! are refused as a batch astream rolled back. A request is processed inside
//! the client's `write`, on the bridge's thread; a subscription's deliveries
//! are produced only when the scheduler pumps it.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{self, Read};
use std::sync::{Arc, Mutex};

use astream_broker::proto::{decode_request, encode_response, Request, Response};
use astream_broker::retention::{self, Ledger, Plan, Stamps};
use astream_broker::{
    BrokerRecord, LogIdentity, NonceExt, Retention, ServedInfo, ServedReason, Tier as WireTier,
};
use astream_wire::{Filter, Offset};

use super::{lock, Process, Rng};
use crate::fx::BusCloser;
use crate::transport::Conn;

/// The simulated log's id: a `Hello` names it with the generation ([`State::identity`]).
pub(crate) const LOG_ID: [u8; 16] = [0x5a; 16];

/// The tier the simulated broker serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tier {
    Strict,
    Ordered,
    Relaxed,
}

/// How the broker stops ([`Broker::stop`]).
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
    /// Whether the broker's own escalation table named it ([`Escalation::escalates`]).
    pub(crate) escalated: bool,
    /// The principal of the producer that wrote it.
    pub(crate) principal: Option<String>,
}

impl Rec {
    fn hidden(&self) -> bool {
        !matches!(self.kind, Kind::Plain)
    }
}

/// THE ESCALATION TABLE the model applies.
#[derive(Clone, Debug)]
pub(crate) struct Escalation {
    pub(crate) subjects: Vec<Filter>,
    pub(crate) principal_prefixes: Vec<String>,
    pub(crate) groups: Vec<Filter>,
}

impl Escalation {
    /// aterm's table, the one it opens its broker with (`crate::durability::escalation`).
    pub(crate) fn aterm() -> Self {
        Self::of(&crate::durability::escalation())
    }

    /// The model of `table`: each rule's filter parsed as astream parses it at the open.
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
    /// The offset of its registration record.
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
    /// Answers the broker has made and not yet SENT.
    held: VecDeque<Held>,
    open: bool,
    /// `Some((cursor, filter))` once it is a subscription.
    sub: Option<(u64, Filter)>,
    /// Whether it asked for gap notices (`Request::GapNotices`).
    gaps: bool,
    /// What its tail was told of the records retention gave up below it, once it is a
    /// subscription that asked.
    told: Option<Told>,
}

impl ConnState {
    fn new(pid: u64) -> Self {
        Self {
            pid,
            inbound: Vec::new(),
            out: VecDeque::new(),
            held: VecDeque::new(),
            open: true,
            sub: None,
            gaps: false,
            told: None,
        }
    }
}

/// One answer the broker holds ([`ConnState::held`]).
#[derive(Debug)]
struct Held {
    /// The record a `PublishAck` names: what the reorder count reads ([`State::seen_before_ack`]).
    ack_of: Option<u64>,
    /// The framed answer.
    frame: Vec<u8>,
}

/// What one tail was TOLD of the records retention gave up below it, as astream's
/// `store::Told` keeps it: up to where its last notice told (its start before any; a look
/// that tells nothing leaves it), the ledger's order when it last looked, whether its last
/// notice carried a count, and the retention epoch it last looked at.
#[derive(Clone, Copy, Debug)]
struct Told {
    to: u64,
    seen: u64,
    counted: bool,
    epoch: Option<u64>,
}

impl Told {
    const fn at(start: u64) -> Self {
        Self {
            to: start,
            seen: 0,
            counted: false,
            epoch: None,
        }
    }
}

/// THE LOG'S RETENTION (sim item B05; the module docs): the rules, the held store, the
/// floors, the stamps and the ledger, as astream's held store keeps them in memory.
#[derive(Debug, Default)]
pub(crate) struct Retained {
    /// The rules the held store holds by: set by an open, or read back by one that sets none.
    rules: Option<Retention>,
    /// Whether this open set them: only then does a tick or a cut plan by them and `Hello`
    /// advertise them.
    active: bool,
    /// The log's base: every record below it went with a cut.
    pub(crate) base: u64,
    /// The held store: the records below the base a hold rule still wants, by offset.
    held: BTreeMap<u64, retention::Held>,
    /// Each subject's floor: the offset after the newest record of it a hold rule gave up.
    floors: BTreeMap<String, u64>,
    /// The highest of every floor.
    floor_max: u64,
    /// From where each class of `rules` is whole (`retention::carried_since`).
    since: Vec<u64>,
    /// The tick's stamps (`retention::stamp`).
    stamps: Stamps,
    /// What the hold rules gave up since the open, one by one.
    ledger: Ledger,
    /// Moves with every retention that lands, as astream's `retention_epoch`: a tail looks
    /// for its gap notice again.
    epoch: u64,
    /// THE ORACLE'S: every visible record retention gave up, by offset.
    pub(crate) gone: BTreeSet<u64>,
    /// THE ORACLE'S: the dedup keys a cut dropped.
    pub(crate) dropped: BTreeSet<(u64, u64)>,
    /// THE ORACLE'S: the offsets of the records the log took while no open set its rules
    /// (what an `auto` mirror must never have written).
    pub(crate) unretained: BTreeSet<u64>,
    /// THE ORACLE'S: each record's offset a tail was told it can no longer read, and on
    /// which connection.
    pub(crate) told: Vec<(u64, astream_broker::Gap)>,
    /// Ticks and cuts that landed, cuts that moved the base, and fetches refused below a floor.
    pub(crate) ticks: u64,
    pub(crate) cuts: u64,
    pub(crate) refusals: u64,
    /// The next open sets no rules ([`Broker::start_unretained`]).
    unretained_next: bool,
    /// The next open sets these rules, not its own ([`Broker::start_with`]).
    rules_next: Option<Retention>,
}

/// The simulated broker's state.
#[derive(Debug)]
pub(crate) struct State {
    pub(crate) up: bool,
    pub(crate) tier: Tier,
    pub(crate) table: Escalation,
    /// What the ORACLE escalates.
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
    /// producer -> the subjects its registered wills hold against its subject budget until a record
    /// of its lands on them ([`State::reserve`]).
    reserved: BTreeMap<u64, std::collections::BTreeSet<String>>,
    wills: Vec<Will>,
    conns: BTreeMap<u64, ConnState>,
    next_conn: u64,
    /// Producer id -> principal, for the principal rule.
    principals: BTreeMap<u64, String>,
    /// Refuse the next this-many publishes by a NODE producer, and group commits.
    pub(crate) refuse_next: u32,
    /// HELD ACKS (aterm-0).
    pub(crate) holds: Option<Rng>,
    /// How many answers were held.
    pub(crate) held_total: u64,
    pub(crate) tries_not_yet: u64,
    pub(crate) seen_before_ack: u64,
    /// Writes that carried a group commit and a publish together, the commit first: a
    /// bridge's held inbox commit riding its next publish (critic item B08); and group
    /// commits written alone.
    pub(crate) commits_corked: u64,
    pub(crate) commits_alone: u64,
    /// The rules every open SETS ([`crate::broker_rules::retain`]), as its `Hello` advertises
    /// them; `None` is the broker `aterm link broker` serves when setting them fails, and one
    /// opened by a build before them ([`Broker::retain`]).
    pub(crate) retain: Option<Retention>,
    /// THE LOG'S RETENTION by them (sim item B05).
    pub(crate) ret: Retained,
    /// Every tail is written what it matches before a retention lands, as astream's tail
    /// thread writes it the moment it lands ([`State::catch_up_tails`]): the differential's
    /// model, held to a real broker's tails; a world explores the tail that has yet to read.
    pub(crate) eager_tails: bool,
    /// NEGATIVE CONTROLS of the arm itself ([`law_check`]'s).
    pub(crate) mutant: ArmMutant,
    /// A SIBLING RACING AN ATTACH ([`Race`]), in the worlds that have one.
    pub(crate) race: Option<Race>,
    /// THE DOOR'S JOURNAL ([`Event`]), in a world that keeps one.
    pub(crate) journal: Option<Vec<Event>>,
    /// A SESSION REVIVED between a sweep's read and its retire ([`Revive`]), in the world that arms one.
    pub(crate) revive: Option<Revive>,
    /// Every retire since the world last read them: `(its offset, its subject, the
    /// newest record's body it took out of the index)` ([`Broker::take_retired`]).
    retired: Vec<(u64, String, Vec<u8>)>,
    notes: Vec<String>,
}

/// A SESSION REVIVED BETWEEN A SWEEP'S READ AND ITS RETIRE: when the node's conditional
/// `Retire` of `subject` reaches the broker, `producer` has written `body` there first,
/// as a successor serving the session again would (an update's handoff, the keeper's
/// recovery).
#[derive(Clone, Debug)]
pub(crate) struct Revive {
    pub(crate) subject: String,
    pub(crate) body: Vec<u8>,
    pub(crate) producer: u64,
    pub(crate) seq: u64,
    /// Where the successor's row landed, once it has.
    pub(crate) landed: Option<u64>,
}

/// A SIBLING THAT RACES ONE OF THE NODE'S ATTACHES ( `super::world::Opts::race`).
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
    /// What the sibling's rows carry past the fields every row has, each token led by its space.
    pub(crate) extra: String,
    /// Instead of a previously refused incarnation, take the attach's current one.
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
    /// A resent `(producer, seq)` is appended again under a fresh offset, never deduped.
    FreshDedup,
    /// A will fires whatever its producer published since.
    NoWillFence,
    /// Corrupt each published payload byte.
    AlterPayload,
    /// `Budget` answers `reserved: 0`, whatever its producer's wills hold.
    BudgetReservesNothing,
    /// A will's registration reserves nothing against its producer's subject budget.
    WillReservesNothing,
    /// `/a/retire` skips its `will: ` refusal.
    RetireIgnoresWills,
    /// `/a/retire` skips its `charged: ` refusal.
    RetireIgnoresCharge,
    /// `/a/retire` takes a reserved or malformed target.
    RetireAnyTarget,
    /// The conditional `Retire` retires its subject's newest record whatever offset and log it names.
    RetireIgnoresItsCondition,
    /// `Hello` advertises no retention rules on a retained broker.
    Unretained,
    /// `Hello` names no lineage for a log that never moved.
    HelloNoLineage,
    /// `Publish` refuses every `/a/` subject, not only astream's four.
    RefuseEveryA,
    /// `/a/retire` is a retire only at sequence 0.
    RetireOnlyAtZero,
    /// A will registers on a subject a publish could not take.
    WillTakesAnySubject,
    /// A connection that holds one producer's will takes another producer's.
    WillOfASecondProducer,
    /// A will in the ack half is refused `ERR_REFUSED`, not `ERR_RECORD_REFUSED`.
    WillAckHalfRefused,
    /// A will registers a new subject past its producer's budget.
    WillPastTheBudget,
    /// A publish takes a new subject past its producer's budget.
    PublishPastTheBudget,
    /// `Fetch`'s `Mark` names no oldest offset.
    FetchNamesNoOldest,
    /// `Fetch` scans past `FETCH_SCAN_MAX`.
    FetchUnbounded,
    /// `Fetch` below its filter's oldest offset answers a page from there instead of
    /// `retained-from=`.
    FetchClampsBelowTheFloor,
    /// A `Fetch` of a reader told what retention gave up (one that asked for gap notices) is
    /// refused below its filter's oldest as any other is, whatever retention still holds there.
    FetchNeverTold,
    /// A told `Fetch` below its filter's oldest reads the held records without the gap
    /// notice ahead of them.
    FetchToldNoNotice,
    /// A cut keeps the dedup keys of the records below it: a resend there is deduped.
    KeepsDedupBelowTheCut,
    /// No tail is told a gap notice, whatever retention gave up below it.
    NoGapNotices,
    /// `Last` resumes after a full page only when more rows follow it.
    LastResumesOnlyBeforeMore,
    /// `Last` answers more than `LAST_PAGE_MAX` rows.
    LastPastItsPage,
    /// A name past `MAX_NAME_LEN` is served.
    NoNameBound,
    /// A refused subscribe leaves its connection open.
    RefusedSubscribeLives,
}

/// One visible record as the oracle reads it: `(offset, subject, body, producer key)`.
pub(crate) type Visible = (u64, String, Vec<u8>, Option<(u64, u64)>);

/// ONE THING THAT HAPPENED AT THE BROKER'S DOOR.
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
    /// The broker opened again setting no rules ([`Broker::start_unretained`]).
    StartUnretained,
    /// The broker opened again setting these rules, not its own ([`Broker::start_with`]).
    StartWith(Retention),
    /// A retention tick at the world's wall clock, ms ([`Broker::tick`]).
    Tick(u64),
    /// A cut at this offset ([`Broker::cut_to`]).
    Cut(u64),
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
                id: LOG_ID,
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
                commits_corked: 0,
                commits_alone: 0,
                retain: None,
                ret: Retained::default(),
                eager_tails: false,
                mutant: ArmMutant::None,
                race: None,
                journal: None,
                revive: None,
                retired: Vec::new(),
                notes: Vec::new(),
            }),
        }
        .retaining(Some(crate::broker_rules::default_rules()))
    }

    /// `self`, retained by `rules` ([`Broker::retain`]).
    fn retaining(self, rules: Option<Retention>) -> Self {
        self.retain(rules);
        self
    }

    /// RETAIN BY `rules` at every open from this one on (`None`: never), as `aterm link
    /// broker` does: set now, on a log that holds nothing yet.
    pub(crate) fn retain(&self, rules: Option<Retention>) {
        let mut s = lock(&self.state);
        s.ret = Retained::default();
        s.retain = rules;
        if let Some(rules) = s.retain.clone() {
            s.set_rules(rules);
        }
    }

    /// THE RETENTION TICK at `now_ms`, the world's wall clock (astream's
    /// `Broker::retention_tick_at`): planned by the rules this open set, the cut never above
    /// the head. Nothing on a broker that is down or set no rules. Whether anything landed.
    pub(crate) fn tick(&self, now_ms: u64) -> bool {
        let mut s = lock(&self.state);
        if !s.up || !s.ret.active {
            return false;
        }
        s.note_event(|| Event::Tick(now_ms));
        s.catch_up_tails();
        s.ret.ticks += 1;
        let head = s.log.len() as u64;
        s.plan_and_apply(now_ms, head, None)
    }

    /// ASTREAM'S FIRST TICK OF AN OPEN THAT SETS THE RULES (asked for at once by
    /// `Broker::set_retention`) as a differential's real arm takes it: by the wall clock, which
    /// the run's clock is always ahead of, so `retention::stamp`'s `now.max(last)` stamps what
    /// the open found at the last stamp's time. A world's opens tick by the world's own clock
    /// instead (`super::world`).
    pub(crate) fn open_tick(&self) -> bool {
        let now = lock(&self.state).ret.stamps.last().map_or(0, |&(_, ms)| ms);
        self.tick(now)
    }

    /// A CUT AT `at` on a log this open retains (astream's `retain_before_carrying_state`,
    /// planned by the rules at that cut, on the clock of the tick's last stamp): what a byte
    /// bound the fleet's traffic reaches does. Clamped to the head; nothing at or below the
    /// base, on a broker that is down, or on one that set no rules. Whether it cut.
    pub(crate) fn cut_to(&self, at: u64) -> bool {
        let mut s = lock(&self.state);
        if !s.up || !s.ret.active {
            return false;
        }
        let head = s.log.len() as u64;
        let min = astream_broker::proto::compact_floor_monotone(at.min(head), s.ret.base);
        if min <= s.ret.base {
            return false;
        }
        s.note_event(|| Event::Cut(min));
        s.catch_up_tails();
        let now = s.ret.stamps.last().map_or(0, |&(_, ms)| ms);
        s.plan_and_apply(now, min, Some(min))
    }

    /// OPEN AGAIN SETTING NO RULES, as `aterm link broker` serves when setting them fails:
    /// reads answer by the rules the held store kept, `Hello` advertises none, nothing cuts.
    pub(crate) fn start_unretained(&self) {
        lock(&self.state).ret.unretained_next = true;
        self.start();
    }

    /// OPEN AGAIN SETTING `rules`, not the ones every open of this world sets: a change of rules
    /// no aterm build makes (`world::changed_rules`; rules set outside aterm, as
    /// `asb serve --retain-file` sets them). The open after it sets its own again.
    pub(crate) fn start_with(&self, rules: Retention) {
        lock(&self.state).ret.rules_next = Some(rules);
        self.start();
    }

    /// The offsets of every visible record retention gave up, for the oracle.
    pub(crate) fn gone(&self) -> BTreeSet<u64> {
        lock(&self.state).ret.gone.clone()
    }

    /// The log's base: every record below it went with a cut.
    pub(crate) fn base(&self) -> u64 {
        lock(&self.state).ret.base
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

    /// Every retire since the last call: `(offset, subject, the body of the newest record it took out of the index)`.
    pub(crate) fn take_retired(&self) -> Vec<(u64, String, Vec<u8>)> {
        std::mem::take(&mut lock(&self.state).retired)
    }

    /// `subject`'s newest record in the last-value index, `(offset, body)`; `None` once retired.
    pub(crate) fn last_row(&self, subject: &str) -> Option<(u64, Vec<u8>)> {
        let s = lock(&self.state);
        let (off, _) = *s.index.get(subject)?;
        let rec = s.log.get(usize::try_from(off).ok()?)?;
        Some((off, rec.body.clone()))
    }

    /// The subjects charged to `producer` plus its wills' reservations: what its `Budget` spends.
    pub(crate) fn spent(&self, producer: u64) -> u64 {
        let s = lock(&self.state);
        (s.charged_to(producer) + s.reserved.get(&producer).map_or(0, |held| held.len())) as u64
    }

    /// A WORLD PRODUCER's write (a peer node, a person).
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

    /// THE BROKER ANSWERS WHAT IT HELD.
    pub(crate) fn answer_held(&self, and_stop: bool) -> usize {
        let mut s = lock(&self.state);
        if and_stop {
            s.holds = None;
        }
        let mut sent = 0;
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

    /// The held-ack counts ([`State::held_total`]).
    pub(crate) fn held_counts(&self) -> (u64, u64, u64) {
        let s = lock(&self.state);
        (s.held_total, s.tries_not_yet, s.seen_before_ack)
    }

    /// `(commits that rode a publish in one write, commits written alone)` ([`State::commits_corked`]).
    pub(crate) fn commit_writes(&self) -> (u64, u64) {
        let s = lock(&self.state);
        (s.commits_corked, s.commits_alone)
    }

    /// THE RECORDS WHOSE ACKS THIS BROKER HOLDS for process `pid` on its open connections.
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

    /// Whether a subscription's next read has something to answer: a frame owed, or its end.
    pub(crate) fn readable(&self, conn: u64) -> bool {
        let mut s = lock(&self.state);
        let Some(c) = s.conns.get(&conn) else {
            return true;
        };
        if !c.out.is_empty() || !c.open {
            return true;
        }
        match c.sub.clone() {
            Some((cursor, filter)) => {
                s.next_match(cursor, &filter).is_some() || s.owed_gap(conn, false).is_some()
            }
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

    /// STOP THE BROKER (`stop`).
    pub(crate) fn stop(&self, stop: Stop, rng: &mut Rng) -> Restart {
        self.stop_choosing(stop, rng, |cuts, rng| {
            let pick = usize::try_from(rng.below(cuts.len() as u64)).unwrap_or(0);
            cuts.get(pick).copied()
        })
    }

    /// [`Broker::stop`], KEEPING THE WHOLE LOG whatever the law would allow it to cut.
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
        s.up = false;
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
        // The offsets a loss took name other records from here.
        s.ret.unretained.retain(|&off| off < kept);
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

    /// OPEN AGAIN.
    pub(crate) fn start(&self) {
        let mut s = lock(&self.state);
        let unretained = std::mem::take(&mut s.ret.unretained_next);
        let instead = s.ret.rules_next.take();
        s.note_event(|| match (&instead, unretained) {
            (_, true) => Event::StartUnretained,
            (Some(rules), false) => Event::StartWith(rules.clone()),
            (None, false) => Event::Start,
        });
        s.rebuild();
        s.durable = s.log.len() as u64;
        s.up = true;
        let mut latest: Vec<usize> = s.latest_wills().into_values().collect();
        latest.sort_by_key(|&i| s.wills[i].reg_off);
        for i in latest {
            s.fire_will(i);
        }
        // The rules are set after the open's own writes, as aterm sets them.
        let older = instead.is_some();
        let rules = instead.or_else(|| s.retain.clone()).filter(|_| !unretained);
        if let Some(rules) = rules {
            s.set_rules(rules);
        }
        let line = format!(
            "broker start head={} gen={} base={}{}",
            s.log.len(),
            s.generation,
            s.ret.base,
            match (s.ret.active, older) {
                (false, _) => " unretained",
                (true, true) => " older-rules",
                (true, false) => "",
            }
        );
        s.notes.push(line);
    }

    /// Where the inbox group `group` resumes.
    pub(crate) fn group_start(&self, group: &str) -> u64 {
        lock(&self.state)
            .groups
            .get(group)
            .map_or(0, |upto| upto + 1)
    }

    /// Whether a will of `producer` at `seq` firing now would be FENCED.
    pub(crate) fn fences(&self, producer: u64, seq: u64) -> bool {
        lock(&self.state)
            .max_seq
            .get(&producer)
            .is_some_and(|&high_water| astream_broker::proto::will_is_fenced(high_water, seq))
    }

    /// The highest incarnation `producer` ever put anything on the log under, in astream's layout.
    pub(crate) fn max_incarnation(&self, producer: u64) -> u64 {
        lock(&self.state).max_incarnation(producer)
    }

    /// The finite alias actually registered two wills at one incarnation, collided on the first
    /// node publish, and fired only the loser's will.
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

    /// THE ORACLE OF A READ OF ONE OFFSET (finding F3): the visible record at `off`, if one
    /// is there and its subject starts with `prefix`, with whether the broker still holds it
    /// (at or above the base, or carried below it: a read can still be handed it) and
    /// whether it lies below the oldest of `prefix>` (where a plain read of it is refused).
    pub(crate) fn at(&self, off: u64, prefix: &str) -> Option<At> {
        let s = lock(&self.state);
        let rec = s.log.get(usize::try_from(off).ok()?)?;
        if rec.hidden() || !rec.subject.starts_with(prefix) {
            return None;
        }
        let oldest = Filter::new(format!("{prefix}>")).map_or(0, |f| s.oldest(&f));
        Some(At {
            body: rec.body.clone(),
            holds: off >= s.ret.base || s.ret.held.contains_key(&off),
            below_oldest: off < oldest,
        })
    }

    /// THE ORACLE OF WHAT A READ CAN STILL BE HANDED: whether the visible record at `off` is
    /// one the broker holds (at or above the base, or carried below it), not one retention
    /// gave up.
    pub(crate) fn holds(&self, off: u64) -> bool {
        let s = lock(&self.state);
        usize::try_from(off)
            .ok()
            .and_then(|i| s.log.get(i))
            .is_some_and(|rec| {
                !rec.hidden() && (off >= s.ret.base || s.ret.held.contains_key(&off))
            })
    }

    /// The log's identity now (`<id>.<generation>`, as `link log` names it).
    pub(crate) fn log_identity(&self) -> String {
        let s = lock(&self.state);
        astream_broker::LogIdentity {
            id: s.id,
            generation: s.generation,
        }
        .to_string()
    }

    /// The offsets of the visible records whose subject starts with `prefix`: every one, and
    /// the ones the broker carries below its base; and the head.
    pub(crate) fn offsets_of(&self, prefix: &str) -> (Vec<u64>, Vec<u64>, u64) {
        let s = lock(&self.state);
        let all: Vec<u64> = s
            .log
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.hidden() && r.subject.starts_with(prefix))
            .map(|(i, _)| i as u64)
            .collect();
        let held = s
            .ret
            .held
            .iter()
            .filter(|(_, h)| h.rec.subject.starts_with(prefix))
            .map(|(&off, _)| off)
            .collect();
        (all, held, s.log.len() as u64)
    }
}

/// What the broker holds at one offset ([`Broker::at`]).
#[derive(Clone, Debug)]
pub(crate) struct At {
    pub(crate) body: Vec<u8>,
    pub(crate) holds: bool,
    pub(crate) below_oldest: bool,
}

impl State {
    /// EXACTLY THE CUTS THE LAW ALLOWS a restart of this kind to keep.
    pub(crate) fn allowed_cuts(&self, stop: Stop) -> Vec<u64> {
        let head = self.log.len() as u64;
        match (stop, self.tier) {
            (Stop::PowerLoss, Tier::Ordered | Tier::Relaxed) => (self.durable..=head).collect(),
            _ => vec![head],
        }
    }

    fn rebuild(&mut self) {
        self.dedup.clear();
        self.index.clear();
        self.groups.clear();
        self.max_seq.clear();
        let head = self.log.len() as u64;
        let base = self.ret.base;
        for (i, rec) in self.log.iter().enumerate() {
            let off = i as u64;
            if let Some(key) = rec.key {
                // A cut dropped the dedup keys below the base; the high water is carried.
                if off >= base {
                    self.dedup.entry(key).or_insert(off);
                }
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
        self.reserved.clear();
        for i in self.latest_wills().into_values() {
            if !self.settled_above(i) {
                let w = &self.wills[i];
                let (producer, subject) = (w.producer, w.subject.clone());
                self.reserve(producer, subject);
            }
        }
        // An open reads the held store's rules back and retains by none until it sets them;
        // its ledger, in memory only, is whole from the base; the stamps are cut back to the
        // head it finds.
        self.ret.active = false;
        self.ret.ledger = Ledger::default();
        self.ret.ledger.whole_from(base);
        retention::clamp_stamps(&mut self.ret.stamps, head);
    }

    /// SET `rules` at an open, as astream's held store does: the rules it already holds by
    /// keep it whole from where it was; others start each class where
    /// `retention::carried_since` says.
    fn set_rules(&mut self, rules: Retention) {
        let same = self
            .ret
            .rules
            .as_ref()
            .is_some_and(|held| held.encode() == rules.encode());
        if !same {
            self.ret.since = retention::carried_since(
                self.ret.rules.as_ref(),
                &self.ret.since,
                &rules,
                self.ret.base,
            );
            self.ret.rules = Some(rules);
        }
        self.ret.active = true;
        self.ret.epoch += 1;
    }

    /// THE OLDEST offset a read of `filter` can reach (astream's `BrokerLog::oldest`).
    pub(crate) fn oldest(&self, filter: &Filter) -> u64 {
        retention::oldest(
            self.ret.rules.as_ref(),
            self.ret.base,
            &self.ret.since,
            &self.ret.floors,
            0,
            self.ret.floor_max,
            filter,
        )
    }

    /// The record at `off` as astream's log holds it, for the planner: a hidden record's
    /// subject, body and commit as astream writes them, so its bytes count as they do there.
    fn brecord(&self, off: u64) -> Option<Arc<BrokerRecord>> {
        let rec = self.log.get(usize::try_from(off).ok()?)?;
        let seq = Offset(off);
        let made = match &rec.kind {
            Kind::Plain => {
                let (producer_id, producer_seq) = rec.key.unwrap_or((0, 0));
                BrokerRecord {
                    seq,
                    producer_id,
                    producer_seq,
                    subject: rec.subject.clone(),
                    body: rec.body.clone(),
                    commit: None,
                }
            }
            Kind::Commit { group, upto } => BrokerRecord {
                seq,
                producer_id: 0,
                producer_seq: 0,
                subject: astream_broker::store::COMMIT_SUBJECT.to_string(),
                body: Vec::new(),
                commit: Some(astream_broker::brecord::GroupCommit {
                    group: group.clone(),
                    upto: *upto,
                }),
            },
            Kind::WillReg(i) => {
                let w = self.wills.get(*i)?;
                // astream's `WillRecord::encode_body`: the subject's length, the subject, the body.
                let mut body = u32::try_from(w.subject.len())
                    .unwrap_or(u32::MAX)
                    .to_le_bytes()
                    .to_vec();
                body.extend_from_slice(w.subject.as_bytes());
                body.extend_from_slice(&w.body);
                BrokerRecord {
                    seq,
                    producer_id: w.producer,
                    producer_seq: w.seq,
                    subject: astream_broker::store::WILL_SUBJECT.to_string(),
                    body,
                    commit: None,
                }
            }
            Kind::Retire => {
                BrokerRecord::marked_retire(seq, 0, &String::from_utf8_lossy(&rec.body))
            }
        };
        Some(Arc::new(made))
    }

    /// THE RETENTION at `now` (ms of the world's wall clock), the cut never above `ceiling`,
    /// or at `forced`: astream's planning (`BrokerLog::retention_plan`: the stamps first,
    /// then `retention::plan`), then what its job's finish does ([`State::apply`]). Whether
    /// anything landed.
    fn plan_and_apply(&mut self, now: u64, ceiling: u64, forced: Option<u64>) -> bool {
        let head = self.log.len() as u64;
        let stamps_before = self.ret.stamps.clone();
        retention::stamp(&mut self.ret.stamps, head, now);
        let Some(rules) = self.ret.rules.clone() else {
            return false;
        };
        let base = self.ret.base;
        let records: Vec<Arc<BrokerRecord>> =
            (base..head).filter_map(|off| self.brecord(off)).collect();
        let groups: HashMap<String, u64> = self
            .groups
            .iter()
            .map(|(g, upto)| (g.clone(), *upto))
            .collect();
        let plan = {
            let view = retention::View {
                base,
                ceiling: ceiling.min(head),
                records: &records,
                // No record here has a broker time (`bt=`, astream 93ec213): a bridge never asks.
                record_bts: &[],
                stamps: &self.ret.stamps,
                groups: &groups,
                held: &self.ret.held,
                now,
            };
            retention::plan(&rules, &view, forced)
        };
        if plan.is_empty(base) {
            // A tick that took stamps writes them, and its job's finish moves the epoch.
            if self.ret.stamps != stamps_before {
                self.ret.epoch += 1;
            }
            return false;
        }
        self.apply(plan);
        true
    }

    /// WHAT A PLAN'S JOB DOES ONCE IT LANDS (astream's `HeldStore::finish` and, for a cut,
    /// `BrokerLog::compact_finish_tick`): the held records it gave up go, the ones it carries
    /// are held, the floors rise, the ledger takes its give-ups; a cut drops the dedup keys
    /// still pointing below it, moves the base, and leaves the whole log flushed (the
    /// compaction's copy is). The stamps below the base go.
    fn apply(&mut self, plan: Plan) {
        let base = self.ret.base;
        let head = self.log.len() as u64;
        let carried: BTreeSet<u64> = plan.carry.iter().map(|h| h.rec.seq.0).collect();
        for off in base..plan.cut {
            let Some(rec) = usize::try_from(off).ok().and_then(|i| self.log.get(i)) else {
                continue;
            };
            if let Some(key) = rec.key {
                if self.dedup.get(&key) == Some(&off)
                    && self.mutant != ArmMutant::KeepsDedupBelowTheCut
                {
                    self.dedup.remove(&key);
                    self.ret.dropped.insert(key);
                }
            }
            if !rec.hidden() && !carried.contains(&off) {
                self.ret.gone.insert(off);
            }
        }
        for off in &plan.remove {
            self.ret.held.remove(off);
            self.ret.gone.insert(*off);
        }
        for h in plan.carry {
            self.ret.held.insert(h.rec.seq.0, h);
        }
        for (subject, floor) in plan.floors {
            let f = self.ret.floors.entry(subject).or_insert(0);
            *f = (*f).max(floor);
            self.ret.floor_max = self.ret.floor_max.max(*f);
        }
        self.ret.ledger.take(plan.given);
        if plan.cut > base {
            self.ret.base = plan.cut;
            self.durable = head;
            self.ret.cuts += 1;
        }
        retention::prune_stamps(&mut self.ret.stamps, self.ret.base);
        self.ret.epoch += 1;
        self.notes.push(format!(
            "retention base={} held={} floors={}",
            self.ret.base,
            self.ret.held.len(),
            self.ret.floors.len()
        ));
    }

    /// Every tail caught up before a retention lands, as astream's tail thread is whenever
    /// it is not writing: past what it does not match, its cursor at its next match or the
    /// head. With [`State::eager_tails`] every record it matches is written to it first
    /// too, its gap notice ahead of them, as that thread writes them the moment they land,
    /// so its cursor is the head; without, a world explores the tail that has yet to read
    /// them when the cut lands, and is told their gap instead.
    fn catch_up_tails(&mut self) {
        let head = self.log.len() as u64;
        let tails: Vec<(u64, u64, Filter)> = self
            .conns
            .iter()
            .filter(|(_, c)| c.open)
            .filter_map(|(id, c)| c.sub.clone().map(|(cursor, f)| (*id, cursor, f)))
            .collect();
        for (id, cursor, filter) in tails {
            if !self.eager_tails {
                // It never reads past a gap it is owed a notice of: the notice comes first.
                if self.owed_gap(id, false).is_some() {
                    continue;
                }
                let to = self.next_match(cursor, &filter).unwrap_or(head);
                if let Some((at, _)) = self.conns.get_mut(&id).and_then(|c| c.sub.as_mut()) {
                    *at = to.max(cursor);
                }
                continue;
            }
            let mut frames = Vec::new();
            if let Some(gap) = self.owed_gap(id, true) {
                self.ret.told.push((id, gap));
                frames.push(Response::Gap(gap));
            }
            let mut at = cursor;
            while let Some(off) = self.next_match(at, &filter) {
                let rec = &self.log[usize::try_from(off).unwrap_or(0)];
                frames.push(Response::Delivery {
                    offset: off,
                    subject: rec.subject.clone(),
                    body: rec.body.clone(),
                });
                at = off + 1;
            }
            if let Some(c) = self.conns.get_mut(&id) {
                for frame in &frames {
                    put(&mut c.out, frame);
                }
                c.sub = Some((head.max(cursor), filter));
            }
        }
    }

    /// THE GAP NOTICE a tail reading `filter` is owed at `cursor` (astream's
    /// `BrokerLog::gap_notice`): looked for once per retention epoch, owed only below the
    /// base, from the cursor to the filter's oldest, counted from the ledger where it names
    /// every record; a range a count found nothing in is not told.
    fn gap_notice(
        &self,
        filter: &Filter,
        cursor: u64,
        told: &mut Told,
    ) -> Option<astream_broker::Gap> {
        if told.epoch == Some(self.ret.epoch) {
            return None;
        }
        told.epoch = Some(self.ret.epoch);
        let seen = self.ret.ledger.seq();
        if cursor >= self.ret.base {
            told.seen = seen;
            return None;
        }
        let to = told.to.max(self.oldest(filter));
        let notice = if cursor < to {
            match self.gap_count(filter, cursor, to, told) {
                Some(0) => None,
                Some(n) => Some(Some(n)),
                None if to > told.to || told.counted => Some(None),
                None => None,
            }
        } else {
            None
        };
        told.seen = seen;
        let records = notice?;
        told.to = to;
        told.counted = records.is_some();
        Some(astream_broker::Gap {
            from: cursor,
            to,
            records,
        })
    }

    /// How many records of `filter` in `cursor..to` the hold rules gave up that `told` was not
    /// told of, where the ledger names every one (astream's `HeldStore::gap_count`).
    fn gap_count(&self, filter: &Filter, cursor: u64, to: u64, told: &Told) -> Option<u64> {
        if self.ret.ledger.whole() > cursor
            || !retention::counted_from(
                self.ret.rules.as_ref(),
                &self.ret.since,
                self.ret.base,
                cursor,
                filter,
            )
        {
            return None;
        }
        Some(
            self.ret
                .ledger
                .count(filter, cursor, to, told.to, told.seen),
        )
    }

    fn reserve(&mut self, producer: u64, subject: String) {
        if self.mutant != ArmMutant::WillReservesNothing
            && !astream_broker::store::is_hidden_subject(&subject)
            && !self.index.contains_key(&subject)
        {
            self.reserved.entry(producer).or_default().insert(subject);
        }
    }

    fn holds_will(&self, i: usize) -> bool {
        self.wills.get(i).is_some_and(|w| {
            usize::try_from(w.reg_off)
                .ok()
                .and_then(|at| self.log.get(at))
                .is_some_and(|r| r.kind == Kind::WillReg(i))
        })
    }

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

    fn can_still_append(&self, w: &Will) -> bool {
        !self
            .max_seq
            .get(&w.producer)
            .is_some_and(|&high_water| astream_broker::proto::will_is_fenced(high_water, w.seq))
            && !self.dedup.contains_key(&(w.producer, w.seq))
    }

    fn will_targets(&self, subject: &str) -> bool {
        let latest = self.latest_wills();
        self.wills.iter().enumerate().any(|(i, w)| {
            w.subject == subject
                && ((w.conn.is_some() && !w.replaced) || latest.get(&w.producer) == Some(&i))
                && self.can_still_append(w)
        })
    }

    fn charged_to(&self, producer: u64) -> usize {
        self.index.values().filter(|(_, c)| *c == producer).count()
    }

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
            if let Some((producer, _)) = rec.key {
                if let Some(held) = self.reserved.get_mut(&producer) {
                    if held.remove(&rec.subject) && held.is_empty() {
                        self.reserved.remove(&producer);
                    }
                }
            }
        }
        if !self.ret.active {
            self.ret.unretained.insert(off);
        }
        let escalated = rec.escalated;
        self.log.push(rec);
        let head = self.log.len() as u64;
        match self.tier {
            Tier::Strict => self.durable = head,
            Tier::Ordered | Tier::Relaxed if escalated => self.durable = head,
            _ => {}
        }
        off
    }

    fn principal(&self, producer: u64) -> Option<&str> {
        self.principals.get(&producer).map(String::as_str)
    }

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
                    let sibling_conn = self.next_conn;
                    self.next_conn += 1;
                    self.conns.insert(sibling_conn, ConnState::new(u64::MAX));
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

    fn publish(
        &mut self,
        producer: u64,
        seq: u64,
        subject: &str,
        body: &[u8],
    ) -> Result<(u64, bool), Box<Response>> {
        self.publish_bounded(producer, seq, subject, body, true)
    }

    fn publish_bounded(
        &mut self,
        producer: u64,
        seq: u64,
        subject: &str,
        body: &[u8],
        bounded: bool,
    ) -> Result<(u64, bool), Box<Response>> {
        if subject == astream_broker::RETIRE_SUBJECT
            && (seq == 0 || self.mutant != ArmMutant::RetireOnlyAtZero)
        {
            return self.retire(producer, body);
        }
        if let Some(refusal) = subject_error(subject) {
            return Err(refusal);
        }
        if self.mutant == ArmMutant::RefuseEveryA && subject.starts_with("/a/") {
            return Err(refused(
                astream_broker::proto::ERR_RECORD_REFUSED,
                format!("reserved subject {subject}"),
            ));
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
        if bounded
            && self.mutant != ArmMutant::PublishPastTheBudget
            && self.past_the_budget(producer, subject)
        {
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

    fn retire(&mut self, producer: u64, body: &[u8]) -> Result<(u64, bool), Box<Response>> {
        use astream_broker::proto::{ERR_BAD_SUBJECT, ERR_RECORD_REFUSED};
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
        let checked = self.mutant != ArmMutant::RetireAnyTarget;
        if checked
            && (astream_broker::store::is_hidden_subject(target)
                || target.starts_with("/a/")
                || target == "/a")
        {
            return Err(refused(
                ERR_RECORD_REFUSED,
                format!("reserved subject {target}"),
            ));
        }
        if checked && astream_wire::Subject::new(target).is_err() {
            return Err(refused(ERR_BAD_SUBJECT, format!("bad subject {target:?}")));
        }
        let Some(&(_, charged)) = self.index.get(target) else {
            return Ok((self.log.len() as u64, true));
        };
        self.retire_charged(producer, target, charged)
    }

    /// THE CONDITIONAL RETIRE (`Request::Retire`, astream's
    /// `stage_retire_conditional`): `target`'s newest record must be the one at
    /// `offset` on `log`.
    fn retire_if(
        &mut self,
        producer: u64,
        target: &str,
        offset: u64,
        log: LogIdentity,
    ) -> Result<(u64, bool), Box<Response>> {
        use astream_broker::proto::{
            ERR_BAD_SUBJECT, ERR_RECORD_REFUSED, ERR_RETIRE_REFUSED, RETIRE_LOG_PREFIX,
        };
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
        let head = self.log.len() as u64;
        let conditional = self.mutant != ArmMutant::RetireIgnoresItsCondition;
        let not_this = |why: String| {
            Err(refused(
                ERR_RETIRE_REFUSED,
                format!("{RETIRE_LOG_PREFIX}{why}"),
            ))
        };
        if conditional && log != self.identity() {
            return not_this(format!(
                "the request names {log}, the log is {}",
                self.identity()
            ));
        }
        match self.index.get(target).copied() {
            Some((newest, charged)) => {
                if conditional && offset > newest {
                    return not_this(format!(
                        "{offset} is past {target}'s newest record, {newest}"
                    ));
                }
                if conditional && offset < newest {
                    return Ok((newest, true));
                }
                self.retire_charged(producer, target, charged)
            }
            None if conditional && offset >= head => {
                not_this(format!("{offset} is at or above the head, {head}"))
            }
            None => Ok((head, true)),
        }
    }

    /// The identity a `Hello` names.
    pub(crate) fn identity(&self) -> LogIdentity {
        LogIdentity {
            id: self.id,
            generation: self.generation,
        }
    }

    /// Retire `target`, charged to `charged`, for `producer`: refused for another
    /// producer's subject, or one a will can still append to.
    fn retire_charged(
        &mut self,
        producer: u64,
        target: &str,
        charged: u64,
    ) -> Result<(u64, bool), Box<Response>> {
        use astream_broker::proto::{
            ERR_RETIRE_REFUSED, RETIRE_CHARGED_PREFIX, RETIRE_WILL_PREFIX,
        };
        if charged != producer && self.mutant != ArmMutant::RetireIgnoresCharge {
            return Err(refused(
                ERR_RETIRE_REFUSED,
                format!("{RETIRE_CHARGED_PREFIX}{target} is charged to producer {charged}"),
            ));
        }
        if self.will_targets(target) && self.mutant != ArmMutant::RetireIgnoresWills {
            return Err(refused(
                ERR_RETIRE_REFUSED,
                format!("{RETIRE_WILL_PREFIX}{target} is targeted by a will that can still append"),
            ));
        }
        let removed = self
            .index
            .remove(target)
            .and_then(|(newest, _)| self.log.get(usize::try_from(newest).ok()?))
            .map(|rec| rec.body.clone())
            .unwrap_or_default();
        let off = self.append(Rec {
            subject: astream_broker::RETIRE_SUBJECT.to_string(),
            body: target.as_bytes().to_vec(),
            key: None,
            kind: Kind::Retire,
            escalated: false,
            principal: self.principal(producer).map(str::to_string),
        });
        self.retired.push((off, target.to_string(), removed));
        Ok((off, false))
    }

    /// Before `req` is served: a retire (conditional or `/a/retire`) of the subject a
    /// [`Revive`] names finds the successor's row written first, journalled as the world
    /// producer's write it is.
    fn before(&mut self, req: &Request) {
        let target = match req {
            Request::Retire { subject, .. } => subject.as_bytes(),
            Request::Publish { subject, body, .. } if subject == astream_broker::RETIRE_SUBJECT => {
                body.as_slice()
            }
            _ => return,
        };
        let Some(r) = self
            .revive
            .clone()
            .filter(|r| r.landed.is_none() && r.subject.as_bytes() == target)
        else {
            return;
        };
        self.note_event(|| Event::Publish {
            producer: r.producer,
            seq: r.seq,
            subject: r.subject.clone(),
            body: r.body.clone(),
        });
        let landed = self
            .publish(r.producer, r.seq, &r.subject, &r.body)
            .map_or(u64::MAX, |(off, _)| off);
        self.notes
            .push(format!("revive {} @{landed} before its retire", r.subject));
        if let Some(r) = self.revive.as_mut() {
            r.landed = Some(landed);
        }
    }

    fn fire(&mut self, i: usize) {
        if self
            .wills
            .get(i)
            .is_some_and(|w| !w.replaced && w.fired_at.is_none())
        {
            self.fire_will(i);
        }
    }

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

    fn note_event(&mut self, event: impl FnOnce() -> Event) {
        if let Some(journal) = self.journal.as_mut() {
            journal.push(event());
        }
    }

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

    /// The next record from `from` a tail of `filter` is handed: the held records below the
    /// base first, then the retained ones (astream's `BrokerLog::read_chunk`).
    fn next_match(&self, from: u64, filter: &Filter) -> Option<u64> {
        let base = self.ret.base;
        if from < base {
            let held = self
                .ret
                .held
                .range(from..base)
                .find(|(_, h)| filter.matches_str(&h.rec.subject))
                .map(|(&off, _)| off);
            if held.is_some() {
                return held;
            }
        }
        let start = usize::try_from(from.max(base)).ok()?;
        self.log
            .iter()
            .enumerate()
            .skip(start)
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
            log: Some(self.identity()),
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
                lineage: if self.mutant == ArmMutant::HelloNoLineage && self.lineage.is_empty() {
                    None
                } else {
                    astream_broker::Lineage::from_steps(&self.lineage)
                },
                // astream's whole set, as its `Hello` says it: the broker times and the stats
                // (astream 93ec213) are asked for by no bridge, and answered here as not simulated.
                requests: Some(
                    astream_broker::REQUEST_RETIRE
                        | astream_broker::REQUEST_GAP_NOTICES
                        | astream_broker::REQUEST_TOLD_FETCH
                        | astream_broker::REQUEST_BROKER_TIMES
                        | astream_broker::REQUEST_STATS,
                ),
                health: None,
                // The rules this open set, and none read back from an earlier one's.
                retention: self
                    .ret
                    .rules
                    .as_ref()
                    .filter(|_| self.ret.active && self.mutant != ArmMutant::Unretained)
                    .map(Retention::encode),
            },
        }
    }

    fn handle(&mut self, conn: u64, req: Request) -> Vec<Response> {
        let head = self.log.len() as u64;
        if let Some(msg) = oversized_name(&req).filter(|_| self.mutant != ArmMutant::NoNameBound) {
            return vec![Response::Error {
                code: astream_broker::proto::ERR_RECORD_REFUSED,
                msg,
            }];
        }
        match req {
            Request::Hello => vec![self.hello()],
            Request::GapNotices => {
                if let Some(c) = self.conns.get_mut(&conn) {
                    c.gaps = true;
                }
                vec![self.mark(head, String::new())]
            }
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
            Request::Retire {
                producer_id,
                subject,
                offset: at,
                log,
            } => match self.retire_if(producer_id, &subject, at, log) {
                Ok((offset, deduped)) => {
                    self.notes.push(format!(
                        "retire c{conn} {subject} if @{at} -> @{offset}{}",
                        if deduped { " moot" } else { "" }
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
                // A batch rolled back refuses the group commit in it too.
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
                let max = if self.mutant == ArmMutant::LastPastItsPage {
                    max
                } else {
                    max.min(astream_broker::broker::LAST_PAGE_MAX)
                } as usize;
                let mut out = Vec::new();
                let mut resume = String::new();
                let after_row = |after: &str| {
                    self.index
                        .range::<str, _>((
                            std::ops::Bound::Excluded(after),
                            std::ops::Bound::Unbounded,
                        ))
                        .filter(|(subject, _)| filt.matches_str(subject))
                };
                if max > 0 {
                    for (subject, (off, _)) in after_row(after.as_str()) {
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
                if self.mutant == ArmMutant::LastResumesOnlyBeforeMore
                    && after_row(resume.as_str()).next().is_none()
                {
                    resume.clear();
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
            } => self.fetch(conn, from_offset, &filter, max),
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
                reserved: self
                    .reserved
                    .get(&producer_id)
                    .filter(|_| self.mutant != ArmMutant::BudgetReservesNothing)
                    .map_or(0, |held| held.len()) as u64,
                max: astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64,
            }],
            other => vec![Response::Error {
                code: astream_broker::proto::ERR_REFUSED,
                msg: format!("malformed request (not simulated): {other:?}"),
            }],
        }
    }

    /// A `Fetch` (astream's `serve_fetch`). BELOW THE OLDEST: told, never a page that
    /// silently starts later. A reader told what retention gave up (one that asked for gap
    /// notices) reads the records retention still holds of its filter from its `from`, the
    /// notice for the range ahead of the page (`BrokerLog::fetch_told`), and is refused where
    /// it holds none there; every other is refused `retained-from=<oldest>`. A head query is
    /// never refused, and its `Mark` names the oldest.
    fn fetch(&mut self, conn: u64, from_offset: u64, filter: &str, max: u32) -> Vec<Response> {
        let Ok(filt) = Filter::new(filter) else {
            return vec![Response::Error {
                code: astream_broker::proto::ERR_BAD_FILTER,
                msg: format!("bad filter {filter:?}"),
            }];
        };
        let oldest = self.oldest(&filt);
        let told = self.conns.get(&conn).is_some_and(|c| c.gaps)
            && self.mutant != ArmMutant::FetchNeverTold;
        let below = max > 0 && from_offset < oldest;
        let (page, next, gap) = if max == 0 {
            (Vec::new(), from_offset, None)
        } else if !below || self.mutant == ArmMutant::FetchClampsBelowTheFloor {
            let (page, next) = self.fetch_from(from_offset.max(oldest), &filt, max);
            (page, next, None)
        } else {
            let (page, next) = self.fetch_from(from_offset, &filt, max);
            // Scanned in offset order: the page's first record is the lowest of the filter
            // at or above `from`, held below the oldest when it lies below it.
            let held = page
                .first()
                .map_or(next < oldest, |&(off, _, _)| off < oldest);
            if !(told && held) {
                self.ret.refusals += 1;
                self.notes.push(format!(
                    "fetch c{conn} {filter} from={from_offset} refused retained-from={oldest}"
                ));
                return vec![Response::Error {
                    code: astream_broker::proto::ERR_RECORD_REFUSED,
                    msg: format!(
                        "{}{oldest}: records of {filter} below offset {oldest} are not all \
                         kept; read from there",
                        astream_broker::proto::RETAINED_FROM_PREFIX
                    ),
                }];
            }
            let gap = self
                .gap_notice(&filt, from_offset, &mut Told::at(from_offset))
                .filter(|_| self.mutant != ArmMutant::FetchToldNoNotice);
            if let Some(g) = gap {
                self.ret.told.push((conn, g));
            }
            (page, next, gap)
        };
        let mut out: Vec<Response> = gap.map(Response::Gap).into_iter().collect();
        out.extend(
            page.into_iter()
                .map(|(offset, subject, body)| Response::Delivery {
                    offset,
                    subject,
                    body,
                }),
        );
        let mut mark = self.mark(next, String::new());
        if let Response::Mark { oldest: o, .. } = &mut mark {
            if self.mutant != ArmMutant::FetchNamesNoOldest {
                *o = Some(oldest);
            }
        }
        out.push(mark);
        out
    }

    /// The scan every `Fetch` shares, from `from` (astream's `BrokerLog::fetch_from`): at most
    /// `FETCH_SCAN_MAX` records, the held ones below the base first, each scanned as a
    /// retained one, then the retained suffix to the head; `max` records of `filter` at most.
    /// The page, and the offset after the last record it scanned.
    fn fetch_from(&self, from: u64, filt: &Filter, max: u32) -> (Vec<(u64, String, Vec<u8>)>, u64) {
        let head = self.log.len() as u64;
        let max = max as usize;
        let mut out = Vec::new();
        let mut budget = if self.mutant == ArmMutant::FetchUnbounded {
            u64::MAX
        } else {
            astream_broker::broker::FETCH_SCAN_MAX as u64
        };
        let base = self.ret.base;
        let mut start = from;
        if from < base {
            for (&off, h) in self.ret.held.range(from..base) {
                if budget == 0 {
                    return (out, start);
                }
                budget -= 1;
                start = off + 1;
                if filt.matches_str(&h.rec.subject) {
                    out.push((off, h.rec.subject.clone(), h.rec.body.clone()));
                    if out.len() >= max {
                        return (out, start);
                    }
                }
            }
        }
        let start = start.max(base);
        let limit = start.saturating_add(budget).min(head);
        let mut next = start;
        let mut off = start;
        while off < limit {
            next = off + 1;
            let rec = &self.log[usize::try_from(off).unwrap_or(0)];
            if !rec.hidden() && filt.matches_str(&rec.subject) {
                out.push((off, rec.subject.clone(), rec.body.clone()));
                if out.len() >= max {
                    break;
                }
            }
            off = next;
        }
        (out, next)
    }

    fn register(
        &mut self,
        conn: u64,
        producer_id: u64,
        producer_seq: u64,
        subject: String,
        body: Vec<u8>,
    ) -> Vec<Response> {
        use astream_broker::proto::{ERR_RECORD_REFUSED, ERR_REFUSED, ERR_SUBJECT_BOUND};
        if let Some(refusal) =
            subject_error(&subject).filter(|_| self.mutant != ArmMutant::WillTakesAnySubject)
        {
            return vec![*refusal];
        }
        let held = self
            .wills
            .iter()
            .find(|w| w.conn == Some(conn) && !w.replaced)
            .map(|w| w.producer)
            .filter(|_| self.mutant != ArmMutant::WillOfASecondProducer);
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
                code: if self.mutant == ArmMutant::WillAckHalfRefused {
                    ERR_REFUSED
                } else {
                    ERR_RECORD_REFUSED
                },
                msg: "will was not persisted: a will's sequence in the ack half".to_string(),
            }];
        }
        if let Some(refusal) = self.race_at_will(producer_id, producer_seq) {
            return refusal;
        }
        if self.mutant != ArmMutant::WillPastTheBudget
            && self.past_the_budget(producer_id, &subject)
        {
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
            c.told = c.gaps.then(|| Told::at(from));
        }
        Vec::new()
    }

    /// The gap notice tail `conn` is owed before its next record, if it asked and has
    /// something to read (astream's tail looks only then); `told` is updated when `take`.
    fn owed_gap(&mut self, conn: u64, take: bool) -> Option<astream_broker::Gap> {
        if self.mutant == ArmMutant::NoGapNotices {
            return None;
        }
        let c = self.conns.get(&conn)?;
        let (cursor, filter) = c.sub.clone()?;
        let mut told = c.told?;
        if (self.log.len() as u64) <= cursor {
            return None;
        }
        let gap = self.gap_notice(&filter, cursor, &mut told);
        if take {
            if let Some(c) = self.conns.get_mut(&conn) {
                c.told = Some(told);
            }
        }
        gap
    }
}

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

fn oversized_name(req: &Request) -> Option<String> {
    let names: Vec<(&str, &str)> = match req {
        Request::Publish { subject, .. }
        | Request::Will { subject, .. }
        | Request::Retire { subject, .. } => vec![("subject", subject)],
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

fn refused(code: u8, msg: String) -> Box<Response> {
    Box::new(Response::Error { code, msg })
}

fn put(out: &mut VecDeque<u8>, resp: &Response) {
    let mut bytes = Vec::new();
    astream_broker::proto::write_frame(&mut bytes, &encode_response(resp))
        .expect("a frame to memory");
    out.extend(bytes);
}

/// DIAL the world's broker for process `p`.
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

/// [`dial`]'s connection as the raw byte stream astream's client would wrap.
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
        s.conns.insert(id, ConnState::new(p.pid));
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
                // TOLD FIRST: what retention gave up below the tail, ahead of what it reads.
                if let Some(gap) = s.owed_gap(self.conn, true) {
                    s.note_event(|| Event::Deliver(self.conn));
                    s.ret.told.push((self.conn, gap));
                    let line = format!(
                        "gap c{} {}..{} records={:?}",
                        self.conn, gap.from, gap.to, gap.records
                    );
                    s.notes.push(line);
                    if let Some(c) = s.conns.get_mut(&self.conn) {
                        put(&mut c.out, &Response::Gap(gap));
                    }
                    out_empty = false;
                } else if let Some(off) = s.next_match(cursor, &filter) {
                    // THE REORDER aterm-0 must survive: a record handed to a reader while its
                    // publisher's ack is still held.
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
        let commit = |p: Option<&Vec<u8>>| {
            matches!(
                p.and_then(|p| decode_request(p)),
                Some(Request::Commit { .. })
            )
        };
        let publish = |p: Option<&Vec<u8>>| {
            matches!(
                p.and_then(|p| decode_request(p)),
                Some(Request::Publish { .. })
            )
        };
        if commit(frames.first()) {
            let mut s = lock(&self.process.core.broker.state);
            match frames.len() {
                1 => s.commits_alone += 1,
                2 if publish(frames.get(1)) => s.commits_corked += 1,
                _ => {}
            }
        }
        for payload in frames {
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
                        s.before(&req);
                        s.note_event(|| Event::Request(self.conn, req.clone()));
                        s.handle(self.conn, req)
                    }
                    None => vec![Response::Error {
                        code: astream_broker::proto::ERR_REFUSED,
                        msg: "malformed request".to_string(),
                    }],
                };
                s.answer(self.conn, &responses);
                if subscribing
                    && !responses.is_empty()
                    && s.mutant != ArmMutant::RefusedSubscribeLives
                {
                    s.end(self.conn);
                }
            }
            self.process.step("bus-after");
        }
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

/// THE LAW CHECK of one restart.
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
