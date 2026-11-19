// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fixed metadata, separately framed authority evidence and causal membership repair.
//! Membership evidence never grants table or credential permissions by itself.
//!
//! Five wires share these frames ([`Wire`]). Under `atdb/5` and `atdb/6`, once metadata names
//! each side's request, each states what it holds of the membership log ([`View`]). A peer then pushes only
//! the records of its seat evidence outside the past of what the other stated, at most
//! [`MAX_EVIDENCE`] bytes, and the other judges that push in a copy of its own records, writing
//! nothing before the peer is authenticated. Bulk repair follows in membership rounds bounded by
//! [`BATCH_RECORDS`] and [`BATCH_BYTES`]. Under `atdb/4` each side sends its whole seat witness,
//! as 0.153 does, so a 0.153 peer and this build still meet.
//!
//! **The records format** (`docs/DESIGN-vault-log-2026-11-14.md` step 3, tracker FL-148E; dark).
//! Every frame that states a records bound (`ACX4`, `AFR4`, `AEV4`, `ABT4`) states it in five
//! bytes, the held bound and the crossed state, which is format 1's: a Mac with no format mark
//! writes each frame byte for byte as every release since 0.153 does, and reads a peer's as
//! they do. A Mac whose format mark names format 2, on a build that reads it
//! ([`client::reads_later_format`]: none does while
//! [`aterm_secrets::commons::hearing::READS_FORMAT`] names no release), sets a bit of the
//! state byte ([`FORMATTED`]) and ends the frame with its byte capacity and its format
//! ([`FORMAT_TRAILER`]), over `atdb/5` and later. Every release before it refuses each such
//! frame, by its length or by the state byte, so it is sent only to a peer known to read it
//! ([`stated_to`]), and a Mac that states none reads none ([`reads_format`]). Two bounds
//! exchange only where their formats are equal ([`RecordsBound::can_exchange`]), which each
//! frame's reader asks before any record is offered, taken or acknowledged, so a peer of
//! another format is refused at its bound and leaves no record refused one by one
//! ([`super::MembershipHint`]). A Mac past format 1 on a build that does not read its mark's
//! format, and one on `atdb/4`, writes no frame at all ([`follows_format`], [`stated_to`]).
//!
//! Records are read at the format this Mac's bound states (`receive_evidence`,
//! `client::retain_bounded_records_for_commons`), and a record a root of this Mac's log dropped
//! ([`aterm_secrets::membership_log::rooted`]) reads as one it holds: a push that brings one is
//! not refused for it ([`overlay`]), and `atdb/8`'s reconciliation names it, so a peer that
//! holds the whole log offers none of them ([`held_and_known`]).
//!
//! **A served root** (tracker FL-148E5). On `atdb/8` and later, a Mac whose log is rooted serves
//! its roots ([`aterm_secrets::membership_log::Served`]) to a peer whose reconciled set lacks a
//! record a root dropped ([`serves`]): its first batch then holds every record the peer lacks,
//! its header sets [`SERVED`] in the state byte, and the served frame follows the header, as
//! the vault's pullers are served one reply (`Store::missing_served`). The peer takes the
//! records and the roots together or nothing (`client::retain_served_records_for_commons`,
//! which writes a rooted store as a rewritten records entry), so a peer behind a cut completes
//! its session where records alone would leave it waiting. A frame goes only where both Macs
//! state format 2 in this session: this Mac's bound (its mark, on a build that reads it,
//! [`follows_format`]) and the peer's, which it stated in its own frames over a channel that
//! proved its seat, and which only a build that reads format 2, and so this frame, writes
//! ([`stated_to`]). A Mac that states format 1 serves nothing and reads [`SERVED`] in no frame,
//! so every batch it writes is the released one. A refused frame takes nothing and leaves no
//! strike ([`MembershipHint::settle`] is not told of it); under the same views it is not served
//! again for [`REFUSAL_HOLD`] ([`MembershipHint::serves`]), and the peer is offered the records
//! it lacks alone meanwhile.

use super::*;
use aterm_secrets::{
    client,
    commons::{
        binding::{MAX_CHALLENGE, Pending, RESPONSE_LEN},
        bootstrap::Bootstrap,
    },
    crossing::{FORMAT_ONE, FORMAT_TWO, RecordsBound},
    membership_log::{Body, Log, MAX_RECORD, REPLY_CAP, Reading, Record, Served, Store as Records},
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

/// The most bytes of records a log these wires move holds: a store's capacity (the storage
/// class of byte bound, [`aterm_secrets::crossing::capacity`]), which is one number for every
/// format this build reads ([`Reading::STORE_BYTES`], which stops the build once it is not).
const LOG_BYTES: usize = Reading::STORE_BYTES;

/// What a bound of a later format than 1 adds to the end of the frame that states it: its byte
/// capacity (`u64`) and its format (`u32`), big-endian.
pub(super) const FORMAT_TRAILER: usize = 12;
/// The bit of a bound's state byte, beside its crossed bit, that says [`FORMAT_TRAILER`] ends
/// the frame. Every release before this one refuses a state byte above 1.
pub(super) const FORMATTED: u8 = 2;
/// The bit of a batch header's state byte, beside [`FORMATTED`], that says a served frame
/// follows the header ahead of its records: only in a header that states a later format than 1,
/// on `atdb/8` and later. Every reader of format 2 before this one refuses a state byte with it.
pub(super) const SERVED: u8 = 4;

/// The frontier wire a session speaks, as its ALPN named it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Wire {
    /// `atdb/4` ([`fleet::ALPN_DATABASE_FRONTIER`]), frame for frame as 0.153 released it, so a
    /// 0.153 peer and this build meet on it: each side sends the whole seat witness its bootstrap
    /// selected (never none), judged from that witness's genesis alone, at most one stream's
    /// bytes ([`REPLY_CAP`]); membership rounds move at most [`WHOLE_BATCH_RECORDS`]
    /// records or [`WHOLE_BATCH_BYTES`] bytes, and records left for a later session end this one
    /// before table repair.
    Whole,
    /// `atdb/5` ([`fleet::ALPN_DATABASE_DELTA`]): each side states what it holds before any
    /// evidence ([`View`]) and pushes only the seat evidence the other lacks, perhaps none, at
    /// most [`MAX_EVIDENCE`] bytes, judged in a copy of the receiver's records; membership rounds
    /// move at most [`BATCH_RECORDS`] records or [`BATCH_BYTES`] bytes, and records left for a
    /// later session hold up no table repair.
    Delta,
    /// `atdb/6` ([`fleet::ALPN_DATABASE_DUPLEX`]): the delta-frontier frames, then
    /// credential handoffs in both directions so the custodian may also be the listener.
    Duplex,
    /// `atdb/7` ([`fleet::ALPN_DATABASE_RECONCILE`]): [`Self::Duplex`]'s frames, with each table's
    /// operation inventory reconciled by range and its difference drained in one round, parents
    /// first ([`crate::replica`]).
    Reconcile,
    /// `atdb/8` ([`fleet::ALPN_DATABASE_RANGES`]): [`Self::Reconcile`]'s, with the control
    /// journal's inventory ([`super::exchange_ranges`]) and the membership log's
    /// ([`drain_records`]) reconciled by range as well, each one's whole difference moved in one
    /// session, parents first, in bounded batches.
    Ranges,
    /// `atdb/9` ([`fleet::ALPN_DATABASE_PROOFS`]): [`Self::Ranges`]'s, with each table's
    /// conflict-proof inventory reconciled by range as well ([`crate::replica`]).
    Proofs,
}

impl Wire {
    /// Whether this wire reconciles the control journal and the membership log by range
    /// (`atdb/8` and later).
    pub(super) const fn ranges(self) -> bool {
        matches!(self, Self::Ranges | Self::Proofs)
    }

    /// The most records one membership round pushes.
    pub(super) const fn batch_records(self) -> usize {
        match self {
            Self::Whole => WHOLE_BATCH_RECORDS,
            Self::Delta | Self::Duplex | Self::Reconcile | Self::Ranges | Self::Proofs => {
                BATCH_RECORDS
            }
        }
    }

    /// The most bytes one membership round pushes, but for a single record larger than it.
    pub(super) const fn batch_bytes(self) -> usize {
        match self {
            Self::Whole => WHOLE_BATCH_BYTES,
            Self::Delta | Self::Duplex | Self::Reconcile | Self::Ranges | Self::Proofs => {
                BATCH_BYTES
            }
        }
    }

    /// The most bytes of seat evidence a peer sends before it is authenticated.
    pub(super) const fn evidence_bytes(self) -> usize {
        match self {
            Self::Whole => REPLY_CAP,
            Self::Delta | Self::Duplex | Self::Reconcile | Self::Ranges | Self::Proofs => {
                MAX_EVIDENCE
            }
        }
    }

    /// The most membership rounds a session runs between peers that hold at most `capacity`
    /// records, which both sides count alike. Under `atdb/5`, every two rounds a side with
    /// records left sends a full batch of records or of bytes, or every record left, so the
    /// rounds a log of `capacity` records and a store's bytes ([`LOG_BYTES`]) needs, and two more to
    /// agree that nothing is left. Under `atdb/4`, 0.153's count: `capacity` over the number of
    /// largest records ([`MAX_RECORD`]) one batch's bytes hold, and two more.
    pub(super) const fn round_cap(self, capacity: usize) -> usize {
        match self {
            Self::Whole => capacity / (WHOLE_BATCH_BYTES / MAX_RECORD) + 2,
            Self::Delta | Self::Duplex | Self::Reconcile | Self::Ranges | Self::Proofs => {
                2 * (capacity.div_ceil(BATCH_RECORDS) + LOG_BYTES.div_ceil(BATCH_BYTES)) + 2
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

    pub(super) fn encode(&self, trust: Trust) -> [u8; META_LEN] {
        let mut wire = [0u8; META_LEN];
        wire[..4].copy_from_slice(b"ACX4");
        wire[4..20].copy_from_slice(&trust.network);
        wire[20..52].copy_from_slice(&trust.authority);
        wire[52..84].copy_from_slice(&trust.genesis);
        wire[84..HELLO_FIXED].copy_from_slice(self.request.bytes());
        wire[HELLO_FIXED..HELLO_FIXED + 4].copy_from_slice(&self.bound.held.to_be_bytes());
        wire[HELLO_FIXED + 4] = state(self.bound);
        wire[META_LEN - 1] = self.flags;
        wire
    }

    /// The frame that states this metadata: [`Self::encode`], and after it the trailer of a
    /// bound of a later format than 1 ([`put_trailer`]). At format 1 it is [`Self::encode`]'s
    /// bytes alone.
    pub(super) fn frame(&self, trust: Trust) -> Vec<u8> {
        let mut wire = self.encode(trust).to_vec();
        put_trailer(&mut wire, self.bound);
        wire
    }

    /// [`Self::decode_as`] as a Mac that states format 1 reads it: a frame of exactly
    /// [`META_LEN`] bytes whose bound is format 1's.
    #[cfg(test)]
    pub(super) fn decode(trust: Trust, wire: &[u8]) -> Result<Self> {
        Self::decode_as(trust, wire, false)
    }

    /// The metadata `wire` states, its bound of a later format than 1 read only where `reads`
    /// ([`reads_format`]).
    pub(super) fn decode_as(trust: Trust, wire: &[u8], reads: bool) -> Result<Self> {
        let (wire, trailer) = split_trailer(wire, HELLO_FIXED + 4, reads);
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
            bound: read_bound(&wire[HELLO_FIXED..HELLO_FIXED + 5], trailer)?,
            flags,
        })
    }
}

/// A bound's state byte: its crossed bit, and [`FORMATTED`] where it is of a later format than
/// 1. A format-1 bound's is 0 or 1, as every release writes it.
fn state(bound: RecordsBound) -> u8 {
    let formatted = if bound.format == FORMAT_ONE {
        0
    } else {
        FORMATTED
    };
    u8::from(bound.crossed) | formatted
}

fn put_bound(wire: &mut Vec<u8>, bound: RecordsBound) {
    wire.extend_from_slice(&bound.held.to_be_bytes());
    wire.push(state(bound));
}

/// End `wire`, a frame that states `bound`, with what a later format than 1 adds
/// ([`FORMAT_TRAILER`]): nothing at format 1.
fn put_trailer(wire: &mut Vec<u8>, bound: RecordsBound) {
    if bound.format != FORMAT_ONE {
        wire.extend_from_slice(&bound.bytes.to_be_bytes());
        wire.extend_from_slice(&bound.format.to_be_bytes());
    }
}

/// How many more bytes a frame that states a bound may hold where its reader reads a later
/// format than 1 (`reads`, [`reads_format`]): [`FORMAT_TRAILER`], and none otherwise, so a Mac
/// that states format 1 holds each frame to the length every release does.
const fn trailer_room(reads: bool) -> usize {
    if reads { FORMAT_TRAILER } else { 0 }
}

/// `wire` without the trailer of the bound whose state byte is at `at`, and that trailer:
/// only where this Mac reads one (`reads`) and the byte says one ends the frame
/// ([`FORMATTED`]). Otherwise all of `wire` and no trailer, which each frame's own checks then
/// judge as every release does: a state byte above 1 is no bound, and a longer frame is not
/// that frame.
fn split_trailer(wire: &[u8], at: usize, reads: bool) -> (&[u8], Option<&[u8]>) {
    match wire.get(at) {
        Some(byte) if reads && byte & FORMATTED != 0 && wire.len() > at + FORMAT_TRAILER => {
            let (frame, trailer) = wire.split_at(wire.len() - FORMAT_TRAILER);
            (frame, Some(trailer))
        }
        _ => (wire, None),
    }
}

/// The bound a frame states in the five bytes `wire` (the held bound and the state byte) and,
/// where it is of a later format than 1, in the `trailer` that ended its frame
/// ([`split_trailer`]). With no trailer it is format 1's, read as every release reads it.
fn read_bound(wire: &[u8], trailer: Option<&[u8]>) -> Result<RecordsBound> {
    let most = if trailer.is_some() { FORMATTED | 1 } else { 1 };
    if wire.len() != 5 || wire[4] > most {
        return Err(Error::new("invalid Commons records bound"));
    }
    let held = u32::from_be_bytes(wire[..4].try_into().map_err(failure)?);
    if held == 0 {
        return Err(Error::new("invalid Commons records capacity"));
    }
    let Some(trailer) = trailer else {
        return Ok(RecordsBound::of(held as usize, wire[4] != 0));
    };
    let (bytes, format) = trailer
        .split_at_checked(8)
        .ok_or_else(|| Error::new("invalid Commons records bound"))?;
    let bytes = u64::from_be_bytes(bytes.try_into().map_err(failure)?);
    let format = u32::from_be_bytes(format.try_into().map_err(failure)?);
    if bytes == 0 || format <= FORMAT_ONE {
        return Err(Error::new("invalid Commons records bound"));
    }
    Ok(RecordsBound {
        held,
        bytes,
        format,
        crossed: wire[4] & 1 != 0,
    })
}

fn own_bound(endpoint: &Endpoint) -> Result<RecordsBound> {
    let mut bound = endpoint.records_bound;
    if let Some(paths) = &endpoint.paths {
        bound.crossed |= client::records_marked_crossed(paths)
            .map_err(|error| local_failure("records protocol", error))?;
        // The custody's format mark, as its crossed mark: read on every path, not only one
        // that loads its records.
        bound = bound.at_format(
            client::records_marked_format(paths)
                .map_err(|error| local_failure("records protocol", error))?,
        );
        // MUTANT (`mutants::Mutant::StatesFormatUnmarked`): a build that reads format 2 states
        // it, mark or no mark.
        #[cfg(test)]
        if mutants::on(mutants::Mutant::StatesFormatUnmarked) && client::reads_later_format(paths) {
            bound = bound.at_format(FORMAT_TWO);
        }
    }
    follows_format(endpoint, bound)?;
    Ok(bound)
}

/// The gate of every frame a Mac past format 1 writes or reads here: a bound of a later format
/// than 1 is this Mac's only where its format mark names [`FORMAT_TWO`] and its build reads it
/// ([`client::reads_later_format`], which none does while
/// [`aterm_secrets::commons::hearing::READS_FORMAT`] names no release). A log past format 1 on
/// any other build speaks no frontier protocol, rather than state format 1 to a peer that
/// would take its records for that. A format-1 bound passes unasked.
fn follows_format(endpoint: &Endpoint, bound: RecordsBound) -> Result<()> {
    if bound.format == FORMAT_ONE
        || (bound.format == FORMAT_TWO
            && endpoint
                .paths
                .as_ref()
                .is_some_and(client::reads_later_format))
    {
        return Ok(());
    }
    Err(Error::new(
        "a membership log past records format 1 speaks no frontier protocol on a build that does not read its format",
    ))
}

/// Whether this Mac reads a bound of a later format than 1 in a peer's frame: only where it
/// states one itself ([`own_bound`]), so a Mac with no format mark reads each frame as every
/// release does, and refuses one that states a later format as it refuses any frame that is
/// not that frame.
fn reads_format(endpoint: &Endpoint) -> Result<bool> {
    // MUTANT (`mutants::Mutant::ReadsFormatUngated`): every Mac reads one.
    #[cfg(test)]
    if mutants::on(mutants::Mutant::ReadsFormatUngated) {
        return Ok(true);
    }
    Ok(own_bound(endpoint)?.format != FORMAT_ONE)
}

/// The seat a channel proved ([`super::authenticate_identity`] holds a peer's request to it):
/// its writer key and incarnation.
fn proven(proof: &NodeProof) -> Option<Actor> {
    let node = crate::database::decode_hex::<16>(&proof.node).ok()?;
    let mut actor = [0; 48];
    actor[..32].copy_from_slice(proof.key.bytes());
    actor[32..].copy_from_slice(&node);
    Some(actor)
}

/// Whether the peer the channel proved reads a bound of a later format than 1, by signed
/// evidence this Mac holds of that seat: the time-signed version report it keeps of it
/// ([`hearing::Reports::reported`], verified under the seat's binding) names
/// [`Endpoint::format_floor`]'s release or later; or this Mac's own records hold, complete, a
/// version-2 record that seat signed, which only a build that writes format 2 signs.
fn peer_reads(endpoint: &Endpoint, proof: Option<&NodeProof>) -> bool {
    let Some(actor) = proof
        .and_then(proven)
        .filter(|actor| *actor != endpoint.actor)
    else {
        return false;
    };
    let reported = endpoint.format_floor.is_some_and(|floor| {
        endpoint.reports.as_deref().is_some_and(|reports| {
            reports
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .reported(&actor)
                .is_some_and(|version| version >= floor)
        })
    });
    reported
        || View::of(endpoint).is_ok_and(|view| {
            view.0.as_ref().and_then(Records::log).is_some_and(|log| {
                log.records()
                    .any(|record| record.version() == 2 && *record.actor() == actor)
            })
        })
}

/// Whether a Mac that states a later format than 1 writes its first frame of a session on
/// `wire` to the peer the channel proved: on `atdb/5` and later, and only to a peer known to
/// read it.
///
/// A dialling Mac (`first` is none) knows by signed evidence alone ([`peer_reads`]).
///
/// An answering Mac knows by `first`, the peer's own first frame: it reads as metadata that
/// states this Mac's own format and a capacity it exchanges with ([`compatible`]), for the
/// seat whose key the channel proved ([`super::authenticate_identity`]). The peer wrote the
/// form this Mac answers in, and the answer states this Mac's bound, as an answer states one
/// to any peer; no record moves, and nothing is stored, until the peer's signed evidence
/// gives it its seat. A peer that states format 1 is not answered: every frame after its
/// metadata would be one it refuses.
///
/// # Errors
/// The peer is not known to read it, or the wire is `atdb/4`: this Mac then writes nothing.
fn stated_to(
    endpoint: &Endpoint,
    wire: Wire,
    proof: Option<&NodeProof>,
    first: Option<&[u8]>,
) -> Result<()> {
    // MUTANT (`mutants::Mutant::StatesFormatBlind`): to any peer, on any wire.
    #[cfg(test)]
    if mutants::on(mutants::Mutant::StatesFormatBlind) {
        return Ok(());
    }
    if wire == Wire::Whole {
        return Err(Error::new(
            "a membership log past records format 1 speaks no atdb/4 frontier protocol",
        ));
    }
    match first {
        Some(frame) => {
            let peer = Metadata::decode_as(endpoint.trust, frame, true)?;
            authenticate_identity(endpoint.actor, proof, &peer.request)?;
            compatible(endpoint, peer.bound)?;
            Ok(())
        }
        None if peer_reads(endpoint, proof) => Ok(()),
        None => Err(Error::new(
            "a membership log past records format 1 dials only a peer known to read its format",
        )),
    }
}

fn compatible(endpoint: &Endpoint, remote: RecordsBound) -> Result<RecordsBound> {
    let local = own_bound(endpoint)?;
    // MUTANT (`mutants::Mutant::FormatUnjudgedAtBound`): two bounds that differ only in their
    // format exchange.
    #[cfg(test)]
    let remote = if mutants::on(mutants::Mutant::FormatUnjudgedAtBound) {
        RecordsBound {
            format: local.format,
            bytes: local.bytes,
            ..remote
        }
    } else {
        remote
    };
    if !local.can_exchange(remote) {
        return Err(Error::new(
            "Commons frontier record capacities are incompatible",
        ));
    }
    Ok(local)
}

/// Before a frame after the metadata states `bound`, this Mac's own: a bound of a later format
/// than 1 is stated only to a peer whose last stated bound it exchanges with ([`compatible`]),
/// so a format mark stored since the metadata was traded ends the session before a frame
/// states it to a peer that would refuse the frame. A format-1 bound is stated as it always
/// was, and its reader judges it.
fn restated(endpoint: &Endpoint, bound: RecordsBound, peer: RecordsBound) -> Result<()> {
    if bound.format != FORMAT_ONE {
        compatible(endpoint, peer)?;
    }
    Ok(())
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

    /// The kernel decides what the read gives ([`aterm_db_core::peer_check::view`]).
    ///
    /// # Errors
    /// The records of an endpoint built from its membership log ([`super::Custody::Log`]) do
    /// not read, hold no log, or are another root's or lineage's: a push judged from its own
    /// genesis would pass a Mac those records remove.
    pub(super) fn of(endpoint: &Endpoint) -> Result<Self> {
        use aterm_db_core::peer_check::{Read, View as Held};
        let custody = endpoint.custody();
        let (outcome, held, why) = match custody {
            super::Custody::None => (Read::Absent, None, None),
            super::Custody::Log(paths) | super::Custody::Journal(paths) => {
                match super::held_records(&aterm_secrets::client::VaultLook::new(paths)) {
                    Err(error) => (
                        Read::Unread,
                        None,
                        Some(local_failure("records read for a peer check", error)),
                    ),
                    Ok(store) => match store.log() {
                        None => (Read::Absent, None, None),
                        Some(log)
                            if matches!(
                                log.genesis().body(),
                                Body::Genesis { root, .. }
                                    if root.network == endpoint.trust.network
                                        && root.authority == endpoint.trust.authority
                            ) && endpoint
                                .snapshot_log
                                .as_ref()
                                .is_none_or(|own| own.network() == log.network()) =>
                        {
                            (Read::Clear, Some(store), None)
                        }
                        Some(_) => (Read::Foreign, None, None),
                    },
                }
            }
        };
        match super::peer_check::view(custody.code(), outcome) {
            Held::Held => Ok(Self(held)),
            Held::None => Ok(Self(None)),
            Held::Refuse => Err(why.unwrap_or_else(|| {
                Error::new("this Mac's membership records cannot judge a Commons peer's push")
            })),
        }
    }

    pub(super) fn have(&self) -> Vec<Digest> {
        self.0.as_ref().map_or_else(Vec::new, knowledge)
    }

    /// A view that holds `store` (tests): what [`Self::of`] gives an endpoint whose records
    /// read as `store`.
    #[cfg(all(test, unix))]
    pub(super) const fn holding(store: Records) -> Self {
        Self(Some(store))
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
    restated(endpoint, bound, peer.bound)?;
    let own = encode_frontier(bound, &view.have());
    let max = 13 + bound.held as usize * 32 + trailer_room(reads_format(endpoint)?);
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
/// names records this side lacks is sent the rest. Each record is tested by a bit against
/// `held`'s past first ([`Log::past_of`]), and the custody is read, and its past tested, only for
/// the records that past leaves: a push to a peer that states every head of the witness reads
/// none. A set of every id in both pasts, built at each push of a 9,605-record witness, was 7.0%
/// of a sustained quiet round (2026-11-09).
fn lacked<'a>(
    endpoint: &Endpoint,
    held: Option<&Log>,
    records: impl IntoIterator<Item = &'a Record>,
    have: &[Digest],
) -> Vec<&'a Record> {
    let mut lacked: Vec<&Record> = records.into_iter().collect();
    if have.is_empty() {
        return lacked;
    }
    if let Some(log) = held {
        let past = log.past_of(have);
        lacked.retain(|record| !past.holds(record.id()));
    }
    if lacked.is_empty() {
        return lacked;
    }
    let custody = endpoint.paths.as_ref().and_then(|paths| {
        #[cfg(test)]
        CUSTODY_READS.set(CUSTODY_READS.get() + 1);
        client::records_now(paths).ok()
    });
    if let Some(log) = custody.as_ref().and_then(Records::log) {
        let past = log.past_of(have);
        lacked.retain(|record| !past.holds(record.id()));
    }
    lacked
}

#[cfg(test)]
thread_local! {
    /// How many pushes on this thread asked for their endpoint's records custody ([`lacked`]).
    pub(super) static CUSTODY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
    let wires: Vec<Frame<'_>> = match evidence {
        Evidence::Log(log) => {
            // Bootstrap already selected the evidence needed for the seat,
            // restored node keys and audience access. Taking only the seat's
            // ancestors again can discard those later proofs.
            admit_by_log_witness(log, target.binding())?;
            match wire {
                Wire::Whole => log.records().map(Frame::Record).collect(),
                Wire::Delta | Wire::Duplex | Wire::Reconcile | Wire::Ranges | Wire::Proofs => {
                    lacked(endpoint, Some(log), log.records(), session.have)
                        .into_iter()
                        .map(Frame::Record)
                        .collect()
                }
            }
        }
        Evidence::Node(node) => vec![Frame::Bytes(node)],
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
/// One frame of evidence: a record of this Mac's log, whose bytes are read at its send
/// ([`Record::wire_bytes`]: a replaced body of a log read through the vault's head is read back
/// for that frame alone, and one that does not read back fails the send), or a journal node.
#[derive(Clone, Copy)]
enum Frame<'a> {
    Record(&'a Record),
    Bytes(&'a [u8]),
}

impl Frame<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Record(record) => record.wire_len(),
            Self::Bytes(bytes) => bytes.len(),
        }
    }
}

fn send_evidence_wires(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    remote: RecordsBound,
    wire: Wire,
    log: bool,
    wires: &[Frame<'_>],
    max_bytes: usize,
) -> Result<()> {
    let bound = compatible(endpoint, remote)?;
    let bytes = wires
        .iter()
        .try_fold(0usize, |n, frame| n.checked_add(frame.len()))
        .ok_or_else(|| Error::new("Commons evidence length overflow"))?;
    let counted = if log {
        wire != Wire::Whole || !wires.is_empty()
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
    let mut header = vec![0u8; EVIDENCE_HEADER];
    header[..4].copy_from_slice(b"AEV4");
    header[4..8].copy_from_slice(&bound.held.to_be_bytes());
    header[8] = state(bound);
    header[9] = u8::from(log);
    header[10..14].copy_from_slice(&(wires.len() as u32).to_be_bytes());
    header[14..22].copy_from_slice(&(bytes as u64).to_be_bytes());
    put_trailer(&mut header, bound);
    send_frame(stream, &header)?;
    for frame in wires {
        compatible(endpoint, remote)?;
        match frame {
            Frame::Record(record) => send_frame(stream, &record.wire_bytes()?)?,
            Frame::Bytes(bytes) => send_frame(stream, bytes)?,
        }
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
    peer_bound: &mut RecordsBound,
    target: &Request,
    answer: Option<&mut Repair>,
    session: &Session,
    max_bytes: usize,
) -> Result<Received> {
    let wire = session.wire;
    let reads = reads_format(endpoint)?;
    let most = EVIDENCE_HEADER + trailer_room(reads);
    let header = match answer {
        Some(repair) => repair.receive_bounded_answer(stream, Side::Initiator, most)?,
        None => receive_frame(stream, most)?,
    };
    let (header, trailer) = split_trailer(&header, 8, reads);
    if header.len() != EVIDENCE_HEADER || &header[..4] != b"AEV4" || header[9] > 1 {
        return Err(Error::new("invalid Commons evidence header"));
    }
    let bound = read_bound(&header[4..9], trailer)?;
    let local = compatible(endpoint, bound)?;
    *peer_bound = bound;
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
        // Read as this Mac's store reads its own records: at the format its bound states.
        records.push(Record::decode_in(&frame, Reading::Format(local.format)).map_err(failure)?);
    }
    if actual != bytes {
        return Err(Error::new("Commons evidence length differs"));
    }
    #[cfg(test)]
    if mutants::on(mutants::Mutant::StoresPushFirst)
        && let Some(paths) = &endpoint.paths
    {
        let wires: Vec<_> = records
            .iter()
            .map(|r| r.wire_bytes().map(std::borrow::Cow::into_owned))
            .collect::<std::io::Result<_>>()?;
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
        Wire::Delta | Wire::Duplex | Wire::Reconcile | Wire::Ranges | Wire::Proofs => {
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
///
/// A record a root of this Mac's log dropped ([`Log::knows`]) is one it holds: the store takes
/// it as known and changes nothing, and the push is complete with it, as it is with any record
/// this Mac held before the push. A peer that holds the whole log pushes such a record with
/// its seat evidence whenever this Mac's stated heads do not reach it.
pub(super) fn overlay(
    endpoint: &Endpoint,
    view: &View,
    pushed: Vec<Record>,
) -> Result<(Log, Vec<Record>)> {
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
    // MUTANT (`mutants::Mutant::DroppedUnheld`): a dropped record is not held.
    #[cfg(test)]
    let knows = |id: &Digest| {
        if mutants::on(mutants::Mutant::DroppedUnheld) {
            log.get(id).is_some()
        } else {
            log.knows(id)
        }
    };
    #[cfg(not(test))]
    let knows = |id: &Digest| log.knows(id);
    if pushed
        .iter()
        .any(|record| !unheld.contains(record.id()) && !knows(record.id()))
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

/// Check `peer` on its `evidence` ([`verify_peer_against_local_log`]): the pass the next grant
/// to it takes.
fn verify_evidence(endpoint: &Endpoint, peer: &Metadata, evidence: &Received) -> Result<Pass> {
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
    // A Mac that states format 1 writes and reads here what every release does. One that
    // states a later format writes nothing until the peer is known to read it ([`stated_to`]).
    let states = own.bound.format != FORMAT_ONE;
    let reads = reads_format(endpoint)?;
    let most = META_LEN + trailer_room(reads);
    let remote = match side {
        Side::Initiator => {
            if states {
                stated_to(endpoint, wire, proof, None)?;
            }
            send_frame(stream, &own.frame(endpoint.trust))?;
            repair.receive_bounded_answer(stream, side, most)?
        }
        Side::Responder => {
            let remote = receive_frame(stream, most)?;
            if states {
                stated_to(endpoint, wire, proof, Some(&remote))?;
            }
            send_frame(stream, &own.frame(endpoint.trust))?;
            remote
        }
    };
    let mut peer = Metadata::decode_as(endpoint.trust, &remote, reads)?;
    let actor = authenticate_identity(endpoint.actor, proof, &peer.request)?;
    compatible(endpoint, peer.bound)?;
    // Delta-frontier peers state what each side holds; an `atdb/4` peer sends its evidence next.
    let (view, have) = match wire {
        Wire::Whole => (View::NONE, Vec::new()),
        Wire::Delta | Wire::Duplex | Wire::Reconcile | Wire::Ranges | Wire::Proofs => {
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
                &mut peer.bound,
                &peer.request,
                Some(repair),
                &session,
                wire.evidence_bytes(),
            )?;
            let pass = verify_evidence(endpoint, &peer, &theirs)?;
            let lease = lease_after_evidence(pass, endpoint, repair, actor, side, until)?;
            (theirs, lease)
        }
        Side::Responder => {
            let theirs = receive_evidence(
                endpoint,
                stream,
                &mut peer.bound,
                &peer.request,
                None,
                &session,
                wire.evidence_bytes(),
            )?;
            let pass = verify_evidence(endpoint, &peer, &theirs)?;
            let lease = lease_after_evidence(pass, endpoint, repair, actor, side, until)?;
            compatible(endpoint, peer.bound)?;
            // The responder sends its own evidence only to a peer a check passed after its lease.
            verify_evidence(endpoint, &peer, &theirs)?.spend();
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
    let pass = verify_evidence(endpoint, &peer, &theirs)?;
    let control = exchange(
        pass,
        stream,
        &Disk::new(&endpoint.dir, endpoint.trust),
        side,
        &mut lease,
        wire.ranges().then_some(until),
    )?;
    if own.flags & peer.flags & RECORDS != 0 {
        lease.phase = Phase::MembershipRecords;
        let pass = verify_evidence(endpoint, &peer, &theirs)?;
        match repair_records(
            pass,
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
                let ending = if matches!(ending, Ending::Deferred | Ending::Exhausted)
                    && mutants::on(mutants::Mutant::DeferredEndsSession)
                {
                    Ending::Interrupted
                } else {
                    ending
                };
                // Running out of rounds or time spends a bounded budget; a round that moved
                // nothing spends none, and names no more specific category.
                let stop = match ending {
                    Ending::Complete => None,
                    Ending::Deferred | Ending::Exhausted if wire != Wire::Whole => None,
                    Ending::Deferred => Some((
                        aterm_types::network_state::RepairCause::Other,
                        "Commons membership repair moved nothing and has more records",
                    )),
                    Ending::Exhausted => Some((
                        aterm_types::network_state::RepairCause::Quota,
                        "Commons membership repair has more records",
                    )),
                    Ending::Interrupted => Some((
                        aterm_types::network_state::RepairCause::Quota,
                        "Commons membership repair ran out of time",
                    )),
                };
                if let Some((cause, stop)) = stop {
                    lease.membership_error = Some(super::Failure::new(cause, stop));
                    return Ok(control);
                }
            }
            Err(error) => {
                lease.membership_error = Some(super::Failure::of(&error));
                return Ok(control);
            }
        }
    }
    compatible(endpoint, peer.bound)?;
    let pass = verify_evidence(endpoint, &peer, &theirs)?;
    if own.flags & peer.flags & APPLICATION != 0 {
        endpoint.repair_application(
            pass,
            stream,
            actor,
            side,
            &mut lease,
            Protocol::Frontier(wire),
        );
        if lease.application_error.is_none() {
            lease.phase = Phase::VersionReport;
            lease.said = endpoint.trade_reports(stream, actor, side);
            if own.flags & peer.flags & CREDENTIALS != 0 {
                compatible(endpoint, peer.bound)?;
                let pass = verify_evidence(endpoint, &peer, &theirs)?;
                lease.phase = Phase::Credentials;
                (lease.handoff, lease.owing) = endpoint.hand_off_credentials(
                    pass,
                    actor,
                    side,
                    matches!(
                        wire,
                        Wire::Duplex | Wire::Reconcile | Wire::Ranges | Wire::Proofs
                    ),
                    stream,
                );
            }
            lease.phase = Phase::Complete;
            // The round is complete and stays so: what follows only trades kept version
            // reports, with a peer whose reported release trades them. A test counts and
            // faults what moves from here on.
            #[cfg(test)]
            let stream = &mut super::tail::Tail::of(stream);
            endpoint.trade_relayed(stream, actor, side, &mut lease, until);
        }
    } else {
        pass.waive();
        lease.phase = Phase::ControlComplete;
    }
    Ok(control)
}

/// The lease on `actor`'s channel once its evidence passed, the grant `pass` clears.
fn lease_after_evidence<'a>(
    pass: Pass,
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
    let mut lease = endpoint.lease_on(
        pass,
        actor,
        repair,
        side == Side::Initiator,
        preferred,
        until,
    )?;
    #[cfg(test)]
    tests::peer_cache_stage(tests::PeerCacheStage::Acquired(side));
    lease.prepare(endpoint.trust, actor);
    lease.authenticated = Some(authenticated);
    lease.clear_application();
    lease.heed();
    Ok(lease)
}

fn load_records(endpoint: &Endpoint) -> Result<(Records, RecordsBound)> {
    let paths = endpoint
        .paths
        .as_ref()
        .ok_or_else(|| Error::new("Commons has no membership custody"))?;
    let store = client::records_now(paths).map_err(|e| local_failure("frontier records", e))?;
    let bound =
        client::records_bound(paths, &store).map_err(|e| local_failure("frontier bound", e))?;
    follows_format(endpoint, bound)?;
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
    let mut wire = Vec::with_capacity(13 + ids.len() * 32);
    wire.extend_from_slice(b"AFR4");
    put_bound(&mut wire, bound);
    wire.extend_from_slice(&(ids.len() as u32).to_be_bytes());
    for id in ids {
        wire.extend_from_slice(id);
    }
    put_trailer(&mut wire, bound);
    wire
}

fn knowledge(store: &Records) -> Vec<Digest> {
    knowledge_and_set(store).0
}

fn knowledge_and_set(store: &Records) -> (Vec<Digest>, BTreeSet<Digest>) {
    let mut ids = store.frontier();
    let mut seen: BTreeSet<_> = ids.iter().copied().collect();
    // Every head remains represented. Bounded ancestry checkpoints let a peer
    // find shared history even when it has never seen one of those heads.
    for id in store.have() {
        if seen.insert(id) {
            ids.push(id);
        }
    }
    (ids, seen)
}

fn decode_frontier(endpoint: &Endpoint, peer: &mut Metadata, wire: &[u8]) -> Result<Vec<Digest>> {
    decode_frontier_and_set(endpoint, peer, wire).map(|(ids, _)| ids)
}

fn decode_frontier_and_set(
    endpoint: &Endpoint,
    peer: &mut Metadata,
    wire: &[u8],
) -> Result<(Vec<Digest>, BTreeSet<Digest>)> {
    let (wire, trailer) = split_trailer(wire, 8, reads_format(endpoint)?);
    if wire.len() < 13 || &wire[..4] != b"AFR4" {
        return Err(Error::new("invalid Commons frontier"));
    }
    let bound = read_bound(&wire[4..9], trailer)?;
    compatible(endpoint, bound)?;
    let count = u32::from_be_bytes(wire[9..13].try_into().map_err(failure)?) as usize;
    if count > bound.held as usize || wire.len() != 13 + count * 32 {
        return Err(Error::new("Commons frontier exceeds its bound"));
    }
    let mut ids = Vec::with_capacity(count);
    let mut seen = BTreeSet::new();
    for &id in wire[13..].as_chunks::<32>().0 {
        if !seen.insert(id) {
            return Err(Error::new("Commons frontier repeats an id"));
        }
        ids.push(id);
    }
    peer.bound = bound;
    Ok((ids, seen))
}

/// Send one batch of `records` and read the peer's durable receipts. With `served`, the batch is
/// a served one ([`serves`]): its header sets [`SERVED`] and the served frame follows it ahead
/// of the records, which may be more than a batch's bound, up to one reply's bytes.
fn send_batch(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    peer: RecordsBound,
    records: &[Record],
    wire: Wire,
    served: Option<&Served>,
) -> Result<Report> {
    let bound = compatible(endpoint, peer)?;
    if served.is_some() && (bound.format == FORMAT_ONE || !wire.ranges()) {
        return Err(Error::new(
            "a served root goes only in a batch that states a later records format, on atdb/8",
        ));
    }
    let bytes: usize = records.iter().map(Record::wire_len).sum();
    let mut header = vec![0u8; 21];
    header[..4].copy_from_slice(b"ABT4");
    header[4..8].copy_from_slice(&bound.held.to_be_bytes());
    header[8] = state(bound);
    // MUTANT (`mutants::Mutant::ServedBitUngated`): every batch header says a frame follows.
    #[cfg(test)]
    if mutants::on(mutants::Mutant::ServedBitUngated) {
        header[8] |= SERVED;
    }
    if served.is_some() {
        header[8] |= SERVED;
    }
    header[9..13].copy_from_slice(&(records.len() as u32).to_be_bytes());
    header[13..21].copy_from_slice(&(bytes as u64).to_be_bytes());
    put_trailer(&mut header, bound);
    send_frame(stream, &header)?;
    if let Some(served) = served {
        send_frame(stream, &served.encode())?;
    }
    for record in records {
        compatible(endpoint, peer)?;
        send_frame(stream, &record.wire_bytes()?)?;
    }
    let ack = receive_frame(stream, 8 + records.len().max(wire.batch_records()))?;
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
        added: 0,
    })
}

/// Take one batch of the peer's records, of at most `room` records: how many records it held
/// and whether it was a served one ([`SERVED`]), whose frame and records were taken together
/// or not at all. A served batch is read only where this Mac reads a later format than 1 (the
/// header's trailer) on `atdb/8` and later; every other reader judges the bit as no bound, as a
/// release does.
pub(super) fn receive_batch(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    peer: &mut Metadata,
    wire: Wire,
    room: usize,
) -> Result<(Report, usize, bool)> {
    let reads = reads_format(endpoint)?;
    let header = receive_frame(stream, 21 + trailer_room(reads))?;
    let (header, trailer) = split_trailer(&header, 8, reads);
    if header.len() != 21 || &header[..4] != b"ABT4" {
        return Err(Error::new("invalid Commons batch"));
    }
    let formatted = trailer.is_some();
    // MUTANT (`mutants::Mutant::ServedBitUnread`): the served bit is read in a header with no
    // format trailer.
    #[cfg(test)]
    let formatted = formatted || mutants::on(mutants::Mutant::ServedBitUnread);
    let served = formatted && wire.ranges() && header[8] & SERVED != 0;
    let mut stated = [0u8; 5];
    stated.copy_from_slice(&header[4..9]);
    if served {
        stated[4] &= !SERVED;
    }
    let bound = read_bound(&stated, trailer)?;
    let local = compatible(endpoint, bound)?;
    peer.bound = bound;
    let count = u32::from_be_bytes(header[9..13].try_into().map_err(failure)?) as usize;
    let bytes = u64::from_be_bytes(header[13..21].try_into().map_err(failure)?);
    let most = if served {
        REPLY_CAP
    } else if count == 1 {
        wire.batch_bytes().max(MAX_RECORD)
    } else {
        wire.batch_bytes()
    };
    if (!served && count > wire.batch_records())
        || count > room
        || count > bound.held as usize
        || count > local.held as usize
        || bytes > most as u64
    {
        return Err(Error::new("Commons batch exceeds its bound"));
    }
    let frame = if served {
        let room = REPLY_CAP - usize::try_from(bytes).map_err(failure)?;
        Some(receive_frame(stream, room)?)
    } else {
        None
    };
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
    let held = match &frame {
        Some(frame) => client::retain_served_records_for_commons(
            paths,
            endpoint.trust.network,
            endpoint.trust.authority,
            bound,
            frame,
            &wires,
        ),
        None => client::retain_bounded_records_for_commons(
            paths,
            endpoint.trust.network,
            endpoint.trust.authority,
            bound,
            &wires,
        ),
    }
    .map_err(|e| local_failure("frontier retain", e))?;
    let mut ack = Vec::with_capacity(8 + held.len());
    ack.extend_from_slice(b"ACK4");
    ack.extend_from_slice(&(held.len() as u32).to_be_bytes());
    ack.extend(held.iter().map(|held| u8::from(*held)));
    send_frame(stream, &ack)?;
    let received = held.iter().filter(|held| **held).count();
    Ok((
        Report {
            sent: 0,
            received,
            // What a served batch leaves is what this Mac still lacks, which its caller counts
            // again from its store.
            deferred: if served { 0 } else { count - received },
            // The records' store does not tell held from new ([`super::Report::added`]).
            added: 0,
        },
        count,
        served,
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
        hash.update([state(bound)]);
        // A later format's capacity and number, as its frames state them: none at format 1.
        let mut stated = Vec::new();
        put_trailer(&mut stated, bound);
        hash.update(&stated);
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
            bytes += record.wire_len();
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
    /// Both sides stopped after the same round with records left, at a frame boundary: that round
    /// moved nothing (offers held back after the peer refused them, or a local writer moved a
    /// view).
    Deferred,
    /// Both sides stopped with records left when the session's rounds ran out, at a frame
    /// boundary.
    Exhausted,
    /// This side's deadline passed before a round, which the peer may have begun: nothing more is
    /// read or sent.
    Interrupted,
}

/// Repair membership records with the peer in bounded rounds, the grant `pass` clears.
#[expect(
    clippy::too_many_arguments,
    reason = "a grant takes the pass its check minted by value, beside the session's seven"
)]
fn repair_records(
    pass: Pass,
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    side: Side,
    peer: &mut Metadata,
    hint: &mut MembershipHint,
    until: Instant,
    wire: Wire,
) -> Result<(Report, Ending)> {
    pass.spend();
    if wire.ranges() {
        return drain_records(endpoint, stream, side, peer, hint, until);
    }
    let mut total = Report::default();
    let capacity = own_bound(endpoint)?.held.max(peer.bound.held) as usize;
    for _ in 0..wire.round_cap(capacity) {
        if Instant::now() >= until {
            total.deferred = total.deferred.max(1);
            return Ok((total, Ending::Interrupted));
        }
        let (store, bound) = load_records(endpoint)?;
        restated(endpoint, bound, peer.bound)?;
        let (ids, ours) = knowledge_and_set(&store);
        let frontier = trade(
            stream,
            side,
            &encode_frontier(bound, &ids),
            13 + bound.held as usize * 32 + trailer_room(reads_format(endpoint)?),
        )?;
        let (theirs, remote) = decode_frontier_and_set(endpoint, peer, &frontier)?;
        let key = repair_key(bound, peer.bound, &ours, &remote);
        let missing = store.missing(&theirs);
        let now = Instant::now();
        let offered = if hint.offers(&key, now) {
            select_batch(wire, &missing, &mut hint.cursor)
        } else {
            Vec::new()
        };
        let (sent, (received, incoming, _)) = match side {
            Side::Initiator => {
                let sent = send_batch(endpoint, stream, peer.bound, &offered, wire, None)?;
                (
                    sent,
                    receive_batch(endpoint, stream, peer, wire, usize::MAX)?,
                )
            }
            Side::Responder => {
                let got = receive_batch(endpoint, stream, peer, wire, usize::MAX)?;
                (
                    send_batch(endpoint, stream, peer.bound, &offered, wire, None)?,
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
            total.deferred = total.deferred.max(1);
            return Ok((total, Ending::Deferred));
        }
    }
    total.deferred = total.deferred.max(1);
    Ok((total, Ending::Exhausted))
}

/// `atdb/8`'s membership repair: the two sides' causally complete records reconcile by range once
/// ([`super::reconcile`]; each signer's dense, unforked prefix agreed by its frontier, the rest by
/// range, [`snapshot`]), and every record either side lacks then moves in this one session,
/// parents first (the log's own order), in batches of at most [`BATCH_RECORDS`] records and
/// [`BATCH_BYTES`] bytes (a larger record alone): batch after batch each way, until both sides offer
/// an empty one in the same turn, which each side reads alike, or either side's `until` passes
/// ([`super::proceed`]), both at the same frame boundary. A peer offers at most the records
/// the reconciliation found it holds that this side lacks. Under the refusal hold
/// ([`MembershipHint::offers`]) this side offers nothing and still takes what the peer offers.
///
/// Where this side serves its roots ([`serves`]), its first batch is a served one: the frame and
/// every record the peer lacks, which the peer takes together or not at all; the peer then
/// counts what it still lacks from its store, the ids the roots dropped among them where it
/// took none. A served batch the peer took none of settles nothing ([`MembershipHint::settle`]):
/// the peer judged a frame, not records, and keeps no strike. Under the same views it is not
/// served the frame again for [`REFUSAL_HOLD`] ([`MembershipHint::serves`]), and those sessions
/// offer it the records it lacks alone, which it takes or refuses as any records.
fn drain_records(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    side: Side,
    peer: &mut Metadata,
    hint: &mut MembershipHint,
    until: Instant,
) -> Result<(Report, Ending)> {
    let wire = Wire::Ranges;
    let (store, bound) = load_records(endpoint)?;
    let (records, ours) = held_and_known(&store);
    let roots = served_roots(endpoint, &store, bound, peer.bound);
    drop(store);
    let most = most_known(bound, peer.bound);
    let theirs = super::reconcile(
        stream,
        side,
        snapshot(&records, ours.clone()),
        ours.len(),
        most,
    )?;
    let mut lacked = theirs.difference(&ours).count();
    let key = repair_key(bound, peer.bound, &ours, &theirs);
    let now = Instant::now();
    // FAULT (`super::faults::clock`): a test's Mac reads its holds later than the wall.
    #[cfg(test)]
    let now = super::faults::clock(now);
    let owed: Vec<Record> = records
        .into_iter()
        .filter(|record| !theirs.contains(record.id()))
        .collect();
    // Under the hold the peer is offered nothing, and what it lacks stays deferred.
    let offers = hint.offers(&key, now);
    let (missing, withheld) = if offers {
        (owed, 0)
    } else {
        (Vec::new(), owed.len())
    };
    // A served batch the peer took nothing of under these views holds its frame back.
    let again = hint.serves(&key, now);
    // MUTANT (`mutants::Mutant::ServesRefusedRoot`): a refused frame is served again at once.
    #[cfg(test)]
    let again = again || mutants::on(mutants::Mutant::ServesRefusedRoot);
    let mut serving = roots.filter(|served| offers && again && serves(served, &theirs, &missing));
    // Whether the peer took the served batch, once one went.
    let mut taken = None;
    let mut total = Report::default();
    let mut at = 0;
    let ending = loop {
        if !super::proceed(stream, side, until)? {
            total.deferred = total.deferred.max(1);
            break Ending::Interrupted;
        }
        let served = serving.take();
        #[cfg(test)]
        let mut served = served;
        let batch = if served.is_some() {
            &missing[..]
        } else {
            next_batch(wire, &missing[at..])
        };
        at += batch.len();
        // FAULT (`super::faults::served_as`): what a test's peer serves in place of this Mac's
        // frame and records.
        #[cfg(test)]
        let faulted = served
            .as_mut()
            .map(|served| super::faults::served_as(served, batch));
        #[cfg(test)]
        let batch = faulted.as_deref().unwrap_or(batch);
        let (sent, (received, offered, got_served)) = match side {
            Side::Initiator => {
                let sent = send_batch(endpoint, stream, peer.bound, batch, wire, served.as_ref())?;
                (sent, receive_batch(endpoint, stream, peer, wire, lacked)?)
            }
            Side::Responder => {
                let got = receive_batch(endpoint, stream, peer, wire, lacked)?;
                (
                    send_batch(endpoint, stream, peer.bound, batch, wire, served.as_ref())?,
                    got,
                )
            }
        };
        if served.is_some() {
            taken = Some(sent.sent > 0);
        }
        lacked = if got_served {
            theirs
                .difference(&held_and_known(&load_records(endpoint)?.0).1)
                .count()
        } else {
            lacked - offered
        };
        total.sent += sent.sent;
        total.received += received.received;
        total.deferred += sent.deferred + received.deferred;
        if batch.is_empty() && served.is_none() && offered == 0 && !got_served {
            // What either side held back (the peer's `lacked` is its own `withheld`).
            total.deferred += withheld + lacked;
            break if total.deferred == 0 {
                Ending::Complete
            } else {
                Ending::Deferred
            };
        }
    };
    if let Some(taken) = taken {
        hint.served(key, taken, now);
    }
    // Every record the peer lacked was offered once this session, unless it was cut short.
    let refused = taken == Some(false);
    // MUTANT (`mutants::Mutant::RefusedRootStrikes`): a refused served batch is a refused round.
    #[cfg(test)]
    let refused = refused && !mutants::on(mutants::Mutant::RefusedRootStrikes);
    if !refused {
        hint.settle(
            key,
            &missing[..at]
                .iter()
                .map(|record| *record.id())
                .collect::<Vec<_>>(),
            total.sent,
            at == missing.len(),
            now,
        );
    }
    Ok((total, ending))
}

/// The roots this Mac may serve its peer on this session's records wire: its log's
/// ([`Log::served`]), and only where both sides state format 2 (this Mac's `bound`, which its
/// mark and its build gave it, [`follows_format`], and the `peer`'s, which it stated in its own
/// frames this session) and this Mac's build reads it ([`client::reads_later_format`]). `None`
/// for a whole log, and for every Mac that states format 1, which asks nothing more of its
/// store.
fn served_roots(
    endpoint: &Endpoint,
    store: &Records,
    bound: RecordsBound,
    peer: RecordsBound,
) -> Option<Served> {
    if bound.format != FORMAT_TWO
        || peer.format != FORMAT_TWO
        || !endpoint
            .paths
            .as_ref()
            .is_some_and(client::reads_later_format)
    {
        return None;
    }
    // MUTANT (`mutants::Mutant::ServesNoRoot`): no root is served.
    #[cfg(test)]
    if mutants::on(mutants::Mutant::ServesNoRoot) {
        return None;
    }
    store.log()?.served()
}

/// Whether this Mac serves `served` to a peer whose reconciled set is `theirs`, beside the
/// records it lacks (`missing`): where the peer lacks a record a root dropped, which no record
/// alone brings it (a record past the cut that follows one waits on it), and where the frame
/// and the records fit one reply ([`REPLY_CAP`]), as a vault server serves them
/// (`Store::missing_served`). A peer that holds or dropped every such record (it holds the cut,
/// or a root that nests it) is sent records alone, and a reply past [`REPLY_CAP`] is sent as
/// records alone, which the peer holds waiting for a peer that holds the cut.
fn serves(served: &Served, theirs: &BTreeSet<Digest>, missing: &[Record]) -> bool {
    // MUTANT (`mutants::Mutant::ServesAtTheCut`): served whatever the peer holds.
    #[cfg(test)]
    let lacks = mutants::on(mutants::Mutant::ServesAtTheCut)
        || served.stubs.keys().any(|id| !theirs.contains(id));
    #[cfg(not(test))]
    let lacks = served.stubs.keys().any(|id| !theirs.contains(id));
    lacks
        && missing
            .iter()
            .try_fold(served.wire_len(), |n, record| {
                n.checked_add(record.wire_len())
            })
            .is_some_and(|bytes| bytes <= REPLY_CAP)
}

/// The records `store` holds complete, in causal order, and the ids it reconciles with a peer:
/// those records' and, of a log rooted at a final checkpoint, the id of each record a root
/// dropped ([`Records::complete_inventory`]). A dropped record is one this Mac has: a peer that
/// holds the whole log then finds this Mac lacks none of them and offers none, where a set of
/// the held ids alone would have it offer every one of them in every session, each to be
/// answered as known. This Mac offers a peer only records it holds, so a dropped id a peer
/// lacks is offered no record: the root that dropped it is served instead ([`serves`]). For a
/// whole log the ids are the records', as before.
pub(super) fn held_and_known(store: &Records) -> (Vec<Record>, BTreeSet<Digest>) {
    let records: Vec<Record> = store
        .log()
        .map_or_else(Vec::new, |log| log.records().cloned().collect());
    // MUTANT (`mutants::Mutant::DroppedUnheld`): the held records' ids alone.
    #[cfg(test)]
    if mutants::on(mutants::Mutant::DroppedUnheld) {
        let ids = records.iter().map(|record| *record.id()).collect();
        return (records, ids);
    }
    (records, store.complete_inventory())
}

/// The most ids a peer's reconciled set names: at most what both sides can hold, since a peer's
/// stated bound is its own claim. At format 1 that is the lesser held bound. A log of a later
/// format may be rooted, and names each record a root dropped beside those it holds; a dropped
/// record's stub counts against the store's bytes and holds its 32-byte id, so a store names at
/// most its capacity over 32 of them.
pub(super) fn most_known(local: RecordsBound, peer: RecordsBound) -> usize {
    let held = local.held.min(peer.held) as usize;
    if local.format == FORMAT_ONE {
        return held;
    }
    held.saturating_add(usize::try_from(local.bytes.min(peer.bytes) / 32).unwrap_or(usize::MAX))
}

/// The reconciliation snapshot of `ids`, the records `records` holds in causal order: each
/// signer's dense, unforked prefix agreed by its frontier ([`Snapshot::from_inventory`]), as a
/// table round's is, so two long logs that share most of their history trade their tails rather
/// than ranges over the whole log. A header the index refuses leaves its record to the ranges.
pub(super) fn snapshot(
    records: &[Record],
    ids: BTreeSet<Digest>,
) -> astream_wire::sync::rbsr::Snapshot {
    let mut index = astream_wire::sync::Index::new(records.len().max(1));
    for record in records {
        let _ = index.insert(record.sync_header());
    }
    astream_wire::sync::rbsr::Snapshot::from_inventory(ids, &index)
}

/// The next batch of `missing`: at most [`Wire::batch_records`] records and [`Wire::batch_bytes`]
/// bytes, but for a first record larger than that, which goes alone.
fn next_batch(wire: Wire, missing: &[Record]) -> &[Record] {
    let mut bytes = 0;
    let count = missing
        .iter()
        .take(wire.batch_records())
        .enumerate()
        .take_while(|(n, record)| {
            bytes += record.wire_len();
            *n == 0 || bytes <= wire.batch_bytes()
        })
        .count();
    &missing[..count]
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
        let evidence = receive_evidence(
            endpoint,
            stream,
            &mut peer.bound,
            &peer.request,
            Some(repair),
            session,
            session.wire.evidence_bytes(),
        )?;
        let pass = verify_evidence(endpoint, peer, &evidence)?;
        let challenge = receive_frame(stream, MAX_CHALLENGE)?;
        // Answering commits this device to the member its check passed.
        pass.spend();
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
            &mut peer.bound,
            &own.request,
            None,
            session,
            REPLY_CAP,
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
        let request = peer.request.clone();
        let check = || enrollment::check(endpoint, paths, &request, log_backed);
        let pass = check()?;
        let actor = request.binding().actor();
        let mut lease = endpoint.lease(pass, actor, repair)?;
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
        // The member authors the device's seat only on a fresh check: a `Remove` made durable
        // here during the challenge refuses it.
        let pass = check()?;
        pass.spend();
        let sent = if log_backed {
            let chain = client::enroll_commons_member_in_log(
                paths,
                endpoint.trust,
                peer.request.clone(),
                possession,
            )
            .map_err(failure)?;
            let wires: Vec<_> = match session.wire {
                Wire::Whole => chain.iter().map(Frame::Record).collect(),
                Wire::Delta | Wire::Duplex | Wire::Reconcile | Wire::Ranges | Wire::Proofs => {
                    lacked(endpoint, None, &chain, session.have)
                        .into_iter()
                        .map(Frame::Record)
                        .collect()
                }
            };
            send_evidence_wires(
                endpoint,
                stream,
                peer.bound,
                session.wire,
                true,
                &wires,
                REPLY_CAP,
            )?;
            wires.len()
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
                &[Frame::Bytes(node.bytes())],
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
        .map(|record| record.wire_bytes().map(std::borrow::Cow::into_owned))
        .collect::<std::io::Result<_>>()?;
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
