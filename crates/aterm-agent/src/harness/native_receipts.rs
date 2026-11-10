// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local, authenticated restart admission, possible execution and terminal history.
//! The native issuer reserves closing space before admission and records possible
//! execution before returning a grant. A [`noexec`] observation requires a sealed,
//! drained operation with no possible execution. Disk records never reconstruct
//! execution or succession authority. A successor claim only consumes a positive
//! NoExec receipt; its caller must retain and revalidate live native evidence.
//! Absence alone is unknown, never proof of nonexecution: the host's `unadmitted` answer
//! composes it with its own operation table and an expired same-epoch deadline
//! ([`contains_operation`]), and its `lapsed` answer with a prepare's custody stamp, a clock
//! read before the journal and an expired deadline or another boot ([`observe_closure`]).
//! Complete recipient histories additionally support same-boot closure when the host proves
//! each recorded process and group retired after reading the expired deadline.
//! Across epochs, [`reject_unadmitted`] instead commits permanent denial before absence can
//! release an unbound recipe.
//! The separate enrollment file detects a missing established journal; neither
//! it nor a signed log protects against rollback of the whole custody tree.

use super::native_act::{Binding, Closure, ImagePhase, NativePlan, Prepare, Recorded, Request};
use aterm_journal::{Committed, Limits, Log};
use aterm_secrets::{age::Identity, client::Paths};
use aterm_uds::private_file::{self, PrivateFileError};
use ring::hmac;
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[path = "native_receipts_account.rs"]
mod account;
#[path = "native_receipts_recipients.rs"]
mod recipients;
#[cfg(any(test, debug_assertions))]
#[path = "native_receipts_replay_bench.rs"]
mod replay_bench;
pub use account::{
    AccountObservation, AccountOutcome, AccountRelation, ClaimedState, claim_account_successor,
    fence_account_successor, observe_account_successor, recover_account_successor,
};
pub use recipients::{Recipient, RecipientHistory, observe_recipients};

const NAME: &str = "noexec";
const ENROLLMENT: &str = "native-noexec.enrolled";
const SUBJECT: &str = "native/noexec/1";
const BODY_BYTES: usize = 4 + 8 + 8 + 32 * 5;
const ISSUER_SUBJECT: &str = "native/issuer/2";
const ISSUER_BYTES: usize = BODY_BYTES + 1 + 32;
const PHASE_SUBJECT: &str = "native/issuer/4";
const SUCCESSOR_SUBJECT: &str = "native/successor/3";
const SUCCESSOR_BYTES: usize = ISSUER_BYTES + 32;
const ACCOUNT_SUBJECT: &str = "native/successor/4";
const FENCE_SUBJECT: &str = "native/account-fence/4";
const ACCOUNT_BYTES: usize = SUCCESSOR_BYTES + 1;
const ACCOUNT_FRAME: usize = ACCOUNT_BYTES + ACCOUNT_SUBJECT.len() + aterm_journal::APPEND_OVERHEAD;
const FENCE_FRAME: usize = ACCOUNT_BYTES + FENCE_SUBJECT.len() + aterm_journal::APPEND_OVERHEAD;
const ISSUER_FRAME: usize = ISSUER_BYTES + ISSUER_SUBJECT.len() + aterm_journal::APPEND_OVERHEAD;
const SUCCESSOR_FRAME: usize =
    SUCCESSOR_BYTES + SUCCESSOR_SUBJECT.len() + aterm_journal::APPEND_OVERHEAD;
const LIMITS: Limits = Limits {
    records: 32_768,
    bytes: 16 * 1024 * 1024,
    record_bytes: recipients::BODY_BYTES,
};

type Digest = [u8; 32];

struct Key {
    mac: hmac::Key,
    anchor: Vec<u8>,
}

impl Key {
    fn new(identity: &Identity) -> Self {
        let root = hmac::Key::new(hmac::HMAC_SHA256, identity.secret_bytes());
        let derived = hmac::sign(&root, b"aterm/native-noexec/local-auth/1");
        let mut anchor = b"aterm/native-noexec/1\0".to_vec();
        anchor.extend_from_slice(&identity.recipient().0);
        Self {
            mac: hmac::Key::new(hmac::HMAC_SHA256, derived.as_ref()),
            anchor,
        }
    }

    fn digest(&self, domain: &[u8], value: &[u8]) -> Digest {
        let mut context = hmac::Context::with_key(&self.mac);
        context.update(domain);
        context.update(value);
        context.sign().as_ref().try_into().expect("SHA-256 length")
    }

    fn operation(&self, id: &str) -> Digest {
        self.digest(b"operation\0", id.as_bytes())
    }

    fn prepare(&self, prepare: &Prepare) -> io::Result<Digest> {
        if !matches!(prepare.plan, NativePlan::Restart { .. }) {
            return Err(invalid());
        }
        let text = Request::prepare(prepare.clone())
            .encode()
            .ok_or_else(invalid)?;
        Ok(self.digest(b"prepare\0", text.as_bytes()))
    }

    fn capability(&self, binding: &Binding) -> Digest {
        self.digest(b"capability\0", binding.capability.as_bytes())
    }

    fn legacy_prepare(&self, prepare: &Prepare) -> Option<Digest> {
        let text = Request::prepare(prepare.clone()).encode_legacy_cold()?;
        Some(self.digest(b"prepare\0", text.as_bytes()))
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Receipt {
    operation: Digest,
    prepare: Digest,
    capability: Digest,
}

/// An issuer record describes possible execution, never permission reconstructed
/// from disk. Every nonterminal state keeps its closing capacity reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IssuerState {
    Prepared,
    MayExecute,
    NoExec,
    Sealed { may_execute: bool },
    Fenced,
    Rejected,
}

impl IssuerState {
    fn reserved(self) -> usize {
        match self {
            Self::Prepared => 2,
            Self::MayExecute => 1,
            Self::NoExec | Self::Sealed { .. } | Self::Fenced | Self::Rejected => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Prepare = 1,
    MayExecute = 2,
    NoExec = 3,
    Seal = 4,
    Reject = 5,
}

impl Event {
    fn parse(byte: u8) -> io::Result<Self> {
        match byte {
            1 => Ok(Self::Prepare),
            2 => Ok(Self::MayExecute),
            3 => Ok(Self::NoExec),
            4 => Ok(Self::Seal),
            5 => Ok(Self::Reject),
            _ => Err(invalid()),
        }
    }

    fn advance(self, previous: Option<IssuerState>) -> io::Result<IssuerState> {
        match (previous, self) {
            (None, Self::Prepare) => Ok(IssuerState::Prepared),
            (None, Self::Reject) => Ok(IssuerState::Rejected),
            (Some(IssuerState::Prepared), Self::MayExecute) => Ok(IssuerState::MayExecute),
            (Some(IssuerState::Prepared), Self::NoExec) => Ok(IssuerState::NoExec),
            (Some(IssuerState::Prepared), Self::Seal) => {
                Ok(IssuerState::Sealed { may_execute: false })
            }
            (Some(IssuerState::MayExecute), Self::Seal) => {
                Ok(IssuerState::Sealed { may_execute: true })
            }
            _ => Err(invalid()),
        }
    }

    fn already(self, state: IssuerState) -> bool {
        matches!(
            (self, state),
            (Self::Prepare, IssuerState::Prepared)
                | (Self::MayExecute, IssuerState::MayExecute)
                | (Self::NoExec, IssuerState::NoExec)
                | (Self::Seal, IssuerState::Sealed { .. } | IssuerState::NoExec)
                | (Self::Reject, IssuerState::Rejected)
        )
    }
}

struct Entry {
    receipt: Receipt,
    observed: Observed,
    epoch: Option<Digest>,
    state: IssuerState,
    predecessor: Option<Digest>,
    successor: Option<Digest>,
    closed_phase: Option<ImagePhase>,
    account: Option<account::Link>,
    recipients: Option<recipients::History>,
}

impl Entry {
    fn reservation(&self) -> (usize, usize) {
        let (records, frame) = self.recipients.as_ref().map_or_else(
            || (self.state.reserved(), ISSUER_FRAME),
            |history| (history.reserved(self.state), recipients::FRAME_BYTES),
        );
        (records, records * frame)
    }
}

/// A verified observation, deliberately without any reusable native capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observed {
    /// Zero-based position of this exact record in the authenticated journal.
    /// A retry retains the original position, independent of later appends.
    pub sequence: u64,
    /// Local wall-clock timestamp, for attribution, not ordering or authorization.
    pub recorded_ms: u64,
}

/// An exact local closure observation, without execution or succession authority.
/// The caller supplies the original preparation and binding on every lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhaseObserved {
    /// The phase explicitly committed by the original issuer.
    pub phase: ImagePhase,
    /// Position of the original NoExec record, never the child's claim position.
    pub sequence: u64,
    /// Local attribution time; it is not an ordering or authorization clock.
    pub recorded_ms: u64,
}

#[derive(Default)]
struct View {
    entries: BTreeMap<Digest, Entry>,
    head: Digest,
    /// The authenticated head after the first record: it names this journal instance, which
    /// the anchor alone does not (a journal enrolled again under the same identity, or a copy
    /// of that identity beside another journal, has the same anchor). `None` while empty.
    genesis: Option<Digest>,
    sequence: u64,
    committed_bytes: usize,
    reserved_records: usize,
    reserved_bytes: usize,
}

impl View {
    /// Only the named operation changes its reservation. Successor backlinks change no
    /// reservation, so every format can maintain the aggregate through this replacement.
    fn replace_entry(&mut self, entry: Entry) -> io::Result<()> {
        let operation = entry.receipt.operation;
        let before = self
            .entries
            .get(&operation)
            .map_or((0, 0), Entry::reservation);
        let after = entry.reservation();
        let records = self
            .reserved_records
            .checked_sub(before.0)
            .and_then(|n| n.checked_add(after.0))
            .ok_or_else(invalid)?;
        let bytes = self
            .reserved_bytes
            .checked_sub(before.1)
            .and_then(|n| n.checked_add(after.1))
            .ok_or_else(invalid)?;
        self.entries.insert(operation, entry);
        self.reserved_records = records;
        self.reserved_bytes = bytes;
        Ok(())
    }

    /// The `custody=` spelling of this journal instance: SHA-256 of the public anchor and the
    /// first record's authenticated head, which carries no secret. `None` while the journal
    /// holds no record, so an absence from it proves nothing.
    fn custody(&self, key: &Key) -> Option<String> {
        let genesis = self.genesis?;
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        digest.update(&key.anchor);
        digest.update(&genesis);
        Some(aterm_codec::hex::encode_lower(digest.finish().as_ref()))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NoExecReceipt",
            action = "Record",
            reason = "Prior issuers produced legacy receipts; current writes are bound to IssuerJournal"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NoExecReceipt",
            action = "Retry",
            reason = "Prior issuers produced legacy receipts; current writes are bound to IssuerJournal"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NoExecReceipt",
            action = "Conflict",
            reason = "Prior issuers produced legacy receipts; current writes are bound to IssuerJournal"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RecipientJournal",
            action = "Prepare",
            project = "aterm_agent::harness::native_receipts::recipients::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RecipientJournal",
            action = "Forward",
            project = "aterm_agent::harness::native_receipts::recipients::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RecipientJournal",
            action = "Rollback",
            project = "aterm_agent::harness::native_receipts::recipients::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RecipientJournal",
            action = "NoExec",
            project = "aterm_agent::harness::native_receipts::recipients::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RecipientJournal",
            action = "Seal",
            project = "aterm_agent::harness::native_receipts::recipients::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RecipientJournal",
            action = "Other",
            project = "aterm_agent::harness::native_receipts::recipients::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RecipientJournal",
            action = "Legacy",
            project = "aterm_agent::harness::native_receipts::recipients::tests::transition"
        )
    )]
    fn ingest(&mut self, key: &Key, body: &[u8]) -> io::Result<()> {
        if body.starts_with(b"ANG5") {
            recipients::ingest(self, key, body)?;
        } else if body.starts_with(b"ANS4") || body.starts_with(b"ANF4") {
            account::ingest(self, key, body)?;
        } else {
            self.ingest_issuer(key, body)?;
        }
        self.genesis.get_or_insert(self.head);
        #[cfg(test)]
        {
            // Check the incremental totals against the original definition on every
            // successful transition exercised by the journal conformance models.
            let total = self.entries.values().fold((0, 0), |sum, entry| {
                let reserve = entry.reservation();
                (sum.0 + reserve.0, sum.1 + reserve.1)
            });
            assert_eq!((self.reserved_records, self.reserved_bytes), total);
        }
        Ok(())
    }

    fn ingest_bounded(&mut self, key: &Key, body: &[u8], limits: Limits) -> io::Result<()> {
        self.ingest(key, body)?;
        if self.within_capacity(limits, 0) {
            Ok(())
        } else {
            Err(invalid())
        }
    }

    fn ingest_issuer(&mut self, key: &Key, body: &[u8]) -> io::Result<()> {
        let legacy = body.len() == BODY_BYTES && &body[..4] == b"ANE1";
        let issuer = body.len() == ISSUER_BYTES && &body[..4] == b"ANI2";
        let successor = body.len() == SUCCESSOR_BYTES && &body[..4] == b"ANI3";
        let phased = body.len() == ISSUER_BYTES && &body[..4] == b"ANI4";
        if !legacy && !issuer && !successor && !phased {
            return Err(invalid());
        }
        hmac::verify(&key.mac, &body[..body.len() - 32], &body[body.len() - 32..])
            .map_err(|_| invalid())?;
        let number = |at| u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
        let digest = |at| -> Digest { body[at..at + 32].try_into().unwrap() };
        if number(4) != self.sequence || digest(20) != self.head {
            return Err(invalid());
        }
        let receipt = Receipt {
            operation: digest(52),
            prepare: digest(84),
            capability: digest(116),
        };
        let old = self.entries.get(&receipt.operation);
        if old.is_some_and(|entry| entry.recipients.is_some()) {
            return Err(invalid());
        }
        let closed_phase = if phased {
            Some(match body[148] {
                5 => ImagePhase::Forward,
                6 => ImagePhase::Rollback,
                _ => return Err(invalid()),
            })
        } else {
            old.and_then(|entry| entry.closed_phase)
        };
        let predecessor = if successor {
            let predecessor = digest(181);
            let parent = self.entries.get(&predecessor).ok_or_else(invalid)?;
            if old.is_some()
                || Event::parse(body[148])? != Event::Prepare
                || parent.state != IssuerState::NoExec
                || parent.recipients.is_some()
                || parent.epoch != Some(digest(149))
                || parent.successor.is_some()
            {
                return Err(invalid());
            }
            Some(predecessor)
        } else {
            old.and_then(|entry| entry.predecessor)
        };
        let retained_successor = old.and_then(|entry| entry.successor);
        let account = old.and_then(|entry| entry.account);
        let (state, epoch) = if legacy {
            // An old reader rejects ANI2, so it cannot append legacy records
            // after opening an issuer history. Existing ANE1 receipts remain final.
            if old.is_some() {
                return Err(invalid());
            }
            (IssuerState::NoExec, None)
        } else {
            let epoch = digest(149);
            if old.is_some_and(|old| old.receipt != receipt || old.epoch != Some(epoch)) {
                return Err(invalid());
            }
            let event = if phased {
                Event::NoExec
            } else {
                Event::parse(body[148])?
            };
            if event == Event::NoExec
                && old.is_some_and(|old| {
                    !account::permits_noexec(old, if phased { closed_phase } else { None })
                })
            {
                return Err(invalid());
            }
            (event.advance(old.map(|old| old.state))?, Some(epoch))
        };
        let next_sequence = self.sequence.checked_add(1).ok_or_else(invalid)?;
        // Preserve the earlier conservative ANE1/ANI2 reservation size. New
        // lineage records cost more, but cannot retroactively enlarge old ones.
        let next_bytes = self
            .committed_bytes
            .checked_add(if successor {
                SUCCESSOR_FRAME
            } else {
                ISSUER_FRAME
            })
            .ok_or_else(invalid)?;
        let operation = receipt.operation;
        self.replace_entry(Entry {
            receipt,
            observed: Observed {
                sequence: number(4),
                recorded_ms: number(12),
            },
            state,
            epoch,
            predecessor,
            successor: retained_successor,
            closed_phase,
            account,
            recipients: None,
        })?;
        if successor {
            self.entries
                .get_mut(&predecessor.unwrap())
                .unwrap()
                .successor = Some(operation);
        }
        self.sequence = next_sequence;
        self.committed_bytes = next_bytes;
        self.head = key.digest(b"receipt\0", body);
        Ok(())
    }

    fn encode(&self, key: &Key, receipt: &Receipt, now_ms: u64) -> Vec<u8> {
        let mut body = b"ANE1".to_vec();
        body.extend_from_slice(&self.sequence.to_le_bytes());
        body.extend_from_slice(&now_ms.to_le_bytes());
        body.extend_from_slice(&self.head);
        body.extend_from_slice(&receipt.operation);
        body.extend_from_slice(&receipt.prepare);
        body.extend_from_slice(&receipt.capability);
        body.extend_from_slice(hmac::sign(&key.mac, &body).as_ref());
        body
    }

    fn encode_event(
        &self,
        key: &Key,
        receipt: &Receipt,
        epoch: Digest,
        event: Event,
        now_ms: u64,
    ) -> Vec<u8> {
        let mut body = self.encode(key, receipt, now_ms);
        body.truncate(BODY_BYTES - 32);
        body[..4].copy_from_slice(b"ANI2");
        body.push(event as u8);
        body.extend_from_slice(&epoch);
        body.extend_from_slice(hmac::sign(&key.mac, &body).as_ref());
        body
    }

    fn encode_successor(
        &self,
        key: &Key,
        receipt: &Receipt,
        epoch: Digest,
        predecessor: Digest,
        now_ms: u64,
    ) -> Vec<u8> {
        let mut body = self.encode_event(key, receipt, epoch, Event::Prepare, now_ms);
        body.truncate(ISSUER_BYTES - 32);
        body[..4].copy_from_slice(b"ANI3");
        body.extend_from_slice(&predecessor);
        body.extend_from_slice(hmac::sign(&key.mac, &body).as_ref());
        body
    }

    fn encode_phase(
        &self,
        key: &Key,
        receipt: &Receipt,
        epoch: Digest,
        phase: ImagePhase,
        now_ms: u64,
    ) -> Vec<u8> {
        let mut body = self.encode_event(key, receipt, epoch, Event::NoExec, now_ms);
        body.truncate(ISSUER_BYTES - 32);
        body[..4].copy_from_slice(b"ANI4");
        body[148] = match phase {
            ImagePhase::Forward => 5,
            ImagePhase::Rollback => 6,
        };
        body.extend_from_slice(hmac::sign(&key.mac, &body).as_ref());
        body
    }

    fn can_append(&self, event: Option<Event>, limits: Limits) -> bool {
        // Count every epoch. Process-local host epochs do not fence other live
        // processes using the same Paths. Nothing silently expires a reservation.
        self.within_capacity(
            limits,
            match event {
                Some(Event::Prepare) => 3,
                Some(Event::Reject) => 1,
                Some(_) => 0, // The existing reservation already covers this append.
                None => 1,    // A legacy NoExec has no earlier reservation.
            },
        )
    }

    fn within_capacity(&self, limits: Limits, added: usize) -> bool {
        let Some(bytes) = added.checked_mul(ISSUER_FRAME) else {
            return false;
        };
        self.within_capacity_extra(limits, added, bytes)
    }

    fn can_append_successor(&self, limits: Limits) -> bool {
        self.within_capacity_extra(limits, 3, SUCCESSOR_FRAME + 2 * ISSUER_FRAME)
    }

    fn within_capacity_extra(&self, limits: Limits, added: usize, added_bytes: usize) -> bool {
        let Ok(retained) = usize::try_from(self.sequence) else {
            return false;
        };
        let Some(total) = retained
            .checked_add(self.reserved_records)
            .and_then(|n| n.checked_add(added))
        else {
            return false;
        };
        total <= limits.records
            && self
                .reserved_bytes
                .checked_add(self.committed_bytes)
                .and_then(|bytes| bytes.checked_add(added_bytes))
                .is_some_and(|bytes| bytes <= limits.bytes)
    }
}

fn invalid() -> io::Error {
    io::Error::other("native no-exec receipt or custody is invalid")
}

fn directory(paths: &Paths) -> PathBuf {
    paths.dir.join("native-noexec")
}

fn enrolled(paths: &Paths, key: &Key) -> io::Result<bool> {
    match private_file::read_private(&paths.dir, ENROLLMENT, 128) {
        Ok(bytes) if bytes == key.anchor => Ok(true),
        Err(PrivateFileError::Missing) => Ok(false),
        _ => Err(invalid()),
    }
}

fn custody_present(paths: &Paths) -> io::Result<bool> {
    for path in [directory(paths), paths.dir.join(ENROLLMENT)] {
        match std::fs::symlink_metadata(path) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(invalid()),
        }
    }
    Ok(false)
}

fn replay(log: &Log, key: &Key, limits: Limits) -> io::Result<View> {
    let mut view = View::default();
    loop {
        let page = log.read_range(astream_wire::Offset(view.sequence), 256)?;
        if page.is_empty() {
            break;
        }
        for record in page {
            if !record_subject(&record.subject, &record.body) || record.seq.0 != view.sequence {
                return Err(invalid());
            }
            view.ingest_bounded(key, &record.body, limits)?;
        }
    }
    Ok(view)
}

fn record_subject(subject: &str, body: &[u8]) -> bool {
    (subject == SUBJECT && body.starts_with(b"ANE1"))
        || (subject == ISSUER_SUBJECT && body.starts_with(b"ANI2"))
        || (subject == SUCCESSOR_SUBJECT && body.starts_with(b"ANI3"))
        || (subject == PHASE_SUBJECT && body.starts_with(b"ANI4"))
        || (subject == ACCOUNT_SUBJECT && body.starts_with(b"ANS4"))
        || (subject == FENCE_SUBJECT && body.starts_with(b"ANF4"))
        || (subject == recipients::SUBJECT && body.starts_with(b"ANG5"))
}

fn open(paths: &Paths, key: &Key, limits: Limits) -> io::Result<(Log, View)> {
    open_with_enrollment(paths, key, limits, true)
}

fn open_existing(paths: &Paths, key: &Key, limits: Limits) -> io::Result<(Log, View)> {
    open_with_enrollment(paths, key, limits, false)
}

fn open_with_enrollment(
    paths: &Paths,
    key: &Key,
    limits: Limits,
    may_enroll: bool,
) -> io::Result<(Log, View)> {
    // Serialize enrollment as well as append across independent app processes.
    // This lock is acquired only on the control worker, never under native locks.
    let _enrollment = private_file::PrivateLock::take(
        &paths.dir,
        "native-noexec.lock",
        Duration::from_secs(1),
        "native receipt enrollment is busy",
    )?;
    let established = enrolled(paths, key)?;
    if !established && !may_enroll {
        return Err(invalid());
    }
    let dir = directory(paths);
    if !established {
        match std::fs::symlink_metadata(&dir) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            // An interrupted enrollment remains unavailable rather than silently
            // adopting or erasing an existing custody tree.
            _ => return Err(invalid()),
        }
    }
    if established {
        private_file::check_private_dir(&dir)?;
    } else {
        aterm_journal::create_private_dir_durable(&dir)?;
    }
    let mut log = Log::open_until(
        &dir,
        NAME,
        &key.anchor,
        established,
        limits,
        Instant::now() + Duration::from_secs(1),
    )?;
    let view = replay(&log, key, limits)?;
    log.finish_open()?;
    if !established {
        // This is separate from the log and checkpoint. No receipt is appended
        // or acknowledged before this directory barrier succeeds.
        aterm_journal::write_private_durable(&paths.dir, ENROLLMENT, &key.anchor)?;
    }
    Ok((log, view))
}

// Kept only for the legacy format's executable conformance and migration tests.
#[cfg(test)]
fn record_at(paths: &Paths, prepare: &Prepare, binding: &Binding) -> io::Result<Observed> {
    record_event_at(paths, prepare, binding, None)
}

fn write_key(paths: &Paths) -> io::Result<Key> {
    let identity = match aterm_secrets::client::read_machine_identity(paths)
        .map_err(|_| invalid())?
    {
        Some(identity) => identity,
        None => {
            if !matches!(std::fs::symlink_metadata(directory(paths)), Err(error) if error.kind() == io::ErrorKind::NotFound)
                || !matches!(
                    private_file::read_private(&paths.dir, ENROLLMENT, 128),
                    Err(PrivateFileError::Missing)
                )
            {
                return Err(invalid());
            }
            aterm_secrets::client::machine_identity(paths).map_err(|_| invalid())?
        }
    };
    Ok(Key::new(&identity))
}

// A closing record or successor consumes existing issuer history. Missing
// custody must not enroll a new journal before discovering its parent is absent.
fn existing_key(paths: &Paths) -> io::Result<Key> {
    let identity = aterm_secrets::client::read_machine_identity(paths)
        .map_err(|_| invalid())?
        .ok_or_else(invalid)?;
    let key = Key::new(&identity);
    if !enrolled(paths, &key)? {
        return Err(invalid());
    }
    Ok(key)
}

fn record_event_at(
    paths: &Paths,
    prepare: &Prepare,
    binding: &Binding,
    event: Option<Event>,
) -> io::Result<Observed> {
    record_event_at_limits(paths, prepare, binding, event, LIMITS)
}

fn valid_pair(prepare: &Prepare, binding: &Binding) -> bool {
    prepare.id == binding.id
        && matches!(prepare.plan, NativePlan::Restart { .. })
        && Request::prepare(prepare.clone()).encode().is_some()
        && Request::Query(binding.clone()).encode().is_some()
}

fn receipt(key: &Key, prepare: &Prepare, binding: &Binding) -> io::Result<Receipt> {
    if !valid_pair(prepare, binding) {
        return Err(invalid());
    }
    Ok(Receipt {
        operation: key.operation(&prepare.id),
        prepare: key.prepare(prepare)?,
        capability: key.capability(binding),
    })
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecSuccessor",
        action = "ClaimA",
        project = "aterm_agent::harness::native_receipts::tests::successor::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecSuccessor",
        action = "ClaimB",
        project = "aterm_agent::harness::native_receipts::tests::successor::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecSuccessor",
        action = "Retry",
        project = "aterm_agent::harness::native_receipts::tests::successor::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecSuccessor",
        action = "RetryClosed",
        project = "aterm_agent::harness::native_receipts::tests::successor::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecSuccessor",
        action = "Rival",
        project = "aterm_agent::harness::native_receipts::tests::successor::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecSuccessor",
        action = "Conflict",
        project = "aterm_agent::harness::native_receipts::tests::successor::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecSuccessor",
        action = "FailedClaim",
        project = "aterm_agent::harness::native_receipts::tests::successor::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NoExecSuccessor",
        action = "ClaimLostAck",
        reason = "The caller can lose a response after the committed append without changing storage"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NoExecSuccessor",
        action = "Room7",
        reason = "The projection selects the physical log capacity before admission"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NoExecSuccessor",
        action = "Room9",
        reason = "The projection selects the physical log capacity before admission"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NoExecSuccessor",
        action = "TightBytes",
        reason = "The projection selects an independent physical byte cap before admission"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "LegacyA",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
fn record_successor_at(
    paths: &Paths,
    predecessor: &Prepare,
    predecessor_binding: &Binding,
    prepare: &Prepare,
    binding: &Binding,
    limits: Limits,
) -> io::Result<Observed> {
    // Resolve the exact old capability and recipe under the same log lock as
    // the candidate insertion. A missing, legacy or foreign epoch is never a
    // replaceable command. No lease expiry or absent process frees this slot.
    if predecessor.receipt_mode.is_some()
        || prepare.receipt_mode.is_some()
        || !valid_pair(predecessor, predecessor_binding)
        || !valid_pair(prepare, binding)
        || predecessor.id == prepare.id
        || predecessor.host_epoch != prepare.host_epoch
    {
        return Err(invalid());
    }
    let key = existing_key(paths)?;
    let parent_receipt = receipt(&key, predecessor, predecessor_binding)?;
    let candidate = receipt(&key, prepare, binding)?;
    let epoch = key.digest(b"epoch\0", prepare.host_epoch.as_bytes());
    let (mut log, view) = open_existing(paths, &key, limits)?;
    check_rejection_domain(paths, &key, predecessor)?;
    check_rejection_domain(paths, &key, prepare)?;
    let parent = view
        .entries
        .get(&parent_receipt.operation)
        .ok_or_else(invalid)?;
    if parent.receipt != parent_receipt
        || parent.recipients.is_some()
        || parent.state != IssuerState::NoExec
        || parent.epoch != Some(epoch)
        || predecessor.host_epoch != prepare.host_epoch
        || parent_receipt.operation == candidate.operation
    {
        return Err(invalid());
    }
    if let Some(old) = view.entries.get(&candidate.operation) {
        return if old.state == IssuerState::Prepared
            && old.recipients.is_none()
            && old.account.is_none()
            && old.receipt == candidate
            && old.epoch == Some(epoch)
            && old.predecessor == Some(parent_receipt.operation)
            && parent.successor == Some(candidate.operation)
        {
            Ok(old.observed)
        } else {
            Err(invalid())
        };
    }
    if parent.successor.is_some() {
        return Err(invalid());
    }
    if !view.can_append_successor(limits) {
        return Err(io::Error::other(
            "native issuer closing capacity is unavailable",
        ));
    }
    let observed = Observed {
        sequence: view.sequence,
        recorded_ms: aterm_time::unix_millis().ok_or_else(invalid)?,
    };
    log.append(
        SUCCESSOR_SUBJECT,
        view.encode_successor(
            &key,
            &candidate,
            epoch,
            parent_receipt.operation,
            observed.recorded_ms,
        ),
    )?;
    Ok(observed)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "PrepareA",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "PrepareB",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "MayA",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "MayB",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "NoExecA",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "NoExecB",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "SealA",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "SealB",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "RetryA",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "ConflictA",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "ReopenA",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecSuccessor",
        action = "May",
        project = "aterm_agent::harness::native_receipts::tests::successor::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecSuccessor",
        action = "NoExec",
        project = "aterm_agent::harness::native_receipts::tests::successor::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecSuccessor",
        action = "Seal",
        project = "aterm_agent::harness::native_receipts::tests::successor::transition"
    )
)]
fn record_event_at_limits(
    paths: &Paths,
    prepare: &Prepare,
    binding: &Binding,
    event: Option<Event>,
    limits: Limits,
) -> io::Result<Observed> {
    record_with_phase_at_limits(paths, prepare, binding, event, None, limits)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecPhase",
        action = "Close",
        project = "aterm_agent::harness::native_receipts::tests::phase::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecPhase",
        action = "Conflict",
        project = "aterm_agent::harness::native_receipts::tests::phase::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecPhase",
        action = "Retry",
        project = "aterm_agent::harness::native_receipts::tests::phase::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecPhase",
        action = "Refused",
        project = "aterm_agent::harness::native_receipts::tests::phase::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NoExecPhase",
        action = "Legacy",
        reason = "A prior issuer supplied a phase-less closure in the authenticated history"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NoExecPhase",
        action = "LostReply",
        reason = "A transport can lose a response after the durable closure was recorded"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "May",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "NoExec",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "Seal",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "RecipientJournal",
        action = "LegacyWrite",
        project = "aterm_agent::harness::native_receipts::recipients::tests::transition"
    )
)]
fn record_with_phase_at_limits(
    paths: &Paths,
    prepare: &Prepare,
    binding: &Binding,
    event: Option<Event>,
    phase: Option<ImagePhase>,
    limits: Limits,
) -> io::Result<Observed> {
    if prepare.receipt_mode.is_some()
        || (phase.is_some() && event != Some(Event::NoExec))
        || (phase == Some(ImagePhase::Rollback)
            && !matches!(
                &prepare.plan,
                NativePlan::Restart {
                    rollback: Some(_),
                    ..
                }
            ))
    {
        return Err(invalid());
    }
    if !valid_pair(prepare, binding) {
        return Err(invalid());
    }
    let may_enroll = event.is_none() || event == Some(Event::Prepare);
    let key = if may_enroll {
        write_key(paths)?
    } else {
        existing_key(paths)?
    };
    let receipt = receipt(&key, prepare, binding)?;
    let epoch = key.digest(b"epoch\0", prepare.host_epoch.as_bytes());
    let (mut log, view) = if may_enroll {
        open(paths, &key, limits)?
    } else {
        open_existing(paths, &key, limits)?
    };
    check_rejection_domain(paths, &key, prepare)?;
    let previous = view.entries.get(&receipt.operation);
    if let Some(old) = previous {
        if old.recipients.is_some() {
            return Err(invalid());
        }
        if event == Some(Event::Prepare) && old.predecessor.is_some() {
            return Err(invalid());
        }
        let legacy_observation = old.epoch.is_none()
            && old.state == IssuerState::NoExec
            && matches!(event, Some(Event::NoExec | Event::Seal));
        if old.receipt != receipt
            || (event.is_some() && old.epoch != Some(epoch) && !legacy_observation)
        {
            return Err(invalid());
        }
        if phase.is_some() && old.state == IssuerState::NoExec && old.closed_phase != phase {
            // A phase-less closure cannot acquire a guessed phase on retry.
            return Err(invalid());
        }
        if event == Some(Event::NoExec) && !account::permits_noexec(old, phase) {
            return Err(invalid());
        }
        if event.map_or(old.state == IssuerState::NoExec, |event| {
            event.already(old.state)
        }) {
            return Ok(old.observed);
        }
    }
    if let Some(event) = event {
        event.advance(previous.map(|old| old.state))?;
    } else if previous.is_some() {
        return Err(invalid());
    }
    if !view.can_append(event, limits) {
        return Err(io::Error::other(
            "native issuer closing capacity is unavailable",
        ));
    }
    let observed = Observed {
        sequence: view.sequence,
        recorded_ms: aterm_time::unix_millis().ok_or_else(invalid)?,
    };
    let (subject, body) = if let Some(phase) = phase {
        (
            PHASE_SUBJECT,
            view.encode_phase(&key, &receipt, epoch, phase, observed.recorded_ms),
        )
    } else {
        event.map_or_else(
            || (SUBJECT, view.encode(&key, &receipt, observed.recorded_ms)),
            |event| {
                (
                    ISSUER_SUBJECT,
                    view.encode_event(&key, &receipt, epoch, event, observed.recorded_ms),
                )
            },
        )
    };
    log.append(subject, body)?;
    Ok(observed)
}

/// Reserve the complete closing budget before publishing a restart's admission.
/// This writes evidence only; it creates no recoverable execution authority.
/// # Errors
/// Refuses conflicting identities, custody damage or insufficient closing space.
pub fn prepared(prepare: &Prepare, binding: &Binding) -> io::Result<Observed> {
    record_event_at(
        &Paths::locate().map_err(|_| invalid())?,
        prepare,
        binding,
        Some(Event::Prepare),
    )
}

/// Consume an exact NoExec predecessor and reserve its successor's closing space
/// in one durable append. This records a live caller's claim; replay never admits
/// a new operation, restores process witnesses or returns a capability.
/// # Errors
/// Refuses conflicting or consumed predecessors, different host epochs, legacy
/// observations, custody damage or insufficient closing space.
pub fn prepared_successor(
    predecessor: &Prepare,
    predecessor_binding: &Binding,
    prepare: &Prepare,
    binding: &Binding,
) -> io::Result<Observed> {
    record_successor_at(
        &Paths::locate().map_err(|_| invalid())?,
        predecessor,
        predecessor_binding,
        prepare,
        binding,
        LIMITS,
    )
}

/// Record possible execution before any grant can escape. Once this record is
/// attempted, the host must forbid NoExec until it resolves the write outcome.
/// # Errors
/// Refuses absent, closed, conflicting or damaged preparation history.
pub fn may_execute(prepare: &Prepare, binding: &Binding) -> io::Result<Observed> {
    record_event_at(
        &Paths::locate().map_err(|_| invalid())?,
        prepare,
        binding,
        Some(Event::MayExecute),
    )
}

/// Persist a positive NoExec after a durably prepared restart is sealed/drained.
/// # Errors
/// Refuses any possible-execution record, absent preparation or custody damage.
pub fn noexec(prepare: &Prepare, binding: &Binding) -> io::Result<Observed> {
    record_event_at(
        &Paths::locate().map_err(|_| invalid())?,
        prepare,
        binding,
        Some(Event::NoExec),
    )
}

/// Persist the host's retained closed phase beside a positive NoExec fact.
/// The caller must obtain the phase from its live closed-restart witness. This
/// function authenticates storage, not that witness, and grants no authority.
/// # Errors
/// Refuses conflicting phases, legacy terminal observations, possible execution,
/// absent preparation, mismatched identities or damaged custody.
pub fn noexec_in_phase(
    prepare: &Prepare,
    binding: &Binding,
    phase: ImagePhase,
) -> io::Result<PhaseObserved> {
    let observed = record_with_phase_at_limits(
        &Paths::locate().map_err(|_| invalid())?,
        prepare,
        binding,
        Some(Event::NoExec),
        Some(phase),
        LIMITS,
    )?;
    Ok(PhaseObserved {
        phase,
        sequence: observed.sequence,
        recorded_ms: observed.recorded_ms,
    })
}

/// Observe a phase-bearing closure for this exact preparation and capability.
/// Legacy NoExec remains unknown here; replay never restores live authority.
/// # Errors
/// Refuses malformed requests and damaged or untrusted established custody.
pub fn observe_phase(prepare: &Prepare, binding: &Binding) -> io::Result<Option<PhaseObserved>> {
    observe_phase_at(&Paths::locate().map_err(|_| invalid())?, prepare, binding)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NoExecPhase",
        action = "AppendOther",
        reason = "The fixture interleaves an unrelated operation in the same authenticated journal"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecPhase",
        action = "Read",
        project = "aterm_agent::harness::native_receipts::tests::phase::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "TerminalPublication",
        action = "Observe",
        project = "aterm_gui::native_act::tests::publication::transition"
    )
)]
fn observe_phase_at(
    paths: &Paths,
    prepare: &Prepare,
    binding: &Binding,
) -> io::Result<Option<PhaseObserved>> {
    if !valid_pair(prepare, binding) {
        return Err(invalid());
    }
    if prepare.receipt_mode.is_some() {
        return Ok(None);
    }
    let Some((key, view)) = read(paths)? else {
        return Ok(None);
    };
    let expected = receipt(&key, prepare, binding)?;
    let legacy = key.legacy_prepare(prepare);
    let epoch = key.digest(b"epoch\0", prepare.host_epoch.as_bytes());
    Ok(view
        .entries
        .get(&expected.operation)
        .filter(|entry| {
            entry.receipt.operation == expected.operation
                && entry.recipients.is_none()
                && entry.receipt.capability == expected.capability
                && (entry.receipt.prepare == expected.prepare
                    || Some(entry.receipt.prepare) == legacy)
                && entry.epoch == Some(epoch)
                && entry.state == IssuerState::NoExec
        })
        .and_then(|entry| {
            entry.closed_phase.map(|phase| PhaseObserved {
                phase,
                sequence: entry.observed.sequence,
                recorded_ms: entry.observed.recorded_ms,
            })
        }))
}

/// Seal a completed operation without claiming it executed or did not execute.
/// Its prior possible-execution evidence remains permanent; only unused future
/// append reservations are released. This never authorizes a replacement.
/// # Errors
/// Refuses absent, conflicting or damaged preparation history.
pub fn seal(prepare: &Prepare, binding: &Binding) -> io::Result<Observed> {
    record_event_at(
        &Paths::locate().map_err(|_| invalid())?,
        prepare,
        binding,
        Some(Event::Seal),
    )
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecSuccessor",
        action = "Reopen",
        project = "aterm_agent::harness::native_receipts::tests::successor::transition"
    )
)]
fn read(paths: &Paths) -> io::Result<Option<(Key, View)>> {
    let Some(identity) =
        aterm_secrets::client::read_machine_identity(paths).map_err(|_| invalid())?
    else {
        return if custody_present(paths)? {
            Err(invalid())
        } else {
            Ok(None)
        };
    };
    let key = Key::new(&identity);
    if !enrolled(paths, &key)? {
        return if custody_present(paths)? {
            Err(invalid())
        } else {
            Ok(None)
        };
    }
    let Committed::Rebuilt { records, .. } =
        aterm_journal::read_committed(&directory(paths), NAME, &key.anchor, LIMITS, None)?
    else {
        return Err(invalid());
    };
    let mut view = View::default();
    for record in records {
        if !record_subject(&record.subject, &record.body) || record.seq.0 != view.sequence {
            return Err(invalid());
        }
        view.ingest_bounded(&key, &record.body, LIMITS)?;
    }
    Ok(Some((key, view)))
}

/// Read a previously acknowledged NoExec receipt by its secret capability.
/// This performs no writes, creates no identity, and returns no mutation grant.
/// # Errors
/// Refuses unreadable, untrusted, incomplete or damaged established history.
pub fn observe(binding: &Binding) -> io::Result<Option<Observed>> {
    observe_at(&Paths::locate().map_err(|_| invalid())?, binding)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NoExecReceipt",
        action = "ChangeBinding",
        reason = "The caller presents a different capability"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NoExecReceipt",
        action = "Damage",
        reason = "The environment damages the retained checkpoint"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NoExecReceipt",
        action = "LoseKey",
        reason = "The environment replaces the independent issuer identity"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NoExecReceipt",
        action = "Read",
        project = "aterm_agent::harness::native_receipts::tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "ObserveA",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
fn observe_at(paths: &Paths, binding: &Binding) -> io::Result<Option<Observed>> {
    let Some((key, view)) = read(paths)? else {
        return Ok(None);
    };
    Ok(view
        .entries
        .get(&key.operation(&binding.id))
        .filter(|entry| {
            entry.state == IssuerState::NoExec
                && entry.recipients.is_none()
                && entry.receipt.capability == key.capability(binding)
        })
        .map(|entry| entry.observed))
}

/// Recover a lost acknowledgement of an exact original preparation. The
/// response is only the terminal observation; it never returns its capability.
/// # Errors
/// As [`observe`], and when the preparation is not a valid restart recipe.
pub fn observe_prepare(prepare: &Prepare) -> io::Result<Option<Observed>> {
    let paths = Paths::locate().map_err(|_| invalid())?;
    observe_prepare_at(&paths, prepare)
}

fn observe_prepare_at(paths: &Paths, prepare: &Prepare) -> io::Result<Option<Observed>> {
    if prepare.receipt_mode.is_some() {
        Request::prepare(prepare.clone())
            .encode()
            .ok_or_else(invalid)?;
        return Ok(None);
    }
    let Some((key, view)) = read(paths)? else {
        return Ok(None);
    };
    let digest = key.prepare(prepare)?;
    let legacy = key.legacy_prepare(prepare);
    Ok(view
        .entries
        .get(&key.operation(&prepare.id))
        .filter(|entry| {
            entry.state == IssuerState::NoExec
                && entry.recipients.is_none()
                && entry.account.is_none()
                && (entry.receipt.prepare == digest || Some(entry.receipt.prepare) == legacy)
        })
        .map(|entry| entry.observed))
}

/// The established journal's `custody=` digest, which a prepare stamps and a host compares
/// with its own: it names this journal instance by the anchor the enrollment file names and
/// its first record, so a journal enrolled again under the same identity is another custody.
/// It writes, creates and locks nothing; `None` when no journal is established here or the
/// journal holds no record yet.
/// # Errors
/// Refuses unreadable, untrusted, damaged or partly enrolled custody.
pub fn custody() -> io::Result<Option<String>> {
    custody_at(&Paths::locate().map_err(|_| invalid())?)
}

/// [`custody`] of the vault under `home` (the harness's own home, which its records name),
/// located the same read-only way.
/// # Errors
/// As [`custody`].
pub(super) fn custody_for_home(home: &std::path::Path) -> io::Result<Option<String>> {
    custody_at(&Paths::locate_for_home(home))
}

fn custody_at(paths: &Paths) -> io::Result<Option<String>> {
    Ok(read(paths)?.and_then(|(key, view)| view.custody(&key)))
}

/// Read what the journal holds for this exact restart prepare: its own record (matched by its
/// digest or its legacy cold digest), another record of its id, or none. The read takes no
/// lock and sees every acknowledged append; an absence carries the custody digest of the
/// journal instance it was read from ([`custody`]), since only that instance's history proves
/// it, and none from a journal with no record. Account claims and fences read as
/// [`Closure::Other`]; complete histories return [`Closure::Recipients`]. This never
/// returns a capability or grants anything.
/// # Errors
/// Refuses a prepare that is not a valid restart recipe, and damaged, incomplete or
/// untrusted established custody.
pub fn observe_closure(prepare: &Prepare) -> io::Result<Closure> {
    observe_closure_at(&Paths::locate().map_err(|_| invalid())?, prepare)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "RecipientClosure",
        action = "ReadJournal",
        project = "aterm_gui::native_act::tests::recipient_recovery::World::probe"
    )
)]
fn observe_closure_at(paths: &Paths, prepare: &Prepare) -> io::Result<Closure> {
    if !matches!(prepare.plan, NativePlan::Restart { .. })
        || Request::prepare(prepare.clone()).encode().is_none()
    {
        return Err(invalid());
    }
    let Some((key, view)) = read(paths)? else {
        return Ok(Closure::Absent(None));
    };
    let Some(entry) = view.entries.get(&key.operation(&prepare.id)) else {
        return Ok(Closure::Absent(view.custody(&key)));
    };
    if entry.recipients.is_some() {
        return Ok(recipients::observe_entry(&key, entry, prepare)?
            .map_or(Closure::Other, Closure::Recipients));
    }
    if prepare.receipt_mode.is_some() {
        return Ok(Closure::Other);
    }
    let exact = entry.receipt.prepare == key.prepare(prepare)?
        || Some(entry.receipt.prepare) == key.legacy_prepare(prepare);
    if !exact || entry.account.is_some() || entry.recipients.is_some() {
        return Ok(Closure::Other);
    }
    Ok(match entry.state {
        IssuerState::Prepared => Closure::Exact(Recorded::Prepared),
        IssuerState::MayExecute => Closure::Exact(Recorded::MayExecute),
        IssuerState::NoExec => Closure::Exact(Recorded::NoExec),
        IssuerState::Sealed { may_execute } => Closure::Exact(Recorded::Sealed { may_execute }),
        // A rejected prepare's answer is the rejection's own (`unadmitted`, which the epoch
        // refusal serves before this read); this read claims nothing from it.
        IssuerState::Fenced | IssuerState::Rejected => Closure::Other,
    })
}

/// Whether any authenticated issuer record already owns this restart ID. The log only
/// appends, so a record one read finds, every later read finds, short of a rollback of the
/// whole custody tree (module docs).
/// Absence from this catalog alone is never proof of no execution. The host answers a restart
/// id `unadmitted` only when it is also absent from its operation table, read after its clock
/// passed the deadline on the same host epoch: every restart admission is recorded here before
/// it starts, and none starts after its deadline.
/// Fresh native admission also needs its ordinary live authority and epoch checks.
/// # Errors
/// Refuses damaged, incomplete or untrusted established custody.
pub fn contains_operation(id: &str) -> io::Result<bool> {
    contains_operation_at(&Paths::locate().map_err(|_| invalid())?, id)
}

/// Permanently reject an explicitly marked restart recipe only if its ID was never admitted.
/// The append shares admission's journal lock: either preparation wins and this
/// refuses, or this wins and every later preparation of the ID refuses. No
/// execution, rollback or successor capability is recovered from this record.
///
/// # Errors
/// Refuses unmarked, foreign-domain or malformed recipes, damaged custody,
/// unavailable space or an unacknowledged append.
pub fn reject_unadmitted(prepare: &Prepare) -> io::Result<bool> {
    reject_unadmitted_at(&Paths::locate().map_err(|_| invalid())?, prepare, LIMITS)
}

/// Identify this authenticated journal namespace before preparing a restart.
/// The value grants no authority and survives process/host-epoch changes.
/// # Errors
/// Refuses unavailable identity, untrusted custody or failed journal enrollment.
pub fn rejection_domain() -> io::Result<String> {
    rejection_domain_at(&Paths::locate().map_err(|_| invalid())?)
}

fn rejection_domain_at(paths: &Paths) -> io::Result<String> {
    let key = write_key(paths)?;
    let (_log, _view) = open(paths, &key, LIMITS)?;
    rejection_domain_for(paths, &key)
}

fn rejection_domain_for(paths: &Paths, key: &Key) -> io::Result<String> {
    let canonical = std::fs::canonicalize(directory(paths))?;
    Ok(aterm_codec::hex::encode_lower(&key.digest(
        b"rejection-domain\0",
        canonical.as_os_str().as_encoded_bytes(),
    )))
}

fn check_rejection_domain(paths: &Paths, key: &Key, prepare: &Prepare) -> io::Result<()> {
    if let Some(expected) = &prepare.rejection_domain
        && *expected != rejection_domain_for(paths, key)?
    {
        return Err(invalid());
    }
    Ok(())
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "RejectA",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "RejectB",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
fn reject_unadmitted_at(paths: &Paths, prepare: &Prepare, limits: Limits) -> io::Result<bool> {
    if prepare.receipt_mode.is_some()
        || prepare.rejection_domain.is_none()
        || !matches!(prepare.plan, NativePlan::Restart { .. })
        || Request::prepare(prepare.clone()).encode().is_none()
    {
        return Err(invalid());
    }
    let key = existing_key(paths)?;
    if prepare.rejection_domain.as_deref() != Some(rejection_domain_for(paths, &key)?.as_str()) {
        return Err(invalid());
    }
    let receipt = Receipt {
        operation: key.operation(&prepare.id),
        prepare: key.prepare(prepare)?,
        capability: [0; 32],
    };
    let epoch = key.digest(b"epoch\0", prepare.host_epoch.as_bytes());
    let (mut log, view) = open_existing(paths, &key, limits)?;
    if let Some(old) = view.entries.get(&receipt.operation) {
        return Ok(old.state == IssuerState::Rejected
            && old.receipt == receipt
            && old.epoch == Some(epoch));
    }
    if !view.can_append(Some(Event::Reject), limits) {
        return Err(io::Error::other("native rejection capacity is unavailable"));
    }
    let body = view.encode_event(
        &key,
        &receipt,
        epoch,
        Event::Reject,
        aterm_time::unix_millis().ok_or_else(invalid)?,
    );
    log.append(ISSUER_SUBJECT, body)?;
    Ok(true)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerJournal",
        action = "ContainsA",
        project = "aterm_agent::harness::native_receipts::tests::issuer_transition"
    )
)]
fn contains_operation_at(paths: &Paths, id: &str) -> io::Result<bool> {
    let Some((key, view)) = read(paths)? else {
        return Ok(false);
    };
    Ok(view.entries.contains_key(&key.operation(id)))
}

/// Real journal storage in a caller-owned home for cross-crate conformance tests.
#[cfg(any(test, debug_assertions))]
pub mod testing {
    pub use super::replay_bench::{ReplayCorpus, ReplayFormat, ReplaySummary};
    use super::*;

    /// Development fixture events for the complete journal format. No production
    /// writer emits these records until the recovery readers have deployed.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum RecipientAppend {
        Prepare,
        Forward(Recipient),
        Rollback(Recipient),
        NoExec,
        Seal,
    }

    /// Events supplied to the issuer; the stored state is derived by its DFA.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Append {
        Prepare,
        MayExecute,
        NoExec,
        NoExecInPhase(ImagePhase),
        Seal,
    }

    /// Authenticated on-disk state, without any secret or execution capability.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum State {
        Prepared,
        MayExecute,
        NoExec,
        Sealed { may_execute: bool },
        Fenced,
        Rejected,
    }

    /// The same admission and transition code used by the native issuer.
    pub struct Journal {
        paths: Paths,
    }

    impl Journal {
        #[must_use]
        pub fn new(home: &std::path::Path) -> Self {
            Self {
                paths: Paths::for_home(home),
            }
        }

        /// Commit a complete-format fixture with the production codec validation.
        /// # Errors
        /// Refuses invalid transitions, identities, capacity or custody.
        pub fn record_recipient(
            &self,
            prepare: &Prepare,
            binding: &Binding,
            event: RecipientAppend,
        ) -> io::Result<Observed> {
            recipients::append_fixture(&self.paths, prepare, binding, event)
        }

        /// Append using the production authenticator, capacity and DFA checks.
        /// # Errors
        /// Refuses custody damage, invalid transitions and insufficient space.
        pub fn record(
            &self,
            prepare: &Prepare,
            binding: &Binding,
            event: Append,
        ) -> io::Result<Observed> {
            let (event, phase) = match event {
                Append::Prepare => (Event::Prepare, None),
                Append::MayExecute => (Event::MayExecute, None),
                Append::NoExec => (Event::NoExec, None),
                Append::NoExecInPhase(phase) => (Event::NoExec, Some(phase)),
                Append::Seal => (Event::Seal, None),
            };
            record_with_phase_at_limits(&self.paths, prepare, binding, Some(event), phase, LIMITS)
        }

        /// Claim one successor using the production atomic journal transition.
        /// # Errors
        /// Refuses mismatched, consumed or unissued predecessors and damaged custody.
        pub fn successor(
            &self,
            predecessor: &Prepare,
            predecessor_binding: &Binding,
            prepare: &Prepare,
            binding: &Binding,
        ) -> io::Result<Observed> {
            record_successor_at(
                &self.paths,
                predecessor,
                predecessor_binding,
                prepare,
                binding,
                LIMITS,
            )
        }

        /// Reopen and authenticate committed storage, as an independent reader.
        /// # Errors
        /// Refuses damaged or untrusted established custody.
        pub fn state(&self, binding: &Binding) -> io::Result<Option<State>> {
            let Some((key, view)) = read(&self.paths)? else {
                return Ok(None);
            };
            Ok(view
                .entries
                .get(&key.operation(&binding.id))
                .filter(|entry| entry.receipt.capability == key.capability(binding))
                .map(|entry| match entry.state {
                    IssuerState::Prepared => State::Prepared,
                    IssuerState::MayExecute => State::MayExecute,
                    IssuerState::NoExec => State::NoExec,
                    IssuerState::Sealed { may_execute } => State::Sealed { may_execute },
                    IssuerState::Fenced => State::Fenced,
                    IssuerState::Rejected => State::Rejected,
                }))
        }

        /// Exercise the production ANS4 writer with an isolated journal.
        /// # Errors
        /// Refuses an invalid relation, conflicting claim or damaged custody.
        pub fn claim_account(
            &self,
            relation: AccountRelation<'_>,
            binding: &Binding,
        ) -> io::Result<AccountObservation> {
            account::claim_at(&self.paths, relation, binding, LIMITS)
        }

        /// Fence an exact candidate through the production journal transaction.
        /// # Errors
        /// Refuses a claimed candidate, invalid relation or damaged custody.
        pub fn fence_account(
            &self,
            relation: AccountRelation<'_>,
        ) -> io::Result<AccountObservation> {
            account::fence_at(&self.paths, relation, LIMITS)
        }

        /// Inspect permanent ID ownership with the production history reader.
        /// # Errors
        /// Refuses damaged or untrusted established custody.
        pub fn contains_operation(&self, id: &str) -> io::Result<bool> {
            contains_operation_at(&self.paths, id)
        }

        /// Exercise durable rejection with the production journal lock.
        /// # Errors
        /// Refuses malformed requests, damaged custody or unavailable space.
        pub fn reject_unadmitted(&self, prepare: &Prepare) -> io::Result<bool> {
            reject_unadmitted_at(&self.paths, prepare, LIMITS)
        }

        /// Obtain this test journal's same production domain binding.
        /// # Errors
        /// Refuses invalid identity or failed enrollment.
        pub fn rejection_domain(&self) -> io::Result<String> {
            rejection_domain_at(&self.paths)
        }

        /// Recover an existing exact claim with the production durability path.
        /// # Errors
        /// Refuses mismatched relations, capabilities or damaged custody.
        pub fn recover_account(
            &self,
            relation: AccountRelation<'_>,
            binding: &Binding,
        ) -> io::Result<Option<AccountObservation>> {
            account::recover_at(&self.paths, relation, binding)
        }

        /// Count authenticated committed records, without returning their data.
        /// # Errors
        /// Refuses absent or damaged established custody.
        pub fn record_count(&self) -> io::Result<u64> {
            read(&self.paths)?
                .map(|(_, view)| view.sequence)
                .ok_or_else(invalid)
        }

        /// Read a terminal NoExec observation with the production reader.
        /// # Errors
        /// Refuses damaged or untrusted established custody.
        pub fn observe(&self, binding: &Binding) -> io::Result<Option<Observed>> {
            observe_at(&self.paths, binding)
        }

        /// Read an exact prepare's closure with the production lock-free reader.
        /// # Errors
        /// Refuses invalid recipes and damaged or untrusted established custody.
        pub fn observe_closure(&self, prepare: &Prepare) -> io::Result<Closure> {
            observe_closure_at(&self.paths, prepare)
        }

        /// The established custody digest a prepare would be stamped with.
        /// # Errors
        /// Refuses damaged or partly enrolled custody.
        pub fn custody(&self) -> io::Result<Option<String>> {
            custody_at(&self.paths)
        }

        /// Read the exact recorded phase without turning it into live authority.
        /// # Errors
        /// Refuses damaged or untrusted established custody.
        pub fn observe_phase(
            &self,
            prepare: &Prepare,
            binding: &Binding,
        ) -> io::Result<Option<PhaseObserved>> {
            observe_phase_at(&self.paths, prepare, binding)
        }
    }
}

#[cfg(test)]
#[path = "native_receipts_tests.rs"]
mod tests;
