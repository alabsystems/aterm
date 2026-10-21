// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUS DOOR'S SIMULATED ARM: an in-memory model of astream's broker,
//! served over astream's OWN wire (`astream_broker::proto`'s frames, decoded
//! and encoded by astream's own codec) to astream's OWN client, so the bridge
//! reaches it through exactly the calls it makes on a real broker.
//!
//! ## What it serves
//!
//! The requests the bridge makes, with astream's semantics: `Hello` (the log's
//! identity, the tier served, the LINEAGE), `Publish` (deduped by
//! `(producer, seq)`, a reserved `/a/...` subject refused, `/a/retire` the
//! subject retire), a pipelined publish's acks in order, `Will` (a hidden
//! registration record; it fires when its connection ends and again at every
//! open while the log holds it, deduped like a publish and fenced by any later
//! sequence of its producer below [`astream_broker::ACK_SEQ_BASE`]), `Last`,
//! `Fetch`, `Commit` (a hidden record; a group resumes at `upto + 1`, max-
//! folded), `Subscribe` and `SubscribeGroup` (every matching record from the
//! start, in order, then live), `IdleMarks`, `Keepalive`, `Budget`. Every
//! `Mark` carries the durable head. No capability is checked: the simulated
//! broker is an unguarded one, as a test's broker is.
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
//! crash never moves.
//!
//! ## Deterministic by construction
//!
//! A request is processed inside the client's own `write`, on the bridge's
//! thread, and its reply queued for the client's next `read`; a subscription's
//! deliveries are produced only when the scheduler's pump reads it
//! ([`crate::sim::Core::pump`]). A connection a client drops or closes ends at
//! that call, and its will fires then.

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
    open: bool,
    /// `Some((cursor, filter))` once it is a subscription.
    sub: Option<(u64, Filter)>,
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
    wills: Vec<Will>,
    conns: BTreeMap<u64, ConnState>,
    next_conn: u64,
    /// Producer id -> principal, for the principal rule.
    principals: BTreeMap<u64, String>,
    /// Refuse the next this-many publishes by a NODE producer `ERR_REFUSED`
    /// (a batch rolled back), appending nothing.
    pub(crate) refuse_next: u32,
    /// NEGATIVE CONTROLS of the arm itself ([`law_check`]'s): lose below the
    /// durable head; lose with no move.
    pub(crate) mutant: ArmMutant,
    notes: Vec<String>,
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
}

/// One visible record as the oracle reads it: `(offset, subject, body,
/// producer key)`.
pub(crate) type Visible = (u64, String, Vec<u8>, Option<(u64, u64)>);

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
                wills: Vec::new(),
                conns: BTreeMap::new(),
                next_conn: 1,
                principals: BTreeMap::new(),
                refuse_next: 0,
                mutant: ArmMutant::None,
                notes: Vec::new(),
            }),
        }
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
        s.publish(producer, seq, subject, body).ok()
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
        let mut s = lock(&self.state);
        let before = s.log.clone();
        let durable_before = s.durable;
        let generation_before = s.generation;
        let tier = s.tier;
        // Every connection ends with the process; no will fires until the
        // next open re-fires every will the log still holds.
        s.up = false;
        for c in s.conns.values_mut() {
            c.open = false;
        }
        let cuts = s.allowed_cuts(stop);
        let pick = usize::try_from(rng.below(cuts.len() as u64)).unwrap_or(0);
        let mut kept = cuts.get(pick).copied().unwrap_or(s.log.len() as u64);
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
    /// durable (the open's flush), and re-fire every will it still holds.
    pub(crate) fn start(&self) {
        let mut s = lock(&self.state);
        s.rebuild();
        s.durable = s.log.len() as u64;
        s.up = true;
        let pending: Vec<usize> = s
            .wills
            .iter()
            .enumerate()
            .filter(|(_, w)| !w.replaced && w.fired_at.is_none() && w.reg_off < s.log.len() as u64)
            .map(|(i, _)| i)
            .collect();
        for i in pending {
            s.fire(i);
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
                    *m = (*m).max(key.1);
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
                    *e = (*e).max(*upto);
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
    }

    fn append(&mut self, rec: Rec) -> u64 {
        let off = self.log.len() as u64;
        if let Some(key) = rec.key {
            self.dedup.insert(key, off);
            if key.1 < astream_broker::ACK_SEQ_BASE {
                let m = self.max_seq.entry(key.0).or_insert(0);
                *m = (*m).max(key.1);
            }
        }
        if let Kind::Plain = rec.kind {
            let charged = self
                .index
                .get(&rec.subject)
                .map_or_else(|| rec.key.map_or(0, |k| k.0), |(_, c)| *c);
            self.index.insert(rec.subject.clone(), (off, charged));
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

    /// One publish: dedup, the reserved subjects, the retire, the append.
    fn publish(
        &mut self,
        producer: u64,
        seq: u64,
        subject: &str,
        body: &[u8],
    ) -> Result<(u64, bool), Box<Response>> {
        if subject == astream_broker::RETIRE_SUBJECT && seq == 0 {
            let target = String::from_utf8_lossy(body).into_owned();
            return Ok(match self.index.get(&target) {
                Some((_, charged)) if *charged == producer => {
                    self.index.remove(&target);
                    let off = self.append(Rec {
                        subject: subject.to_string(),
                        body: body.to_vec(),
                        key: None,
                        kind: Kind::Retire,
                        escalated: false,
                        principal: self.principal(producer).map(str::to_string),
                    });
                    (off, false)
                }
                Some((at, _)) => {
                    return Err(refused(
                        astream_broker::proto::ERR_RETIRE_REFUSED,
                        format!("charged: {target} at {at} is another producer's"),
                    ))
                }
                None => (self.log.len() as u64, true),
            });
        }
        if astream_wire::Subject::new(subject).is_err() {
            return Err(refused(
                astream_broker::proto::ERR_BAD_SUBJECT,
                format!("bad subject {subject:?}"),
            ));
        }
        if subject.starts_with("/a/") {
            return Err(refused(
                astream_broker::proto::ERR_RECORD_REFUSED,
                format!("reserved subject {subject:?}"),
            ));
        }
        if let Some(&off) = self.dedup.get(&(producer, seq)) {
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
        let principal = self.principal(producer).map(str::to_string);
        let escalated = self
            .table
            .escalates(subject, &Kind::Plain, principal.as_deref());
        let off = self.append(Rec {
            subject: subject.to_string(),
            body: body.to_vec(),
            key: Some((producer, seq)),
            kind: Kind::Plain,
            escalated,
            principal,
        });
        Ok((off, false))
    }

    /// Fire will `i`: an ordinary publish under its key, fenced by any later
    /// sequence of its producer.
    fn fire(&mut self, i: usize) {
        let Some(w) = self.wills.get(i).cloned() else {
            return;
        };
        if w.replaced || w.fired_at.is_some() {
            return;
        }
        let fenced = self.max_seq.get(&w.producer).is_some_and(|m| *m > w.seq);
        let at = if fenced {
            self.notes.push(format!("will seq={} fenced", w.seq));
            self.log.len() as u64
        } else {
            match self.publish(w.producer, w.seq, &w.subject, &w.body) {
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

    /// Connection `conn` ended: its will fires (while the broker is up).
    fn end(&mut self, conn: u64) {
        if let Some(c) = self.conns.get_mut(&conn) {
            c.open = false;
        }
        if !self.up {
            return;
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
                lineage: if self.lineage.is_empty() {
                    None
                } else {
                    crate::durability::lineage_of(&self.lineage)
                },
                requests: Some(astream_broker::REQUEST_RETIRE),
                health: None,
            },
        }
    }

    /// Answer one request on `conn`: the frames it writes back.
    fn handle(&mut self, conn: u64, req: Request) -> Vec<Response> {
        let head = self.log.len() as u64;
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
            } => {
                if producer_seq >= astream_broker::ACK_SEQ_BASE {
                    return vec![Response::Error {
                        code: astream_broker::proto::ERR_REFUSED,
                        msg: "a will's sequence in the ack half".to_string(),
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
            Request::Commit { group, upto } => {
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
                *e = (*e).max(upto);
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
                let max = max.min(astream_broker::broker::LAST_PAGE_MAX) as usize;
                let rows: Vec<(String, u64)> = self
                    .index
                    .iter()
                    .filter(|(subject, _)| subject.as_str() > after.as_str())
                    .filter(|(subject, _)| filt.matches_str(subject))
                    .map(|(subject, (off, _))| (subject.clone(), *off))
                    .collect();
                let more = rows.len() > max;
                self.notes.push(format!(
                    "last c{conn} {filter} after={after:?}: {} rows",
                    rows.len()
                ));
                let mut out: Vec<Response> = rows
                    .iter()
                    .take(max)
                    .map(|(subject, off)| Response::Delivery {
                        offset: *off,
                        subject: subject.clone(),
                        body: self.log[usize::try_from(*off).unwrap_or(0)].body.clone(),
                    })
                    .collect();
                let resume = if more {
                    rows.get(max.saturating_sub(1))
                        .map_or_else(String::new, |(s, _)| s.clone())
                } else {
                    String::new()
                };
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
                let mut out = Vec::new();
                let mut next = from_offset;
                if max > 0 {
                    let mut off = from_offset;
                    while off < head {
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
                out.push(self.mark(next, String::new()));
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
            Request::Budget { producer_id } => {
                let subjects = self
                    .index
                    .values()
                    .filter(|(_, charged)| *charged == producer_id)
                    .count() as u64;
                vec![Response::Budget {
                    subjects,
                    reserved: 0,
                    max: astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64,
                }]
            }
            other => vec![Response::Error {
                code: astream_broker::proto::ERR_REFUSED,
                msg: format!("malformed request (not simulated): {other:?}"),
            }],
        }
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
                open: true,
                sub: None,
            },
        );
        id
    };
    p.core.note(format!("dial pid={} conn={conn}", p.pid));
    let stream = SimStream {
        conn,
        process: Arc::clone(p),
    };
    Ok((
        Conn::from_stream(Box::new(stream)),
        BusCloser::Sim(SimCloser {
            conn,
            process: Arc::clone(p),
        }),
    ))
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
                    let rec = s.log[usize::try_from(off).unwrap_or(0)].clone();
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
                let responses = match req {
                    Some(req) => s.handle(self.conn, req),
                    None => vec![Response::Error {
                        code: astream_broker::proto::ERR_REFUSED,
                        msg: "malformed request".to_string(),
                    }],
                };
                if let Some(c) = s.conns.get_mut(&self.conn) {
                    for r in &responses {
                        put(&mut c.out, r);
                    }
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
