// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const SUBJECT: &str = "native/recipient-binding/1";
const BODY_BYTES: usize = 228;
const FRAME_BYTES: usize = BODY_BYTES + SUBJECT.len() + aterm_journal::APPEND_OVERHEAD;

/// An acknowledged link between one complete-journal identity and one exact
/// legacy reservation. It does not grant admission, execution or historical closure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundRecipientCustody {
    custody: RecipientCustody,
    observed: Observed,
}

impl BoundRecipientCustody {
    /// Complete-journal identity that the immutable recipe named.
    #[must_use]
    pub fn custody(&self) -> &RecipientCustody {
        &self.custody
    }

    /// Original binding record's position.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.observed.sequence
    }

    /// Original binding record's attribution time, never an authority clock.
    #[must_use]
    pub const fn recorded_ms(&self) -> u64 {
        self.observed.recorded_ms
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Entry {
    receipt: Receipt,
    legacy_custody: Digest,
    legacy_sequence: u64,
    legacy_time: u64,
    observed: Observed,
}

#[derive(Default)]
pub(super) struct View {
    pub(super) sequence: u64,
    pub(super) head: Digest,
    bytes: usize,
    entries: BTreeMap<Digest, Entry>,
    histories: BTreeMap<Digest, super::super::Entry>,
    reserved: usize,
}

fn capacity(reserved_records: usize, committed: usize, records: u64, limits: Limits) -> bool {
    let Some(reserved_bytes) = reserved_records.checked_mul(recipients::FRAME_BYTES) else {
        return false;
    };
    usize::try_from(records)
        .ok()
        .and_then(|count| count.checked_add(reserved_records))
        .is_some_and(|count| count <= limits.records)
        && committed
            .checked_add(reserved_bytes)
            .is_some_and(|bytes| bytes <= limits.bytes)
}

impl View {
    pub(super) fn require_binding(
        &self,
        key: &Key,
        prepare: &Prepare,
        binding: &Binding,
        held: &reservation::HeldReservation,
    ) -> io::Result<()> {
        let observed = self.legacy_origin(key, prepare, binding, &held.custody)?;
        if observed.sequence != held.acknowledgement.sequence()
            || observed.recorded_ms != held.acknowledgement.recorded_ms()
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub(super) fn legacy_origin(
        &self,
        key: &Key,
        prepare: &Prepare,
        binding: &Binding,
        custody: &str,
    ) -> io::Result<Observed> {
        let expected = receipt(key, prepare, binding)?;
        let old = self.entries.get(&expected.operation).ok_or_else(invalid)?;
        if old.receipt != expected
            || old.legacy_custody != key.digest(b"legacy-custody\0", custody.as_bytes())
        {
            return Err(invalid());
        }
        Ok(Observed {
            sequence: old.legacy_sequence,
            recorded_ms: old.legacy_time,
        })
    }

    pub(super) fn ingest_record(
        &mut self,
        key: &Key,
        subject: &str,
        body: &[u8],
    ) -> io::Result<()> {
        match subject {
            SUBJECT => self.ingest(key, body),
            recipients::SUBJECT => self.ingest_recipient(key, body),
            _ => Err(invalid()),
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "BoundRecipientJournal",
            action = "Record",
            project = "aterm_agent::harness::native_receipts::recipient_custody::binding::history::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "BoundRecipientJournal",
            action = "Bind",
            reason = "The independently checked RecipientCustody protocol establishes the binding prerequisite"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "BoundRecipientJournal",
            action = "WrongBinding",
            reason = "The caller changes its requested immutable recipe without changing the journal"
        )
    )]
    fn ingest_recipient(&mut self, key: &Key, body: &[u8]) -> io::Result<()> {
        let next = recipients::decode_entry(key, body, self.sequence, self.head, &self.histories)?;
        let binding = self
            .entries
            .get(&next.receipt.operation)
            .ok_or_else(invalid)?;
        if binding.receipt != next.receipt {
            return Err(invalid());
        }
        let previous = self
            .histories
            .get(&next.receipt.operation)
            .map_or(4, |entry| entry.reservation().0);
        let reserved = self
            .reserved
            .checked_sub(previous)
            .and_then(|count| count.checked_add(next.reservation().0))
            .ok_or_else(invalid)?;
        let sequence = self.sequence.checked_add(1).ok_or_else(invalid)?;
        let bytes = self
            .bytes
            .checked_add(recipients::FRAME_BYTES)
            .ok_or_else(invalid)?;
        if !capacity(reserved, bytes, sequence, LIMITS) {
            return Err(invalid());
        }
        self.histories.insert(next.receipt.operation, next);
        self.sequence = sequence;
        self.bytes = bytes;
        self.reserved = reserved;
        self.head = key.digest(b"receipt\0", body);
        Ok(())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "BoundRecipientJournal",
            action = "Read",
            project = "aterm_agent::harness::native_receipts::recipient_custody::binding::history::transition"
        )
    )]
    pub(super) fn observe(
        &self,
        key: &Key,
        prepare: &Prepare,
    ) -> io::Result<Option<RecipientHistory>> {
        self.histories
            .get(&key.operation(&prepare.id))
            .map_or(Ok(None), |entry| {
                recipients::observe_entry(key, entry, prepare)
            })
    }

    fn ingest(&mut self, key: &Key, body: &[u8]) -> io::Result<()> {
        if body.len() != BODY_BYTES || &body[..4] != b"ARB1" {
            return Err(invalid());
        }
        hmac::verify(&key.mac, &body[..196], &body[196..]).map_err(|_| invalid())?;
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
        if self.entries.contains_key(&receipt.operation) {
            return Err(invalid());
        }
        let sequence = self.sequence.checked_add(1).ok_or_else(invalid)?;
        let bytes = self.bytes.checked_add(FRAME_BYTES).ok_or_else(invalid)?;
        let reserved = self.reserved.checked_add(4).ok_or_else(invalid)?;
        if !capacity(reserved, bytes, sequence, LIMITS) {
            return Err(invalid());
        }
        let entry = Entry {
            receipt,
            legacy_custody: digest(148),
            legacy_sequence: number(180),
            legacy_time: number(188),
            observed: Observed {
                sequence: self.sequence,
                recorded_ms: number(12),
            },
        };
        self.entries.insert(entry.receipt.operation, entry);
        self.sequence = sequence;
        self.bytes = bytes;
        self.reserved = reserved;
        self.head = key.digest(b"receipt\0", body);
        Ok(())
    }

    fn encode(&self, key: &Key, entry: &Entry) -> Vec<u8> {
        let mut body = b"ARB1".to_vec();
        body.extend_from_slice(&self.sequence.to_le_bytes());
        body.extend_from_slice(&entry.observed.recorded_ms.to_le_bytes());
        body.extend_from_slice(&self.head);
        body.extend_from_slice(&entry.receipt.operation);
        body.extend_from_slice(&entry.receipt.prepare);
        body.extend_from_slice(&entry.receipt.capability);
        body.extend_from_slice(&entry.legacy_custody);
        body.extend_from_slice(&entry.legacy_sequence.to_le_bytes());
        body.extend_from_slice(&entry.legacy_time.to_le_bytes());
        body.extend_from_slice(hmac::sign(&key.mac, &body).as_ref());
        body
    }
}

pub(super) fn replay(log: &Log, key: &Key) -> io::Result<View> {
    let mut view = View::default();
    loop {
        let page = log.read_range(astream_wire::Offset(view.sequence), 256)?;
        if page.is_empty() {
            break;
        }
        for record in page {
            if record.seq.0 != view.sequence {
                return Err(invalid());
            }
            view.ingest_record(key, &record.subject, &record.body)?;
        }
    }
    Ok(view)
}

/// Commit the exact legacy reservation into its independently enrolled complete journal.
/// The recipe must already name this journal's custody stamp. Both journal locks
/// remain held through acknowledgement, in legacy-then-complete order. No Execute
/// or recipient record is emitted and no missing enrollment is initialized here.
/// # Errors
/// Refuses invalid recipes, missing or mismatched custody/reservations, conflicting
/// bindings, capacity exhaustion, competing writers, or failed durability barriers.
pub fn bind_recipient_custody(
    prepare: &Prepare,
    binding: &Binding,
) -> io::Result<BoundRecipientCustody> {
    bind_at(&Paths::locate().map_err(|_| invalid())?, prepare, binding)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "RecipientCustody",
        action = "Bind",
        project = "aterm_agent::harness::native_receipts::recipient_custody::tests::model::transition"
    )
)]
pub(super) fn bind_at(
    paths: &Paths,
    prepare: &Prepare,
    binding: &Binding,
) -> io::Result<BoundRecipientCustody> {
    if prepare.receipt_mode != Some(ReceiptMode::Recipients5) || !valid_pair(prepare, binding) {
        return Err(invalid());
    }
    let held = reservation::hold_at(paths, prepare, binding)?;
    let mut opened = open_at(paths, false)?;
    if prepare.custody.as_deref() != Some(opened.custody.stamp()) {
        return Err(invalid());
    }
    let mut entry = Entry {
        receipt: receipt(&opened.key, prepare, binding)?,
        legacy_custody: opened
            .key
            .digest(b"legacy-custody\0", held.custody.as_bytes()),
        legacy_sequence: held.acknowledgement.sequence(),
        legacy_time: held.acknowledgement.recorded_ms(),
        observed: Observed {
            sequence: opened.view.sequence,
            recorded_ms: 0,
        },
    };
    if let Some(old) = opened.view.entries.get(&entry.receipt.operation) {
        entry.observed = old.observed;
        if *old != entry {
            return Err(invalid());
        }
    } else {
        entry.observed.recorded_ms = aterm_time::unix_millis().ok_or_else(invalid)?;
        let body = opened.view.encode(&opened.key, &entry);
        opened.view.ingest(&opened.key, &body)?;
        opened.log.append(SUBJECT, body)?;
    }
    drop(held);
    Ok(BoundRecipientCustody {
        custody: opened.custody,
        observed: entry.observed,
    })
}

#[cfg(test)]
#[path = "native_recipient_history_model_tests.rs"]
mod history;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_replay_keeps_four_recipient_frames_per_operation() {
        let (_home, paths, _, _) = super::super::super::tests::fixture();
        let key = write_key(&paths).unwrap();
        let maximum =
            (LIMITS.records / 5).min(LIMITS.bytes / (FRAME_BYTES + 4 * recipients::FRAME_BYTES));
        let mut view = View::default();
        for number in 0..=maximum {
            let entry = Entry {
                receipt: Receipt {
                    operation: key.operation(&format!("{number:032x}")),
                    prepare: [1; 32],
                    capability: [2; 32],
                },
                legacy_custody: [3; 32],
                legacy_sequence: 0,
                legacy_time: 1,
                observed: Observed {
                    sequence: view.sequence,
                    recorded_ms: 1,
                },
            };
            let body = view.encode(&key, &entry);
            let sequence = view.sequence;
            assert_eq!(view.ingest(&key, &body).is_ok(), number < maximum);
            if number == maximum {
                assert_eq!(view.sequence, sequence);
            }
        }
    }

    #[test]
    fn binding_records_authenticate_and_cannot_replace_an_operation() {
        let (_home, paths, _, _) = super::super::super::tests::fixture();
        let key = write_key(&paths).unwrap();
        let mut view = View::default();
        let entry = Entry {
            receipt: Receipt {
                operation: [1; 32],
                prepare: [2; 32],
                capability: [3; 32],
            },
            legacy_custody: [4; 32],
            legacy_sequence: 5,
            legacy_time: 6,
            observed: Observed {
                sequence: 0,
                recorded_ms: 7,
            },
        };
        let valid = view.encode(&key, &entry);
        let mut forged = valid.clone();
        forged[148] ^= 1;
        assert!(view.ingest(&key, &forged).is_err());
        assert_eq!(view.sequence, 0);
        view.ingest(&key, &valid).unwrap();
        assert!(view.ingest(&key, &view.encode(&key, &entry)).is_err());
        assert_eq!(view.sequence, 1);
    }
}
