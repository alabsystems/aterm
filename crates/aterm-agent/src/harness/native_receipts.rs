// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local, authenticated restart admission, possible execution and terminal history.
//! The native issuer reserves closing space before admission and records possible
//! execution before returning a grant. A [`noexec`] observation requires a sealed,
//! drained operation with no possible execution. Disk records never reconstruct
//! execution or succession authority. A successor claim only consumes a positive
//! NoExec receipt; its caller must retain and revalidate live native evidence.
//! Absence is unknown, never proof of nonexecution.
//! The separate enrollment file detects a missing established journal; neither
//! it nor a signed log protects against rollback of the whole custody tree.

use super::native_act::{Binding, NativePlan, Prepare, Request};
use aterm_journal::{Committed, Limits, Log};
use aterm_secrets::{age::Identity, client::Paths};
use aterm_uds::private_file::{self, PrivateFileError};
use ring::hmac;
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const NAME: &str = "noexec";
const ENROLLMENT: &str = "native-noexec.enrolled";
const SUBJECT: &str = "native/noexec/1";
const BODY_BYTES: usize = 4 + 8 + 8 + 32 * 5;
const ISSUER_SUBJECT: &str = "native/issuer/2";
const ISSUER_BYTES: usize = BODY_BYTES + 1 + 32;
const SUCCESSOR_SUBJECT: &str = "native/successor/3";
const SUCCESSOR_BYTES: usize = ISSUER_BYTES + 32;
const ISSUER_FRAME: usize = ISSUER_BYTES + ISSUER_SUBJECT.len() + aterm_journal::APPEND_OVERHEAD;
const SUCCESSOR_FRAME: usize =
    SUCCESSOR_BYTES + SUCCESSOR_SUBJECT.len() + aterm_journal::APPEND_OVERHEAD;
const LIMITS: Limits = Limits {
    records: 32_768,
    bytes: 16 * 1024 * 1024,
    record_bytes: SUCCESSOR_BYTES,
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
}

impl IssuerState {
    fn reserved(self) -> usize {
        match self {
            Self::Prepared => 2,
            Self::MayExecute => 1,
            Self::NoExec | Self::Sealed { .. } => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Prepare = 1,
    MayExecute = 2,
    NoExec = 3,
    Seal = 4,
}

impl Event {
    fn parse(byte: u8) -> io::Result<Self> {
        match byte {
            1 => Ok(Self::Prepare),
            2 => Ok(Self::MayExecute),
            3 => Ok(Self::NoExec),
            4 => Ok(Self::Seal),
            _ => Err(invalid()),
        }
    }

    fn advance(self, previous: Option<IssuerState>) -> io::Result<IssuerState> {
        match (previous, self) {
            (None, Self::Prepare) => Ok(IssuerState::Prepared),
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
}

/// A verified observation, deliberately without any reusable native capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observed {
    /// Local wall-clock timestamp, for attribution, not ordering or authorization.
    pub recorded_ms: u64,
}

#[derive(Default)]
struct View {
    entries: BTreeMap<Digest, Entry>,
    head: Digest,
    sequence: u64,
    committed_bytes: usize,
}

impl View {
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
    fn ingest(&mut self, key: &Key, body: &[u8]) -> io::Result<()> {
        let legacy = body.len() == BODY_BYTES && &body[..4] == b"ANE1";
        let issuer = body.len() == ISSUER_BYTES && &body[..4] == b"ANI2";
        let successor = body.len() == SUCCESSOR_BYTES && &body[..4] == b"ANI3";
        if !legacy && !issuer && !successor {
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
        let predecessor = if successor {
            let predecessor = digest(181);
            let parent = self.entries.get(&predecessor).ok_or_else(invalid)?;
            if old.is_some()
                || Event::parse(body[148])? != Event::Prepare
                || parent.state != IssuerState::NoExec
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
            (
                Event::parse(body[148])?.advance(old.map(|old| old.state))?,
                Some(epoch),
            )
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
        if successor {
            self.entries
                .get_mut(&predecessor.unwrap())
                .unwrap()
                .successor = Some(receipt.operation);
        }
        self.entries.insert(
            receipt.operation,
            Entry {
                receipt,
                observed: Observed {
                    recorded_ms: number(12),
                },
                state,
                epoch,
                predecessor,
                successor: retained_successor,
            },
        );
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

    fn can_append(&self, event: Option<Event>, limits: Limits) -> bool {
        // Count every epoch. Process-local host epochs do not fence other live
        // processes using the same Paths. Nothing silently expires a reservation.
        self.within_capacity(
            limits,
            match event {
                Some(Event::Prepare) => 3,
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
        let Some(reserved) = self
            .entries
            .values()
            .try_fold(0usize, |sum, entry| sum.checked_add(entry.state.reserved()))
        else {
            return false;
        };
        let Ok(retained) = usize::try_from(self.sequence) else {
            return false;
        };
        let Some(total) = retained
            .checked_add(reserved)
            .and_then(|n| n.checked_add(added))
        else {
            return false;
        };
        total <= limits.records
            && reserved
                .checked_mul(ISSUER_FRAME)
                .and_then(|bytes| bytes.checked_add(self.committed_bytes))
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
            view.ingest(key, &record.body)?;
            if !view.within_capacity(limits, 0) {
                return Err(invalid());
            }
        }
    }
    Ok(view)
}

fn record_subject(subject: &str, body: &[u8]) -> bool {
    (subject == SUBJECT && body.starts_with(b"ANE1"))
        || (subject == ISSUER_SUBJECT && body.starts_with(b"ANI2"))
        || (subject == SUCCESSOR_SUBJECT && body.starts_with(b"ANI3"))
}

fn open(paths: &Paths, key: &Key, limits: Limits) -> io::Result<(Log, View)> {
    // Serialize enrollment as well as append across independent app processes.
    // This lock is acquired only on the control worker, never under native locks.
    let _enrollment = private_file::PrivateLock::take(
        &paths.dir,
        "native-noexec.lock",
        Duration::from_secs(1),
        "native receipt enrollment is busy",
    )?;
    let established = enrolled(paths, key)?;
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
    if !valid_pair(predecessor, predecessor_binding)
        || !valid_pair(prepare, binding)
        || predecessor.id == prepare.id
        || predecessor.host_epoch != prepare.host_epoch
    {
        return Err(invalid());
    }
    let key = write_key(paths)?;
    let parent_receipt = receipt(&key, predecessor, predecessor_binding)?;
    let candidate = receipt(&key, prepare, binding)?;
    let epoch = key.digest(b"epoch\0", prepare.host_epoch.as_bytes());
    let (mut log, view) = open(paths, &key, limits)?;
    let parent = view
        .entries
        .get(&parent_receipt.operation)
        .ok_or_else(invalid)?;
    if parent.receipt != parent_receipt
        || parent.state != IssuerState::NoExec
        || parent.epoch != Some(epoch)
        || predecessor.host_epoch != prepare.host_epoch
        || parent_receipt.operation == candidate.operation
    {
        return Err(invalid());
    }
    if let Some(old) = view.entries.get(&candidate.operation) {
        return if old.state == IssuerState::Prepared
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
    if !valid_pair(prepare, binding) {
        return Err(invalid());
    }
    let key = write_key(paths)?;
    let receipt = receipt(&key, prepare, binding)?;
    let epoch = key.digest(b"epoch\0", prepare.host_epoch.as_bytes());
    let (mut log, view) = open(paths, &key, limits)?;
    let previous = view.entries.get(&receipt.operation);
    if let Some(old) = previous {
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
        recorded_ms: aterm_time::unix_millis().ok_or_else(invalid)?,
    };
    let (subject, body) = event.map_or_else(
        || (SUBJECT, view.encode(&key, &receipt, observed.recorded_ms)),
        |event| {
            (
                ISSUER_SUBJECT,
                view.encode_event(&key, &receipt, epoch, event, observed.recorded_ms),
            )
        },
    );
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
        view.ingest(&key, &record.body)?;
        if !view.within_capacity(LIMITS, 0) {
            return Err(invalid());
        }
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
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "TerminalPublication",
        action = "Observe",
        project = "aterm_gui::native_act::tests::publication::transition"
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
    let Some((key, view)) = read(paths)? else {
        return Ok(None);
    };
    let digest = key.prepare(prepare)?;
    Ok(view
        .entries
        .get(&key.operation(&prepare.id))
        .filter(|entry| entry.state == IssuerState::NoExec && entry.receipt.prepare == digest)
        .map(|entry| entry.observed))
}

/// Whether any authenticated issuer record already owns this restart ID.
/// Absence from this catalog is never proof of no execution.
/// Fresh native admission also needs its ordinary live authority and epoch checks.
/// # Errors
/// Refuses damaged, incomplete or untrusted established custody.
pub fn contains_operation(id: &str) -> io::Result<bool> {
    let Some((key, view)) = read(&Paths::locate().map_err(|_| invalid())?)? else {
        return Ok(false);
    };
    Ok(view.entries.contains_key(&key.operation(id)))
}

/// Real journal storage in a caller-owned home for cross-crate conformance tests.
#[cfg(any(test, debug_assertions))]
pub mod testing {
    use super::*;

    /// Events supplied to the issuer; the stored state is derived by its DFA.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Append {
        Prepare,
        MayExecute,
        NoExec,
        Seal,
    }

    /// Authenticated on-disk state, without any secret or execution capability.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum State {
        Prepared,
        MayExecute,
        NoExec,
        Sealed { may_execute: bool },
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

        /// Append using the production authenticator, capacity and DFA checks.
        /// # Errors
        /// Refuses custody damage, invalid transitions and insufficient space.
        pub fn record(
            &self,
            prepare: &Prepare,
            binding: &Binding,
            event: Append,
        ) -> io::Result<Observed> {
            let event = match event {
                Append::Prepare => Event::Prepare,
                Append::MayExecute => Event::MayExecute,
                Append::NoExec => Event::NoExec,
                Append::Seal => Event::Seal,
            };
            record_event_at(&self.paths, prepare, binding, Some(event))
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
                }))
        }

        /// Read a terminal NoExec observation with the production reader.
        /// # Errors
        /// Refuses damaged or untrusted established custody.
        pub fn observe(&self, binding: &Binding) -> io::Result<Option<Observed>> {
            observe_at(&self.paths, binding)
        }
    }
}

#[cfg(test)]
#[path = "native_receipts_tests.rs"]
mod tests;
