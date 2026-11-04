// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fixed metadata, separately framed authority evidence and causal membership repair.
//! Membership evidence never grants table or credential permissions by itself.

use super::*;
use aterm_secrets::{
    client,
    commons::{
        binding::{MAX_CHALLENGE, Pending, RESPONSE_LEN},
        bootstrap::Bootstrap,
    },
    crossing::RecordsBound,
    membership_log::{Log, MAX_RECORD, MAX_RECORDS_STREAM, Record, Store as Records},
};

pub(super) const ALPN: &[u8] = fleet::ALPN_DATABASE_FRONTIER;
const META_LEN: usize = HELLO_FIXED + 6;
const RECORDS: u8 = 1;
const APPLICATION: u8 = 2;
const CREDENTIALS: u8 = 4;
const PENDING: u8 = 8;
const FLAGS: u8 = RECORDS | APPLICATION | CREDENTIALS | PENDING;
const EVIDENCE_HEADER: usize = 22;
const BATCH_RECORDS: usize = 256;
const BATCH_BYTES: usize = 4 * 1024 * 1024;
const _: () = assert!(BATCH_BYTES >= MAX_RECORD);

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

pub(super) enum Evidence {
    Log(Arc<Log>),
    Node(Vec<u8>),
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

pub(super) fn send_evidence(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    remote: RecordsBound,
    evidence: &Evidence,
    target: &Request,
) -> Result<()> {
    let records = match evidence {
        Evidence::Log(log) => {
            // Bootstrap already selected the evidence needed for the seat,
            // restored node keys and audience access. Taking only the seat's
            // ancestors again can discard those later proofs.
            admit_by_log_witness(log, target.binding())?;
            log.records().cloned().collect()
        }
        Evidence::Node(_) => Vec::new(),
    };
    let wires: Vec<&[u8]> = match evidence {
        Evidence::Log(_) => records.iter().map(Record::bytes).collect(),
        Evidence::Node(wire) => vec![wire],
    };
    send_evidence_wires(
        endpoint,
        stream,
        remote,
        matches!(evidence, Evidence::Log(_)),
        &wires,
    )
}

fn send_evidence_wires(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    remote: RecordsBound,
    log: bool,
    wires: &[&[u8]],
) -> Result<()> {
    let bound = compatible(endpoint, remote)?;
    let bytes = wires
        .iter()
        .try_fold(0usize, |n, wire| n.checked_add(wire.len()))
        .ok_or_else(|| Error::new("Commons evidence length overflow"))?;
    if wires.is_empty() || wires.len() > bound.held as usize || bytes > MAX_RECORDS_STREAM {
        return Err(Error::new("Commons evidence exceeds its bound"));
    }
    let mut header = b"AEV4".to_vec();
    put_bound(&mut header, bound);
    header.push(u8::from(log));
    header.extend_from_slice(&(wires.len() as u32).to_be_bytes());
    header.extend_from_slice(&(bytes as u64).to_be_bytes());
    send_frame(stream, &header)?;
    for wire in wires {
        compatible(endpoint, remote)?;
        send_frame(stream, wire)?;
    }
    Ok(())
}

fn receive_evidence(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    peer: &mut Metadata,
    target: &Request,
    answer: Option<&mut Repair>,
) -> Result<Evidence> {
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
    if count == 0
        || count > bound.held as usize
        || count > local.held as usize
        || (!bound.crossed && count > aterm_secrets::membership_log::LEGACY_RECORDS)
        || bytes > MAX_RECORDS_STREAM as u64
        || (!is_log && (count != 1 || bytes > MAX_NODE as u64))
    {
        return Err(Error::new("Commons evidence exceeds its bound"));
    }
    let mut actual = 0u64;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        compatible(endpoint, bound)?;
        let remaining = usize::try_from(bytes - actual).unwrap_or(usize::MAX);
        let wire = receive_frame(
            stream,
            remaining.min(if is_log { MAX_RECORD } else { MAX_NODE }),
        )?;
        actual = actual
            .checked_add(wire.len() as u64)
            .filter(|n| *n <= bytes)
            .ok_or_else(|| Error::new("Commons evidence exceeds its declared length"))?;
        if !is_log {
            if actual != bytes {
                return Err(Error::new("Commons evidence length differs"));
            }
            check_witness(endpoint.trust, target, &wire)?;
            return Ok(Evidence::Node(wire));
        }
        records.push(Record::decode(&wire).map_err(failure)?);
    }
    if actual != bytes {
        return Err(Error::new("Commons evidence length differs"));
    }
    let log = Arc::new(log_witness_from_records(endpoint.trust, records)?);
    if endpoint
        .snapshot_log
        .as_ref()
        .is_some_and(|own| own.network() != log.network())
    {
        return Err(Error::new("Commons peer belongs to another membership log"));
    }
    admit_by_log_witness(&log, target.binding())?;
    Ok(Evidence::Log(log))
}

fn verify_evidence(endpoint: &Endpoint, peer: &Metadata, evidence: &Evidence) -> Result<()> {
    verify_peer_against_local_log(
        endpoint.paths.as_ref(),
        endpoint.trust,
        &peer.request,
        match evidence {
            Evidence::Log(log) => Some(log),
            Evidence::Node(_) => None,
        },
    )
}

pub(super) fn connected(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    proof: Option<&NodeProof>,
    side: Side,
    repair: &mut Repair,
    until: Instant,
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
    if own.flags & PENDING != 0 || peer.flags & PENDING != 0 {
        return enroll(endpoint, stream, side, repair, &own, &mut peer);
    }
    let evidence = own_evidence(endpoint, &own.request)?;
    let peer_request = peer.request.clone();
    let (theirs, mut lease) = match side {
        Side::Initiator => {
            send_evidence(endpoint, stream, peer.bound, &evidence, &own.request)?;
            let theirs =
                receive_evidence(endpoint, stream, &mut peer, &peer_request, Some(repair))?;
            verify_evidence(endpoint, &peer, &theirs)?;
            let lease = lease_after_evidence(endpoint, repair, actor, side, until)?;
            (theirs, lease)
        }
        Side::Responder => {
            let theirs = receive_evidence(endpoint, stream, &mut peer, &peer_request, None)?;
            verify_evidence(endpoint, &peer, &theirs)?;
            let lease = lease_after_evidence(endpoint, repair, actor, side, until)?;
            compatible(endpoint, peer.bound)?;
            verify_evidence(endpoint, &peer, &theirs)?;
            send_evidence(endpoint, stream, peer.bound, &evidence, &own.request)?;
            (theirs, lease)
        }
    };
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
        ) {
            Ok((report, complete)) => {
                lease.membership = Some(report);
                if !complete {
                    lease.membership_error =
                        Some("Commons membership repair has more records".to_owned());
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
        endpoint.repair_application(stream, actor, side, &mut lease, Protocol::Frontier);
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

fn encode_frontier(bound: RecordsBound, ids: &[Digest]) -> Vec<u8> {
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
    let ack = receive_frame(stream, 8 + BATCH_RECORDS)?;
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
    if count > BATCH_RECORDS
        || count > bound.held as usize
        || count > local.held as usize
        || bytes > BATCH_BYTES as u64
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
pub(super) fn select_batch(missing: &[Record], cursor: &mut Option<Digest>) -> Vec<Record> {
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
        .take(missing.len().min(BATCH_RECORDS))
        .take_while(|record| {
            bytes += record.bytes().len();
            bytes <= BATCH_BYTES
        })
        .cloned()
        .collect();
    if let Some(record) = batch.last() {
        *cursor = Some(*record.id());
    }
    batch
}

fn repair_records(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    side: Side,
    peer: &mut Metadata,
    hint: &mut MembershipHint,
    until: Instant,
) -> Result<(Report, bool)> {
    let mut total = Report::default();
    let capacity = own_bound(endpoint)?.held.max(peer.bound.held) as usize;
    let rounds = capacity / (BATCH_BYTES / MAX_RECORD).max(1) + 2;
    for _ in 0..rounds {
        if Instant::now() >= until {
            break;
        }
        let (store, bound) = load_records(endpoint)?;
        let ids = knowledge(&store);
        let wire = trade(
            stream,
            side,
            &encode_frontier(bound, &ids),
            13 + bound.held as usize * 32,
        )?;
        let theirs = decode_frontier(endpoint, peer, &wire)?;
        let remote: BTreeSet<_> = theirs.iter().copied().collect();
        let key = repair_key(bound, peer.bound, &ids.iter().copied().collect(), &remote);
        let missing = store.missing(&theirs);
        let now = Instant::now();
        let offered = if hint.offers(&key, now) {
            select_batch(&missing, &mut hint.cursor)
        } else {
            Vec::new()
        };
        let (sent, (received, incoming)) = match side {
            Side::Initiator => {
                let sent = send_batch(endpoint, stream, peer.bound, &offered)?;
                (sent, receive_batch(endpoint, stream, peer)?)
            }
            Side::Responder => {
                let got = receive_batch(endpoint, stream, peer)?;
                (send_batch(endpoint, stream, peer.bound, &offered)?, got)
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
            return Ok((total, true));
        }
        if sent.sent + received.received == 0 && offered.is_empty() && incoming == 0 {
            break;
        }
    }
    total.deferred = total.deferred.max(1);
    Ok((total, false))
}

fn enroll(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    side: Side,
    repair: &mut Repair,
    own: &Metadata,
    peer: &mut Metadata,
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
        let evidence = receive_evidence(endpoint, stream, peer, &request, Some(repair))?;
        verify_evidence(endpoint, peer, &evidence)?;
        let challenge = receive_frame(stream, MAX_CHALLENGE)?;
        let response = bootstrap
            .device()
            .answer(&own.request, &challenge)
            .map_err(failure)?;
        send_frame(stream, &response)?;
        let grant = receive_evidence(endpoint, stream, peer, &own.request, None)?;
        let received = match grant {
            Evidence::Log(log) => {
                let wires: Vec<_> = log
                    .records()
                    .map(|record| record.bytes().to_vec())
                    .collect();
                let held = client::retain_bounded_records_for_commons(
                    paths,
                    endpoint.trust.network,
                    endpoint.trust.authority,
                    peer.bound,
                    &wires,
                )
                .map_err(failure)?;
                if held.iter().any(|held| !*held) {
                    return Err(Error::new("pending device refused membership evidence"));
                }
                held.len()
            }
            Evidence::Node(wire) => {
                let mut journal =
                    Journal::open_existing(&endpoint.dir, endpoint.trust).map_err(failure)?;
                journal.ingest(&wire).map_err(failure)?;
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
        send_evidence(endpoint, stream, peer.bound, &evidence, &own.request)?;
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
            let wires: Vec<_> = chain.iter().map(Record::bytes).collect();
            send_evidence_wires(endpoint, stream, peer.bound, true, &wires)?;
            chain.len()
        } else {
            let (admin, bootstrap) = enrollment::local_authority(endpoint, paths)?;
            let node = {
                let mut journal =
                    Journal::open_existing(&endpoint.dir, endpoint.trust).map_err(failure)?;
                admin
                    .enroll_commons_member(&bootstrap, &mut journal, possession)
                    .map_err(failure)?
            };
            send_evidence_wires(endpoint, stream, peer.bound, false, &[node.bytes()])?;
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
