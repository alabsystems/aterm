// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fixed metadata, separately framed authority evidence and causal membership repair.
//! Membership evidence never grants table or credential permissions by itself.
//!
//! Two wires share these frames ([`Wire`]). Under `atdb/5`, once metadata names each side's
//! request, each states what it holds of the membership log ([`View`]). A peer then pushes only
//! the records of its seat evidence outside the past of what the other stated, at most
//! [`MAX_EVIDENCE`] bytes, and the other judges that push in a copy of its own records, writing
//! nothing before the peer is authenticated. Bulk repair follows in membership rounds bounded by
//! [`BATCH_RECORDS`] and [`BATCH_BYTES`]. Under `atdb/4` each side sends its whole seat witness,
//! as 0.153 does, so a 0.153 peer and this build still meet.

use super::*;
use aterm_secrets::{
    client,
    commons::{
        binding::{MAX_CHALLENGE, Pending, RESPONSE_LEN},
        bootstrap::Bootstrap,
    },
    crossing::RecordsBound,
    membership_log::{Body, Log, MAX_RECORD, MAX_RECORDS_STREAM, Record, Store as Records},
};

const META_LEN: usize = HELLO_FIXED + 6;
const RECORDS: u8 = 1;
const APPLICATION: u8 = 2;
const CREDENTIALS: u8 = 4;
const PENDING: u8 = 8;
const FLAGS: u8 = RECORDS | APPLICATION | CREDENTIALS | PENDING;
const EVIDENCE_HEADER: usize = 22;
/// The most bytes of records a peer pushes as its seat evidence before it is authenticated: what
/// an `ACX2` hello carried of a log witness before it ([`MAX_ACX2_HELLO`]).
pub(super) const MAX_EVIDENCE: usize = MAX_ACX2_HELLO - HELLO_FIXED;
/// The most records one `atdb/5` membership round pushes.
pub(super) const BATCH_RECORDS: usize = 1024;
/// The most bytes one `atdb/5` membership round pushes, but for a single record larger than it
/// (up to [`MAX_RECORD`]), which travels alone.
pub(super) const BATCH_BYTES: usize = 1024 * 1024;
const _: () = assert!(MAX_RECORD <= MAX_EVIDENCE);
/// The most records one `atdb/4` membership round pushes, and a 0.153 peer takes.
pub(super) const WHOLE_BATCH_RECORDS: usize = 256;
/// The most bytes one `atdb/4` membership round pushes, and a 0.153 peer takes: room for any one
/// record.
pub(super) const WHOLE_BATCH_BYTES: usize = 4 * 1024 * 1024;
const _: () = assert!(WHOLE_BATCH_BYTES >= MAX_RECORD);

/// The frontier wire a session speaks, as its ALPN named it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Wire {
    /// `atdb/4` ([`fleet::ALPN_DATABASE_FRONTIER`]), frame for frame as 0.153 released it, so a
    /// 0.153 peer and this build meet on it: each side sends the whole seat witness its bootstrap
    /// selected (never none), judged from that witness's genesis alone, at most
    /// [`MAX_RECORDS_STREAM`] bytes; membership rounds move at most [`WHOLE_BATCH_RECORDS`]
    /// records or [`WHOLE_BATCH_BYTES`] bytes, and records left for a later session end this one
    /// before table repair.
    Whole,
    /// `atdb/5` ([`fleet::ALPN_DATABASE_DELTA`]): each side states what it holds before any
    /// evidence ([`View`]) and pushes only the seat evidence the other lacks, perhaps none, at
    /// most [`MAX_EVIDENCE`] bytes, judged in a copy of the receiver's records; membership rounds
    /// move at most [`BATCH_RECORDS`] records or [`BATCH_BYTES`] bytes, and records left for a
    /// later session hold up no table repair.
    Delta,
}

impl Wire {
    /// The most records one membership round pushes.
    pub(super) const fn batch_records(self) -> usize {
        match self {
            Self::Whole => WHOLE_BATCH_RECORDS,
            Self::Delta => BATCH_RECORDS,
        }
    }

    /// The most bytes one membership round pushes, but for a single record larger than it.
    pub(super) const fn batch_bytes(self) -> usize {
        match self {
            Self::Whole => WHOLE_BATCH_BYTES,
            Self::Delta => BATCH_BYTES,
        }
    }

    /// The most bytes of seat evidence a peer sends before it is authenticated.
    pub(super) const fn evidence_bytes(self) -> usize {
        match self {
            Self::Whole => MAX_RECORDS_STREAM,
            Self::Delta => MAX_EVIDENCE,
        }
    }

    /// The most membership rounds a session runs between peers that hold at most `capacity`
    /// records, which both sides count alike. Under `atdb/5`, every two rounds a side with
    /// records left sends a full batch of records or of bytes, or every record left, so the
    /// rounds a log of `capacity` records and [`MAX_RECORDS_STREAM`] bytes needs, and two more to
    /// agree that nothing is left. Under `atdb/4`, 0.153's count: `capacity` over the number of
    /// largest records ([`MAX_RECORD`]) one batch's bytes hold, and two more.
    pub(super) const fn round_cap(self, capacity: usize) -> usize {
        match self {
            Self::Whole => capacity / (WHOLE_BATCH_BYTES / MAX_RECORD) + 2,
            Self::Delta => {
                2 * (capacity.div_ceil(BATCH_RECORDS) + MAX_RECORDS_STREAM.div_ceil(BATCH_BYTES))
                    + 2
            }
        }
    }
}

pub(super) struct Metadata {
    pub(super) request: Request,
    pub(super) bound: RecordsBound,
    pub(super) flags: u8,
}

impl Metadata {
    fn own(endpoint: &Endpoint) -> Result<Self> {
        Ok(Self {
            request: Request::decode(&endpoint.hello[84..HELLO_FIXED]).map_err(failure)?,
            bound: own_bound(endpoint)?,
            flags: (u8::from(endpoint.paths.is_some()) * RECORDS)
                | (u8::from(endpoint.application.is_some()) * APPLICATION)
                | (u8::from(endpoint.application.is_some() && endpoint.credentials.is_some())
                    * CREDENTIALS)
                | (u8::from(endpoint.pending) * PENDING),
        })
    }

    pub(super) fn encode(&self, trust: Trust) -> Vec<u8> {
        let mut wire = Vec::with_capacity(META_LEN);
        wire.extend_from_slice(b"ACX4");
        wire.extend_from_slice(&trust.network);
        wire.extend_from_slice(&trust.authority);
        wire.extend_from_slice(&trust.genesis);
        wire.extend_from_slice(self.request.bytes());
        put_bound(&mut wire, self.bound);
        wire.push(self.flags);
        wire
    }

    pub(super) fn decode(trust: Trust, wire: &[u8]) -> Result<Self> {
        if wire.len() != META_LEN
            || &wire[..4] != b"ACX4"
            || wire[4..20] != trust.network
            || wire[20..52] != trust.authority
            || wire[52..84] != trust.genesis
            || wire[META_LEN - 1] & !FLAGS != 0
        {
            return Err(Error::new("Commons frontier metadata or trust pins differ"));
        }
        let request = Request::decode(&wire[84..HELLO_FIXED]).map_err(failure)?;
        if *request.binding().network() != trust.network
            || *request.binding().authority() != trust.authority
        {
            return Err(Error::new("Commons device belongs to another network"));
        }
        let flags = wire[META_LEN - 1];
        if flags & CREDENTIALS != 0 && flags & APPLICATION == 0 {
            return Err(Error::new("invalid Commons frontier capabilities"));
        }
        if flags & PENDING != 0
            && (flags & RECORDS == 0 || flags & (APPLICATION | CREDENTIALS) != 0)
        {
            return Err(Error::new("invalid pending Commons capabilities"));
        }
        Ok(Self {
            request,
            bound: read_bound(&wire[HELLO_FIXED..HELLO_FIXED + 5])?,
            flags,
        })
    }
}

fn put_bound(wire: &mut Vec<u8>, bound: RecordsBound) {
    wire.extend_from_slice(&bound.held.to_be_bytes());
    wire.push(u8::from(bound.crossed));
}

fn read_bound(wire: &[u8]) -> Result<RecordsBound> {
    if wire.len() != 5 || wire[4] > 1 {
        return Err(Error::new("invalid Commons records bound"));
    }
    let held = u32::from_be_bytes(wire[..4].try_into().map_err(failure)?);
    if held == 0 {
        return Err(Error::new("invalid Commons records capacity"));
    }
    Ok(RecordsBound {
        held,
        crossed: wire[4] != 0,
    })
}

fn own_bound(endpoint: &Endpoint) -> Result<RecordsBound> {
    let mut bound = endpoint.records_bound;
    if let Some(paths) = &endpoint.paths {
        bound.crossed |= client::records_marked_crossed(paths)
            .map_err(|error| local_failure("records protocol", error))?;
    }
    Ok(bound)
}

fn compatible(endpoint: &Endpoint, remote: RecordsBound) -> Result<RecordsBound> {
    let local = own_bound(endpoint)?;
    if !local.can_exchange(remote) {
        return Err(Error::new(
            "Commons frontier record capacities are incompatible",
        ));
    }
    Ok(local)
}

fn trade(stream: &mut (impl Read + Write), side: Side, own: &[u8], max: usize) -> Result<Vec<u8>> {
    match side {
        Side::Initiator => {
            send_frame(stream, own)?;
            receive_frame(stream, max)
        }
        Side::Responder => {
            let peer = receive_frame(stream, max)?;
            send_frame(stream, own)?;
            Ok(peer)
        }
    }
}

/// What this endpoint holds of the membership log for one `atdb/5` session, read once: the
/// frontier and ancestry checkpoints it states before any evidence ([`View::have`]), and the
/// records a peer's evidence push is judged in a copy of ([`overlay`]). Custody whose log is
/// another root's states nothing, and a push to it is judged from its genesis alone, as a log
/// witness was; so does a journal endpoint's custody that does not read
/// ([`super::Custody::Journal`]). An `atdb/4` session reads none ([`View::NONE`]).
pub(super) struct View(Option<Records>);

impl View {
    /// Nothing held: what an `atdb/4` session, which states nothing, judges in.
    pub(super) const NONE: Self = Self(None);

    /// # Errors
    /// The records of an endpoint built from its membership log ([`super::Custody::Log`]) do
    /// not read: a push judged from its own genesis would pass a Mac those records remove.
    pub(super) fn of(endpoint: &Endpoint) -> Result<Self> {
        let custody = endpoint.custody();
        let (super::Custody::Log(paths) | super::Custody::Journal(paths)) = custody else {
            return Ok(Self(None));
        };
        let store = match super::held_records(paths) {
            Ok(store) => store,
            Err(error) => return custody.unread(error).map(|()| Self(None)),
        };
        let ours = store.log().is_some_and(|log| {
            matches!(
                log.genesis().body(),
                Body::Genesis { root, .. }
                    if root.network == endpoint.trust.network
                        && root.authority == endpoint.trust.authority
            ) && endpoint
                .snapshot_log
                .as_ref()
                .is_none_or(|own| own.network() == log.network())
        });
        Ok(Self(ours.then_some(store)))
    }

    pub(super) fn have(&self) -> Vec<Digest> {
        self.0.as_ref().map_or_else(Vec::new, knowledge)
    }
}

/// Trade what each side holds ([`View::have`]) once metadata named the peer's identity and before
/// any evidence, so each pushes only what the other lacks. An initiator that gets no answer marks
/// it [`Repair::unanswered`], as it would its evidence.
fn trade_haves(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    side: Side,
    repair: &mut Repair,
    peer: &mut Metadata,
    view: &View,
) -> Result<Vec<Digest>> {
    let bound = own_bound(endpoint)?;
    let own = encode_frontier(bound, &view.have());
    let max = 13 + bound.held as usize * 32;
    let wire = match side {
        Side::Initiator => {
            send_frame(stream, &own)?;
            repair.receive_bounded_answer(stream, side, max)?
        }
        Side::Responder => {
            let wire = receive_frame(stream, max)?;
            send_frame(stream, &own)?;
            wire
        }
    };
    decode_frontier(endpoint, peer, &wire)
}

/// The records of `records` outside the past of what a peer stated it holds (`have`), in their
/// order: what a push sends. The past of each stated id is read in `held` and in this endpoint's
/// records custody, wherever one holds it; an id neither holds reaches nothing, so a peer that
/// names records this side lacks is sent the rest.
fn lacked<'a>(
    endpoint: &Endpoint,
    held: Option<&Log>,
    records: impl IntoIterator<Item = &'a Record>,
    have: &[Digest],
) -> Vec<Record> {
    let mut known = BTreeSet::new();
    let custody = endpoint
        .paths
        .as_ref()
        .and_then(|paths| client::load_records(paths).ok());
    for log in held
        .into_iter()
        .chain(custody.as_ref().and_then(Records::log))
    {
        known.extend(log.ancestor_closure(have).iter().map(|record| *record.id()));
    }
    records
        .into_iter()
        .filter(|record| !known.contains(record.id()))
        .cloned()
        .collect()
}

/// This endpoint's own authority evidence: the seat witness its bootstrap selected, or a journal
/// node that binds its request.
pub(super) enum Evidence {
    Log(Arc<Log>),
    Node(Vec<u8>),
}

/// A peer's authority evidence as this endpoint took it.
pub(super) enum Received {
    /// Under `atdb/4`: the peer's whole seat witness, a log of its own from its genesis.
    Witness(Box<Log>),
    /// Under `atdb/5`: the records the peer pushed that the copy of this endpoint's records
    /// ([`View`]) holding them took ([`overlay`]), and that copy.
    Push {
        overlay: Box<Log>,
        pushed: Vec<Record>,
    },
    /// A journal node that binds the peer's request.
    Node(Vec<u8>),
}

impl Received {
    /// The membership log the peer's seat was judged in, if its evidence was records.
    fn judged(&self) -> Option<&Log> {
        match self {
            Self::Witness(log) | Self::Push { overlay: log, .. } => Some(log),
            Self::Node(_) => None,
        }
    }
}

fn own_evidence(endpoint: &Endpoint, request: &Request) -> Result<Evidence> {
    if let Some(log) = endpoint.snapshot_log.clone() {
        admit_by_log_witness(&log, request.binding())?;
        Ok(Evidence::Log(log))
    } else {
        let wire = endpoint.hello[HELLO_FIXED..].to_vec();
        check_witness(endpoint.trust, request, &wire)?;
        Ok(Evidence::Node(wire))
    }
}

/// What one session's evidence follows: its wire, and under `atdb/5` what this endpoint holds
/// ([`View`]) and what the peer stated it holds (`have`); under `atdb/4`, nothing of either.
pub(super) struct Session<'a> {
    pub(super) wire: Wire,
    pub(super) view: &'a View,
    pub(super) have: &'a [Digest],
}

/// Push this endpoint's evidence for `target`, at most [`Wire::evidence_bytes`]: of a seat
/// witness, under `atdb/4` all of it, and under `atdb/5` only its records outside the past of
/// what the peer stated it holds, so a peer that holds the whole witness is sent none.
pub(super) fn send_evidence(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    remote: RecordsBound,
    evidence: &Evidence,
    target: &Request,
    session: &Session,
) -> Result<()> {
    let wire = session.wire;
    let records = match evidence {
        Evidence::Log(log) => {
            // Bootstrap already selected the evidence needed for the seat,
            // restored node keys and audience access. Taking only the seat's
            // ancestors again can discard those later proofs.
            admit_by_log_witness(log, target.binding())?;
            match wire {
                Wire::Whole => log.records().cloned().collect(),
                Wire::Delta => lacked(endpoint, Some(log), log.records(), session.have),
            }
        }
        Evidence::Node(_) => Vec::new(),
    };
    let wires: Vec<&[u8]> = match evidence {
        Evidence::Log(_) => records.iter().map(Record::bytes).collect(),
        Evidence::Node(node) => vec![node],
    };
    send_evidence_wires(
        endpoint,
        stream,
        remote,
        wire,
        matches!(evidence, Evidence::Log(_)),
        &wires,
        wire.evidence_bytes(),
    )
}

/// Frame evidence: a journal node alone, or records (under `atdb/5` none among them where the
/// peer holds them all, under `atdb/4` at least one, as 0.153 requires), within the held bounds
/// of both sides and `max_bytes`.
fn send_evidence_wires(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    remote: RecordsBound,
    wire: Wire,
    log: bool,
    wires: &[&[u8]],
    max_bytes: usize,
) -> Result<()> {
    let bound = compatible(endpoint, remote)?;
    let bytes = wires
        .iter()
        .try_fold(0usize, |n, frame| n.checked_add(frame.len()))
        .ok_or_else(|| Error::new("Commons evidence length overflow"))?;
    let counted = if log {
        wire == Wire::Delta || !wires.is_empty()
    } else {
        wires.len() == 1
    };
    if !counted
        || wires.len() > bound.held as usize
        || wires.len() > remote.held as usize
        || bytes > max_bytes
    {
        return Err(Error::new("Commons evidence exceeds its bound"));
    }
    let mut header = b"AEV4".to_vec();
    put_bound(&mut header, bound);
    header.push(u8::from(log));
    header.extend_from_slice(&(wires.len() as u32).to_be_bytes());
    header.extend_from_slice(&(bytes as u64).to_be_bytes());
    send_frame(stream, &header)?;
    for frame in wires {
        compatible(endpoint, remote)?;
        send_frame(stream, frame)?;
    }
    Ok(())
}

/// Take a peer's evidence for `target`, at most `max_bytes` of it: a journal node that binds it,
/// or records that must give `target` a seat it has not lost, judged under `atdb/4` as a whole
/// witness from its genesis, as 0.153 judges it, and under `atdb/5` as a push in an [`overlay`] of
/// the session's [`View`]. Nothing it carries is written.
fn receive_evidence(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    peer: &mut Metadata,
    target: &Request,
    answer: Option<&mut Repair>,
    session: &Session,
    max_bytes: usize,
) -> Result<Received> {
    let wire = session.wire;
    let header = match answer {
        Some(repair) => repair.receive_bounded_answer(stream, Side::Initiator, EVIDENCE_HEADER)?,
        None => receive_frame(stream, EVIDENCE_HEADER)?,
    };
    if header.len() != EVIDENCE_HEADER || &header[..4] != b"AEV4" || header[9] > 1 {
        return Err(Error::new("invalid Commons evidence header"));
    }
    let bound = read_bound(&header[4..9])?;
    let local = compatible(endpoint, bound)?;
    peer.bound = bound;
    let count = u32::from_be_bytes(header[10..14].try_into().map_err(failure)?) as usize;
    let bytes = u64::from_be_bytes(header[14..22].try_into().map_err(failure)?);
    let is_log = header[9] != 0;
    if (wire == Wire::Whole && count == 0)
        || count > bound.held as usize
        || count > local.held as usize
        || (!bound.crossed && count > aterm_secrets::membership_log::LEGACY_RECORDS)
        || bytes > max_bytes as u64
        || (!is_log && (count != 1 || bytes > MAX_NODE as u64))
    {
        return Err(Error::new("Commons evidence exceeds its bound"));
    }
    let mut actual = 0u64;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        compatible(endpoint, bound)?;
        let remaining = usize::try_from(bytes - actual).unwrap_or(usize::MAX);
        let frame = receive_frame(
            stream,
            remaining.min(if is_log { MAX_RECORD } else { MAX_NODE }),
        )?;
        actual = actual
            .checked_add(frame.len() as u64)
            .filter(|n| *n <= bytes)
            .ok_or_else(|| Error::new("Commons evidence exceeds its declared length"))?;
        if !is_log {
            if actual != bytes {
                return Err(Error::new("Commons evidence length differs"));
            }
            check_witness(endpoint.trust, target, &frame)?;
            return Ok(Received::Node(frame));
        }
        records.push(Record::decode(&frame).map_err(failure)?);
    }
    if actual != bytes {
        return Err(Error::new("Commons evidence length differs"));
    }
    #[cfg(test)]
    if mutants::on(mutants::Mutant::StoresPushFirst)
        && let Some(paths) = &endpoint.paths
    {
        let wires: Vec<_> = records.iter().map(|r| r.bytes().to_vec()).collect();
        let _ = client::retain_bounded_records_for_commons(
            paths,
            endpoint.trust.network,
            endpoint.trust.authority,
            bound,
            &wires,
        );
    }
    match wire {
        Wire::Whole => {
            let log = log_witness_from_records(endpoint.trust, records)?;
            if endpoint
                .snapshot_log
                .as_ref()
                .is_some_and(|own| own.network() != log.network())
            {
                return Err(Error::new("Commons peer belongs to another membership log"));
            }
            admit_by_log_witness(&log, target.binding())?;
            Ok(Received::Witness(Box::new(log)))
        }
        Wire::Delta => {
            let (overlay, pushed) = overlay(endpoint, session.view, records)?;
            admit_by_log_witness(&overlay, target.binding())?;
            Ok(Received::Push {
                overlay: Box::new(overlay),
                pushed,
            })
        }
    }
}

/// `pushed` held in a copy of `view`'s records, or in a log of the genesis it begins with where
/// `view` holds none, and the records of it that copy took: each record was verified as it was
/// decoded, and is taken as this Mac's store takes one (its signer seated in its causal past,
/// within the store's budgets). The push is refused whole at the first record the store refuses
/// for anything but room, and, once every record has been offered, when one the store kept
/// waiting for a parent still waits. A record the store refuses for room
/// ([`aterm_secrets::ErrorKind::MembershipCapacity`]) is left out, and so is every record after
/// it in its causal future. Where the store has no room to keep a record waiting (at its held
/// total, its bound of waiting records or its bound of wire bytes), a record that misses a parent
/// is refused for room too: a full Mac leaves it out, where a Mac with room keeps it waiting and
/// refuses the push if it still waits.
///
/// A record left out leaves the copy as it was (the store changes nothing when it refuses one),
/// so the copy is what a push of exactly the taken records, in their order, would build, and each taken record is held there, its parents among
/// this Mac's records and the taken ones: the overlay admits only a peer that this smaller push
/// would admit, and the peer could always have sent it. So a full Mac still judges a peer its own
/// log seats. Nothing is written: a push is stored only once its peer is authenticated, by the
/// membership rounds that follow.
fn overlay(endpoint: &Endpoint, view: &View, pushed: Vec<Record>) -> Result<(Log, Vec<Record>)> {
    let mut store = view
        .0
        .clone()
        .unwrap_or_else(|| Records::with_compiled(None));
    #[cfg(test)]
    let room_refuses = mutants::on(mutants::Mutant::CapacityRefusesPush);
    #[cfg(not(test))]
    let room_refuses = false;
    let mut unheld = BTreeSet::new();
    for record in &pushed {
        if store.log().is_none() {
            let Body::Genesis { root, .. } = record.body() else {
                return Err(Error::new("Commons log witness must start with Genesis"));
            };
            if root.network != endpoint.trust.network || root.authority != endpoint.trust.authority
            {
                return Err(Error::new("Commons peer belongs to another membership log"));
            }
        }
        if record
            .parents()
            .iter()
            .any(|parent| unheld.contains(parent))
        {
            unheld.insert(*record.id());
            continue;
        }
        match store.insert(record.clone()) {
            Ok(_) => {}
            Err(error)
                if error.kind() == aterm_secrets::ErrorKind::MembershipCapacity
                    && !room_refuses =>
            {
                unheld.insert(*record.id());
            }
            Err(_) => {
                return Err(Error::new(
                    "Commons evidence holds a record this Mac refuses",
                ));
            }
        }
    }
    let log = store
        .log()
        .ok_or_else(|| Error::new("Commons peer lacks historical log approval"))?;
    if pushed
        .iter()
        .any(|record| !unheld.contains(record.id()) && log.get(record.id()).is_none())
    {
        return Err(Error::new("Commons evidence is not causally complete"));
    }
    if endpoint
        .snapshot_log
        .as_ref()
        .is_some_and(|own| own.network() != log.network())
    {
        return Err(Error::new("Commons peer belongs to another membership log"));
    }
    let taken = pushed
        .into_iter()
        .filter(|record| !unheld.contains(record.id()))
        .collect();
    Ok((log.clone(), taken))
}

fn verify_evidence(endpoint: &Endpoint, peer: &Metadata, evidence: &Received) -> Result<()> {
    verify_peer_against_local_log(
        endpoint.custody(),
        endpoint.trust,
        &peer.request,
        evidence.judged(),
    )
}

pub(super) fn connected(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    proof: Option<&NodeProof>,
    side: Side,
    repair: &mut Repair,
    until: Instant,
    wire: Wire,
) -> Result<Report> {
    repair.begin_round();
    repair.phase = Phase::Membership;
    let own = Metadata::own(endpoint)?;
    let remote = match side {
        Side::Initiator => {
            send_frame(stream, &own.encode(endpoint.trust))?;
            repair.receive_bounded_answer(stream, side, META_LEN)?
        }
        Side::Responder => {
            let remote = receive_frame(stream, META_LEN)?;
            send_frame(stream, &own.encode(endpoint.trust))?;
            remote
        }
    };
    let mut peer = Metadata::decode(endpoint.trust, &remote)?;
    let actor = authenticate_identity(endpoint.actor, proof, &peer.request)?;
    compatible(endpoint, peer.bound)?;
    // Only `atdb/5` states what each side holds; an `atdb/4` peer sends its evidence next.
    let (view, have) = match wire {
        Wire::Whole => (View::NONE, Vec::new()),
        Wire::Delta => {
            let view = View::of(endpoint)?;
            let have = trade_haves(endpoint, stream, side, repair, &mut peer, &view)?;
            (view, have)
        }
    };
    let session = Session {
        wire,
        view: &view,
        have: &have,
    };
    if own.flags & PENDING != 0 || peer.flags & PENDING != 0 {
        return enroll(endpoint, stream, side, repair, &own, &mut peer, &session);
    }
    let evidence = own_evidence(endpoint, &own.request)?;
    let peer_request = peer.request.clone();
    let (theirs, mut lease) = match side {
        Side::Initiator => {
            send_evidence(
                endpoint,
                stream,
                peer.bound,
                &evidence,
                &own.request,
                &session,
            )?;
            let theirs = receive_evidence(
                endpoint,
                stream,
                &mut peer,
                &peer_request,
                Some(repair),
                &session,
                wire.evidence_bytes(),
            )?;
            verify_evidence(endpoint, &peer, &theirs)?;
            let lease = lease_after_evidence(endpoint, repair, actor, side, until)?;
            (theirs, lease)
        }
        Side::Responder => {
            let theirs = receive_evidence(
                endpoint,
                stream,
                &mut peer,
                &peer_request,
                None,
                &session,
                wire.evidence_bytes(),
            )?;
            verify_evidence(endpoint, &peer, &theirs)?;
            let lease = lease_after_evidence(endpoint, repair, actor, side, until)?;
            compatible(endpoint, peer.bound)?;
            verify_evidence(endpoint, &peer, &theirs)?;
            send_evidence(
                endpoint,
                stream,
                peer.bound,
                &evidence,
                &own.request,
                &session,
            )?;
            (theirs, lease)
        }
    };
    drop(view);
    compatible(endpoint, peer.bound)?;
    verify_evidence(endpoint, &peer, &theirs)?;
    let control = exchange(
        stream,
        &Disk {
            dir: &endpoint.dir,
            trust: endpoint.trust,
        },
        side,
        &mut lease,
    )?;
    if own.flags & peer.flags & RECORDS != 0 {
        lease.phase = Phase::MembershipRecords;
        match repair_records(
            endpoint,
            stream,
            side,
            &mut peer,
            &mut lease.membership_hint,
            until,
            wire,
        ) {
            Ok((report, ending)) => {
                lease.membership = Some(report);
                // Under `atdb/5`, records left for a later session (a full side, refused offers)
                // do not hold up table repair: both sides stopped at the same frame boundary.
                // An `atdb/4` session ends there, as 0.153's does.
                #[cfg(test)]
                let ending = if ending == Ending::Deferred
                    && mutants::on(mutants::Mutant::DeferredEndsSession)
                {
                    Ending::Interrupted
                } else {
                    ending
                };
                let stop = match ending {
                    Ending::Complete => None,
                    Ending::Deferred if wire == Wire::Delta => None,
                    Ending::Deferred => Some("Commons membership repair has more records"),
                    Ending::Interrupted => Some("Commons membership repair ran out of time"),
                };
                if let Some(stop) = stop {
                    lease.membership_error = Some(stop.to_owned());
                    return Ok(control);
                }
            }
            Err(error) => {
                lease.membership_error = Some(error.to_string());
                return Ok(control);
            }
        }
    }
    compatible(endpoint, peer.bound)?;
    verify_evidence(endpoint, &peer, &theirs)?;
    if own.flags & peer.flags & APPLICATION != 0 {
        endpoint.repair_application(stream, actor, side, &mut lease, Protocol::Frontier(wire));
        if lease.application_error.is_none() {
            lease.phase = Phase::VersionReport;
            lease.said = endpoint.trade_reports(stream, actor, side);
            if own.flags & peer.flags & CREDENTIALS != 0 {
                compatible(endpoint, peer.bound)?;
                verify_evidence(endpoint, &peer, &theirs)?;
                lease.phase = Phase::Credentials;
                if let (Some(paths), Some((owner, config))) =
                    (&endpoint.paths, &endpoint.credentials)
                {
                    lease.handoff = Some(
                        crate::credentials::handoff::exchange(
                            paths,
                            config,
                            owner,
                            endpoint.trust.network,
                            endpoint.actor,
                            actor,
                            side,
                            stream,
                        )
                        .map_err(|_| "credential handoff failed".to_owned()),
                    );
                }
            }
            lease.phase = Phase::Complete;
        }
    } else {
        lease.phase = Phase::ControlComplete;
    }
    Ok(control)
}

fn lease_after_evidence<'a>(
    endpoint: &'a Endpoint,
    repair: &'a mut Repair,
    actor: Actor,
    side: Side,
    until: Instant,
) -> Result<peers::Lease<'a>> {
    repair.prepare(endpoint.trust, actor);
    let authenticated = Instant::now();
    repair.authenticated = Some(authenticated);
    repair.phase = Phase::PeerAdmission;
    let preferred = (endpoint.actor < actor) == (side == Side::Initiator);
    let mut lease = endpoint.peers.acquire_on(actor, repair, preferred, until)?;
    #[cfg(test)]
    tests::peer_cache_stage(tests::PeerCacheStage::Acquired(side));
    lease.prepare(endpoint.trust, actor);
    lease.authenticated = Some(authenticated);
    lease.clear_application();
    Ok(lease)
}

fn load_records(endpoint: &Endpoint) -> Result<(Records, RecordsBound)> {
    let paths = endpoint
        .paths
        .as_ref()
        .ok_or_else(|| Error::new("Commons has no membership custody"))?;
    let store = client::load_records(paths).map_err(|e| local_failure("frontier records", e))?;
    let bound =
        client::records_bound(paths, &store).map_err(|e| local_failure("frontier bound", e))?;
    if let Some(log) = store.log()
        && let aterm_secrets::membership_log::Body::Genesis { root, .. } = log.genesis().body()
        && (root.network != endpoint.trust.network
            || root.authority != endpoint.trust.authority
            || endpoint
                .snapshot_log
                .as_ref()
                .is_some_and(|own| own.network() != log.network()))
    {
        return Err(Error::new("Commons membership custody changed its root"));
    }
    Ok((store, bound))
}

pub(super) fn encode_frontier(bound: RecordsBound, ids: &[Digest]) -> Vec<u8> {
    let mut wire = b"AFR4".to_vec();
    put_bound(&mut wire, bound);
    wire.extend_from_slice(&(ids.len() as u32).to_be_bytes());
    for id in ids {
        wire.extend_from_slice(id);
    }
    wire
}

fn knowledge(store: &Records) -> Vec<Digest> {
    let mut ids = store.frontier();
    let mut seen: BTreeSet<_> = ids.iter().copied().collect();
    // Every head remains represented. Bounded ancestry checkpoints let a peer
    // find shared history even when it has never seen one of those heads.
    for id in store.have() {
        if seen.insert(id) {
            ids.push(id);
        }
    }
    ids
}

fn decode_frontier(endpoint: &Endpoint, peer: &mut Metadata, wire: &[u8]) -> Result<Vec<Digest>> {
    if wire.len() < 13 || &wire[..4] != b"AFR4" {
        return Err(Error::new("invalid Commons frontier"));
    }
    let bound = read_bound(&wire[4..9])?;
    compatible(endpoint, bound)?;
    let count = u32::from_be_bytes(wire[9..13].try_into().map_err(failure)?) as usize;
    if count > bound.held as usize || wire.len() != 13 + count * 32 {
        return Err(Error::new("Commons frontier exceeds its bound"));
    }
    let ids: Vec<Digest> = wire[13..].as_chunks::<32>().0.to_vec();
    if ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
        return Err(Error::new("Commons frontier repeats an id"));
    }
    peer.bound = bound;
    Ok(ids)
}

fn send_batch(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    peer: RecordsBound,
    records: &[Record],
    wire: Wire,
) -> Result<Report> {
    let bound = compatible(endpoint, peer)?;
    let bytes: usize = records.iter().map(|record| record.bytes().len()).sum();
    let mut header = b"ABT4".to_vec();
    put_bound(&mut header, bound);
    header.extend_from_slice(&(records.len() as u32).to_be_bytes());
    header.extend_from_slice(&(bytes as u64).to_be_bytes());
    send_frame(stream, &header)?;
    for record in records {
        compatible(endpoint, peer)?;
        send_frame(stream, record.bytes())?;
    }
    let ack = receive_frame(stream, 8 + wire.batch_records())?;
    if ack.len() != 8 + records.len()
        || &ack[..4] != b"ACK4"
        || u32::from_be_bytes(ack[4..8].try_into().map_err(failure)?) as usize != records.len()
        || ack[8..].iter().any(|b| *b > 1)
    {
        return Err(Error::new("invalid Commons durable receipts"));
    }
    let sent = ack[8..].iter().filter(|b| **b == 1).count();
    Ok(Report {
        sent,
        received: 0,
        deferred: records.len() - sent,
    })
}

fn receive_batch(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    peer: &mut Metadata,
    wire: Wire,
) -> Result<(Report, usize)> {
    let header = receive_frame(stream, 21)?;
    if header.len() != 21 || &header[..4] != b"ABT4" {
        return Err(Error::new("invalid Commons batch"));
    }
    let bound = read_bound(&header[4..9])?;
    let local = compatible(endpoint, bound)?;
    peer.bound = bound;
    let count = u32::from_be_bytes(header[9..13].try_into().map_err(failure)?) as usize;
    let bytes = u64::from_be_bytes(header[13..21].try_into().map_err(failure)?);
    let most = if count == 1 {
        wire.batch_bytes().max(MAX_RECORD)
    } else {
        wire.batch_bytes()
    };
    if count > wire.batch_records()
        || count > bound.held as usize
        || count > local.held as usize
        || bytes > most as u64
    {
        return Err(Error::new("Commons batch exceeds its bound"));
    }
    let mut wires = Vec::with_capacity(count);
    let mut actual = 0u64;
    for _ in 0..count {
        compatible(endpoint, bound)?;
        let remaining = usize::try_from(bytes - actual).unwrap_or(usize::MAX);
        let wire = receive_frame(stream, MAX_RECORD.min(remaining))?;
        actual = actual
            .checked_add(wire.len() as u64)
            .filter(|n| *n <= bytes)
            .ok_or_else(|| Error::new("Commons batch exceeds its declared length"))?;
        wires.push(wire);
    }
    if actual != bytes {
        return Err(Error::new("Commons batch length differs"));
    }
    let paths = endpoint
        .paths
        .as_ref()
        .ok_or_else(|| Error::new("Commons has no membership custody"))?;
    let held = client::retain_bounded_records_for_commons(
        paths,
        endpoint.trust.network,
        endpoint.trust.authority,
        bound,
        &wires,
    )
    .map_err(|e| local_failure("frontier retain", e))?;
    let mut ack = b"ACK4".to_vec();
    ack.extend_from_slice(&(held.len() as u32).to_be_bytes());
    ack.extend(held.iter().map(|held| u8::from(*held)));
    send_frame(stream, &ack)?;
    let received = held.iter().filter(|held| **held).count();
    Ok((
        Report {
            sent: 0,
            received,
            deferred: count - received,
        },
        count,
    ))
}

fn repair_key(
    local: RecordsBound,
    remote: RecordsBound,
    ours: &BTreeSet<Digest>,
    theirs: &BTreeSet<Digest>,
) -> Digest {
    let mut hash = Sha256::new();
    hash.update(b"aterm/commons/frontier-refusal/4\0");
    for bound in [local, remote] {
        hash.update(bound.held.to_be_bytes());
        hash.update([u8::from(bound.crossed)]);
    }
    hash.update(hold_key(ours, theirs));
    hash.finalize()
}

/// Rotate through a stable causal list after refused offers. If acknowledged records leave
/// the list, their cursor disappears and the next batch starts with the earliest missing parent.
/// A batch holds at most [`Wire::batch_records`] records and [`Wire::batch_bytes`] bytes, but for
/// a first record larger than that, which goes alone (under `atdb/4` none is).
pub(super) fn select_batch(
    wire: Wire,
    missing: &[Record],
    cursor: &mut Option<Digest>,
) -> Vec<Record> {
    if missing.is_empty() {
        return Vec::new();
    }
    let start = cursor
        .and_then(|id| missing.iter().position(|record| *record.id() == id))
        .map_or(0, |at| (at + 1) % missing.len());
    let mut bytes = 0;
    let batch: Vec<_> = missing
        .iter()
        .cycle()
        .skip(start)
        .take(missing.len().min(wire.batch_records()))
        .enumerate()
        .take_while(|(n, record)| {
            bytes += record.bytes().len();
            *n == 0 || bytes <= wire.batch_bytes()
        })
        .map(|(_, record)| record.clone())
        .collect();
    if let Some(record) = batch.last() {
        *cursor = Some(*record.id());
    }
    batch
}

/// How a session's membership rounds ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// Each side holds what the other offered.
    Complete,
    /// Both sides stopped after the same round with records left (a round moved nothing, or the
    /// rounds ran out), at a frame boundary.
    Deferred,
    /// This side's deadline passed before a round, which the peer may have begun: nothing more is
    /// read or sent.
    Interrupted,
}

fn repair_records(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    side: Side,
    peer: &mut Metadata,
    hint: &mut MembershipHint,
    until: Instant,
    wire: Wire,
) -> Result<(Report, Ending)> {
    let mut total = Report::default();
    let capacity = own_bound(endpoint)?.held.max(peer.bound.held) as usize;
    for _ in 0..wire.round_cap(capacity) {
        if Instant::now() >= until {
            total.deferred = total.deferred.max(1);
            return Ok((total, Ending::Interrupted));
        }
        let (store, bound) = load_records(endpoint)?;
        let ids = knowledge(&store);
        let frontier = trade(
            stream,
            side,
            &encode_frontier(bound, &ids),
            13 + bound.held as usize * 32,
        )?;
        let theirs = decode_frontier(endpoint, peer, &frontier)?;
        let remote: BTreeSet<_> = theirs.iter().copied().collect();
        let key = repair_key(bound, peer.bound, &ids.iter().copied().collect(), &remote);
        let missing = store.missing(&theirs);
        let now = Instant::now();
        let offered = if hint.offers(&key, now) {
            select_batch(wire, &missing, &mut hint.cursor)
        } else {
            Vec::new()
        };
        let (sent, (received, incoming)) = match side {
            Side::Initiator => {
                let sent = send_batch(endpoint, stream, peer.bound, &offered, wire)?;
                (sent, receive_batch(endpoint, stream, peer, wire)?)
            }
            Side::Responder => {
                let got = receive_batch(endpoint, stream, peer, wire)?;
                (
                    send_batch(endpoint, stream, peer.bound, &offered, wire)?,
                    got,
                )
            }
        };
        total.sent += sent.sent;
        total.received += received.received;
        total.deferred = sent.deferred + received.deferred;
        hint.settle(
            key,
            &offered
                .iter()
                .map(|record| *record.id())
                .collect::<Vec<_>>(),
            sent.sent,
            remote.len() >= peer.bound.held as usize,
            now,
        );
        let complete =
            missing.is_empty() && incoming == 0 && knowledge(&load_records(endpoint)?.0) == ids;
        // Both sides agree whether another pass follows, even if a local writer moved one view.
        let again = trade(stream, side, &[u8::from(complete)], 1)?;
        if again.len() != 1 || again[0] > 1 {
            return Err(Error::new("invalid Commons frontier completion"));
        }
        if complete && again[0] == 1 {
            return Ok((total, Ending::Complete));
        }
        // The peer reads the same four counts (its sent is this side's received, its offer this
        // side's incoming), so both stop after this round together.
        if sent.sent + received.received == 0 && offered.is_empty() && incoming == 0 {
            break;
        }
    }
    total.deferred = total.deferred.max(1);
    Ok((total, Ending::Deferred))
}

/// `ACE1` enrollment over a frontier wire: the member pushes its seat evidence and then the new
/// seat's records. Under `atdb/5` each push carries only what the pending device's `have` lacks,
/// and the device judges both in overlays of its own records ([`View`]); under `atdb/4` each is
/// whole, judged from its genesis, as 0.153 enrolls. The device keeps the seat's records only once
/// the second gives it the seat.
fn enroll(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    side: Side,
    repair: &mut Repair,
    own: &Metadata,
    peer: &mut Metadata,
    session: &Session,
) -> Result<Report> {
    if endpoint.pending {
        if side != Side::Initiator || peer.flags & PENDING != 0 {
            return Err(Error::new(
                "pending Commons enrollment requires an established responder",
            ));
        }
        let paths = endpoint
            .paths
            .as_ref()
            .ok_or_else(|| Error::new("pending device has no custody"))?;
        let bootstrap = Bootstrap::cached_until(paths, Instant::now() + Duration::from_secs(5))
            .map_err(failure)?
            .ok_or_else(|| Error::new("pending device lost its root"))?;
        if bootstrap.trust() != endpoint.trust
            || bootstrap.request().map_err(failure)?.bytes() != own.request.bytes()
        {
            return Err(Error::new("pending device custody changed"));
        }
        let request = peer.request.clone();
        let evidence = receive_evidence(
            endpoint,
            stream,
            peer,
            &request,
            Some(repair),
            session,
            session.wire.evidence_bytes(),
        )?;
        verify_evidence(endpoint, peer, &evidence)?;
        let challenge = receive_frame(stream, MAX_CHALLENGE)?;
        let response = bootstrap
            .device()
            .answer(&own.request, &challenge)
            .map_err(failure)?;
        send_frame(stream, &response)?;
        // The member is authenticated now, so its grant may carry as many records as a records
        // stream does: a device that holds little of the log takes its whole seat here.
        let grant = receive_evidence(
            endpoint,
            stream,
            peer,
            &own.request,
            None,
            session,
            MAX_RECORDS_STREAM,
        )?;
        let received = match grant {
            Received::Witness(log) => retain_grant(endpoint, paths, peer.bound, log.records())?,
            Received::Push { pushed, .. } => retain_grant(endpoint, paths, peer.bound, &pushed)?,
            Received::Node(node) => {
                let mut journal =
                    Journal::open_existing(&endpoint.dir, endpoint.trust).map_err(failure)?;
                journal.ingest(&node).map_err(failure)?;
                1
            }
        };
        repair.prepare(endpoint.trust, peer.request.binding().actor());
        repair.authenticated = Some(Instant::now());
        Ok(Report {
            received,
            ..Report::default()
        })
    } else {
        if side != Side::Responder || peer.flags & PENDING == 0 {
            return Err(Error::new("invalid Commons enrollment direction"));
        }
        let paths = endpoint
            .paths
            .as_ref()
            .ok_or_else(|| Error::new("this peer does not serve enrollment"))?;
        check_local_bootstrap(endpoint, paths, own)?;
        // Enrollment may grow the source. The receiver must fit its full capacity before authoring.
        if peer.bound.held < own_bound(endpoint)?.held {
            return Err(Error::new(
                "Commons enrollment receiver has insufficient capacity",
            ));
        }
        let evidence = own_evidence(endpoint, &own.request)?;
        let log_backed = matches!(evidence, Evidence::Log(_));
        if log_backed {
            client::check_log_commons_enrollment(paths, endpoint.trust, peer.request.binding())
                .map_err(failure)?;
        } else {
            let (admin, bootstrap) = enrollment::local_authority(endpoint, paths)?;
            let journal = Journal::read_existing(&endpoint.dir, endpoint.trust).map_err(failure)?;
            admin
                .check_commons_enrollment(&bootstrap, &journal, peer.request.binding())
                .map_err(failure)?;
        }
        let actor = peer.request.binding().actor();
        let mut lease = endpoint.peers.acquire(actor, repair)?;
        lease.begin_round();
        lease.phase = Phase::Membership;
        send_evidence(
            endpoint,
            stream,
            peer.bound,
            &evidence,
            &own.request,
            session,
        )?;
        let (mut pending, challenge) = Pending::issue(
            &peer.request,
            endpoint.trust.network,
            endpoint.trust.authority,
        )
        .map_err(failure)?;
        send_frame(stream, &challenge)?;
        let response = receive_frame(stream, RESPONSE_LEN)?;
        let possession = pending.verify(&response).map_err(failure)?;
        compatible(endpoint, peer.bound)?;
        check_local_bootstrap(endpoint, paths, own)?;
        let sent = if log_backed {
            let chain = client::enroll_commons_member_in_log(
                paths,
                endpoint.trust,
                peer.request.clone(),
                possession,
            )
            .map_err(failure)?;
            let push = match session.wire {
                Wire::Whole => chain,
                Wire::Delta => lacked(endpoint, None, &chain, session.have),
            };
            let wires: Vec<_> = push.iter().map(Record::bytes).collect();
            send_evidence_wires(
                endpoint,
                stream,
                peer.bound,
                session.wire,
                true,
                &wires,
                MAX_RECORDS_STREAM,
            )?;
            push.len()
        } else {
            let (admin, bootstrap) = enrollment::local_authority(endpoint, paths)?;
            let node = {
                let mut journal =
                    Journal::open_existing(&endpoint.dir, endpoint.trust).map_err(failure)?;
                admin
                    .enroll_commons_member(&bootstrap, &mut journal, possession)
                    .map_err(failure)?
            };
            send_evidence_wires(
                endpoint,
                stream,
                peer.bound,
                session.wire,
                false,
                &[node.bytes()],
                MAX_NODE,
            )?;
            1
        };
        lease.prepare(endpoint.trust, actor);
        lease.authenticated = Some(Instant::now());
        Ok(Report {
            sent,
            ..Report::default()
        })
    }
}

/// Keep the records a member's grant gave this pending device, every one of them, from a source
/// stating `bound`: how many there were.
fn retain_grant<'a>(
    endpoint: &Endpoint,
    paths: &client::Paths,
    bound: RecordsBound,
    granted: impl IntoIterator<Item = &'a Record>,
) -> Result<usize> {
    let wires: Vec<_> = granted
        .into_iter()
        .map(|record| record.bytes().to_vec())
        .collect();
    let held = client::retain_bounded_records_for_commons(
        paths,
        endpoint.trust.network,
        endpoint.trust.authority,
        bound,
        &wires,
    )
    .map_err(failure)?;
    if held.iter().any(|held| !*held) {
        return Err(Error::new("pending device refused membership evidence"));
    }
    Ok(held.len())
}

fn check_local_bootstrap(endpoint: &Endpoint, paths: &client::Paths, own: &Metadata) -> Result<()> {
    let bootstrap = Bootstrap::cached_until(paths, Instant::now() + Duration::from_secs(5))
        .map_err(failure)?
        .ok_or_else(|| Error::new("enrollment peer lost its local root"))?;
    if bootstrap.trust() != endpoint.trust
        || bootstrap.request().map_err(failure)?.bytes() != own.request.bytes()
    {
        return Err(Error::new("enrollment peer custody changed"));
    }
    Ok(())
}
