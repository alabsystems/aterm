// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reader-first format for complete, per-phase possible-recipient histories.
//! Application admission does not use this format yet. Legacy append paths refuse it.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum Append {
    Prepare,
    Forward(Recipient),
    Rollback(Recipient),
    NoExec,
    Seal,
}

pub(super) const SUBJECT: &str = "native/recipients/5";
pub(super) const BODY_BYTES: usize = 265;
pub(super) const FRAME_BYTES: usize = BODY_BYTES + SUBJECT.len() + aterm_journal::APPEND_OVERHEAD;

/// Kernel identity recorded before a possible Execute delivery. This is evidence,
/// not a process handle or permission to signal, execute or resume anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recipient {
    pub pid: u32,
    pub birth: u64,
    pub group: u32,
    pub session: u32,
}

impl Recipient {
    fn valid(self) -> bool {
        (2..=i32::MAX as u32).contains(&self.pid)
            && self.birth > 0
            && self.group == self.pid
            && (2..=i32::MAX as u32).contains(&self.session)
    }
}

/// An exact prepare's complete possible-recipient set in the new journal format.
/// Neither an empty set nor a terminal state proves retirement of other effects.
/// A future recovery reader must also establish the deadline and process-group
/// retirement; this observation alone never authorizes a new agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipientHistory {
    pub forward: Option<Recipient>,
    pub rollback: Option<Recipient>,
    pub state: Recorded,
    /// Position and attribution time of the most recent record for this prepare.
    pub observed: Observed,
}

#[derive(Clone)]
pub(super) struct History {
    boot: Digest,
    forward: Option<Recipient>,
    rollback: Option<Recipient>,
}

impl History {
    pub(super) fn reserved(&self, state: IssuerState) -> usize {
        match state {
            IssuerState::Prepared => 3,
            IssuerState::MayExecute if self.rollback.is_none() => 2,
            IssuerState::MayExecute => 1,
            _ => 0,
        }
    }
}

/// Observe only the complete format, matched to the exact recipe and its boot.
/// Legacy, absent or differently bound records return `None`, never an empty set.
/// This reads without creating custody and returns no reusable capability.
/// # Errors
/// Refuses malformed or unstamped recipes and damaged or untrusted custody.
pub fn observe_recipients(prepare: &Prepare) -> io::Result<Option<RecipientHistory>> {
    observe_at(&Paths::locate().map_err(|_| invalid())?, prepare)
}

fn boot_digest(key: &Key, prepare: &Prepare) -> io::Result<Digest> {
    if !matches!(prepare.plan, NativePlan::Restart { .. })
        || Request::prepare(prepare.clone()).encode().is_none()
    {
        return Err(invalid());
    }
    let boot = prepare.boot.as_ref().ok_or_else(invalid)?;
    Ok(key.digest(
        b"recipient-boot\0",
        format!("{}:{}", boot.host, boot.boot).as_bytes(),
    ))
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "RecipientJournal",
        action = "Read",
        project = "aterm_agent::harness::native_receipts::recipients::tests::transition"
    )
)]
pub(super) fn observe_at(paths: &Paths, prepare: &Prepare) -> io::Result<Option<RecipientHistory>> {
    // Validate before reading even an absent custody.
    if !matches!(prepare.plan, NativePlan::Restart { .. })
        || prepare.boot.is_none()
        || Request::prepare(prepare.clone()).encode().is_none()
    {
        return Err(invalid());
    }
    if prepare.receipt_mode.is_some() {
        return recipient_custody::observe_at(paths, prepare);
    }
    let Some((key, view)) = read(paths)? else {
        return Ok(None);
    };
    let Some(entry) = view.entries.get(&key.operation(&prepare.id)) else {
        return Ok(None);
    };
    observe_entry(&key, entry, prepare)
}

pub(super) fn observe_entry(
    key: &Key,
    entry: &Entry,
    prepare: &Prepare,
) -> io::Result<Option<RecipientHistory>> {
    let Some(history) = &entry.recipients else {
        return Ok(None);
    };
    if entry.receipt.prepare != key.prepare(prepare)?
        || entry.epoch != Some(key.digest(b"epoch\0", prepare.host_epoch.as_bytes()))
        || history.boot != boot_digest(key, prepare)?
    {
        return Ok(None);
    }
    let state = match entry.state {
        IssuerState::Prepared => Recorded::Prepared,
        IssuerState::MayExecute => Recorded::MayExecute,
        IssuerState::NoExec => Recorded::NoExec,
        IssuerState::Sealed { may_execute } => Recorded::Sealed { may_execute },
        IssuerState::Fenced | IssuerState::Rejected => return Err(invalid()),
    };
    Ok(Some(RecipientHistory {
        forward: history.forward,
        rollback: history.rollback,
        state,
        observed: entry.observed,
    }))
}

/// Build a committed fixture through the real authenticator, parser and capacity
/// checks. This is available only to development conformance callers.
#[cfg(any(test, debug_assertions))]
pub(super) fn append_fixture(
    paths: &Paths,
    prepare: &Prepare,
    binding: &Binding,
    event: super::testing::RecipientAppend,
) -> io::Result<Observed> {
    if prepare.receipt_mode.is_some() {
        return recipient_custody::append_fixture(paths, prepare, binding, event);
    }
    let key = write_key(paths)?;
    let (mut log, mut view) = open(paths, &key, LIMITS)?;
    let observed = Observed {
        sequence: view.sequence,
        recorded_ms: aterm_time::unix_millis().ok_or_else(invalid)?,
    };
    let body = encode_fixture(
        &key,
        view.sequence,
        view.head,
        prepare,
        binding,
        event,
        observed.recorded_ms,
    )?;
    view.ingest(&key, &body)?;
    if !view.within_capacity(LIMITS, 0) {
        return Err(invalid());
    }
    log.append(SUBJECT, body)?;
    Ok(observed)
}

#[cfg(any(test, debug_assertions))]
pub(super) fn encode_fixture(
    key: &Key,
    sequence: u64,
    head: Digest,
    prepare: &Prepare,
    binding: &Binding,
    event: super::testing::RecipientAppend,
    recorded_ms: u64,
) -> io::Result<Vec<u8>> {
    use super::testing::RecipientAppend;
    let event = match event {
        RecipientAppend::Prepare => Append::Prepare,
        RecipientAppend::Forward(who) => Append::Forward(who),
        RecipientAppend::Rollback(who) => Append::Rollback(who),
        RecipientAppend::NoExec => Append::NoExec,
        RecipientAppend::Seal => Append::Seal,
    };
    encode_event(key, sequence, head, prepare, binding, event, recorded_ms)
}

pub(super) fn encode_event(
    key: &Key,
    sequence: u64,
    head: Digest,
    prepare: &Prepare,
    binding: &Binding,
    event: Append,
    recorded_ms: u64,
) -> io::Result<Vec<u8>> {
    let (code, recipient) = match event {
        Append::Prepare => (1, None),
        Append::Forward(who) => (2, Some(who)),
        Append::Rollback(who) => (3, Some(who)),
        Append::NoExec => (4, None),
        Append::Seal => (5, None),
    };
    let receipt = receipt(key, prepare, binding)?;
    let epoch = key.digest(b"epoch\0", prepare.host_epoch.as_bytes());
    let mut body = b"ANG5".to_vec();
    body.extend_from_slice(&sequence.to_le_bytes());
    body.extend_from_slice(&recorded_ms.to_le_bytes());
    body.extend_from_slice(&head);
    body.extend_from_slice(&receipt.operation);
    body.extend_from_slice(&receipt.prepare);
    body.extend_from_slice(&receipt.capability);
    body.push(code);
    body.extend_from_slice(&epoch);
    body.extend_from_slice(&boot_digest(key, prepare)?);
    let who = recipient.unwrap_or(Recipient {
        pid: 0,
        birth: 0,
        group: 0,
        session: 0,
    });
    body.extend_from_slice(&who.pid.to_le_bytes());
    body.extend_from_slice(&who.birth.to_le_bytes());
    body.extend_from_slice(&who.group.to_le_bytes());
    body.extend_from_slice(&who.session.to_le_bytes());
    body.extend_from_slice(hmac::sign(&key.mac, &body).as_ref());
    Ok(body)
}

pub(super) fn ingest(view: &mut View, key: &Key, body: &[u8]) -> io::Result<()> {
    let entry = decode_entry(key, body, view.sequence, view.head, &view.entries)?;
    let sequence = view.sequence.checked_add(1).ok_or_else(invalid)?;
    let committed_bytes = view
        .committed_bytes
        .checked_add(FRAME_BYTES)
        .ok_or_else(invalid)?;
    view.replace_entry(entry)?;
    view.sequence = sequence;
    view.committed_bytes = committed_bytes;
    view.head = key.digest(b"receipt\0", body);
    Ok(())
}

/// Authenticate one event against its journal chain and return its next state.
/// Both historical and independently bound journals use this transition function.
pub(super) fn decode_entry(
    key: &Key,
    body: &[u8],
    sequence: u64,
    head: Digest,
    entries: &BTreeMap<Digest, Entry>,
) -> io::Result<Entry> {
    if body.len() != BODY_BYTES || &body[..4] != b"ANG5" {
        return Err(invalid());
    }
    hmac::verify(&key.mac, &body[..233], &body[233..]).map_err(|_| invalid())?;
    let number = |at| u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
    let small = |at| u32::from_le_bytes(body[at..at + 4].try_into().unwrap());
    let digest = |at| -> Digest { body[at..at + 32].try_into().unwrap() };
    if number(4) != sequence || digest(20) != head || digest(181) == [0; 32] {
        return Err(invalid());
    }
    let receipt = Receipt {
        operation: digest(52),
        prepare: digest(84),
        capability: digest(116),
    };
    let epoch = digest(149);
    let recipient = Recipient {
        pid: small(213),
        birth: number(217),
        group: small(225),
        session: small(229),
    };
    let event = body[148];
    if matches!(event, 2 | 3) {
        if !recipient.valid() {
            return Err(invalid());
        }
    } else if body[213..233] != [0; 20] {
        return Err(invalid());
    }
    let old = entries.get(&receipt.operation);
    let (mut history, previous) = if let Some(old) = old {
        let history = old.recipients.as_ref().ok_or_else(invalid)?;
        if old.receipt != receipt || old.epoch != Some(epoch) || history.boot != digest(181) {
            return Err(invalid());
        }
        (history.clone(), Some(old.state))
    } else {
        (
            History {
                boot: digest(181),
                forward: None,
                rollback: None,
            },
            None,
        )
    };
    let state = match (event, previous) {
        (1, None) => IssuerState::Prepared,
        (2, Some(IssuerState::Prepared)) => {
            history.forward = Some(recipient);
            IssuerState::MayExecute
        }
        (3, Some(IssuerState::Prepared | IssuerState::MayExecute))
            if history.rollback.is_none() =>
        {
            if history.forward.is_some_and(|forward| {
                (forward.pid == recipient.pid && forward.birth == recipient.birth)
                    || forward.session != recipient.session
            }) {
                return Err(invalid());
            }
            history.rollback = Some(recipient);
            IssuerState::MayExecute
        }
        (4, Some(IssuerState::Prepared)) => IssuerState::NoExec,
        (5, Some(IssuerState::Prepared)) => IssuerState::Sealed { may_execute: false },
        (5, Some(IssuerState::MayExecute)) => IssuerState::Sealed { may_execute: true },
        _ => return Err(invalid()),
    };
    Ok(Entry {
        receipt,
        observed: Observed {
            sequence,
            recorded_ms: number(12),
        },
        epoch: Some(epoch),
        state,
        predecessor: None,
        successor: None,
        closed_phase: None,
        account: None,
        recipients: Some(history),
    })
}

#[cfg(test)]
#[path = "native_receipts_recipients_tests.rs"]
mod tests;
