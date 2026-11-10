// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Synthetic authenticated replay inputs. No machine identity, user files or live journal.

use super::*;

/// Journal workloads for measuring the production record replay and prefix capacity checks.
#[derive(Clone, Copy, Debug)]
pub enum ReplayFormat {
    /// Historical terminal receipts, with no outstanding reservations.
    LegacyClosed,
    /// Issuer prepares, each reserving two closing records.
    IssuerOpen,
    /// Complete-recipient prepares, each reserving three records.
    RecipientOpen,
    /// Legacy receipts, closed issuer histories and closed two-recipient histories.
    MixedClosed,
}

/// Semantic result retained by the benchmark, independently of elapsed time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplaySummary {
    /// Distinct operation identities retained after replay.
    pub operations: usize,
    /// Authenticated records replayed.
    pub records: u64,
    /// Bytes charged by the production compatibility accounting.
    pub committed_bytes: usize,
    /// Records still reserved for closure.
    pub reserved_records: usize,
    /// Bytes still reserved at each format's own frame size.
    pub reserved_bytes: usize,
}

/// A bounded, authenticated synthetic history, prepared outside the timed replay.
pub struct ReplayCorpus {
    key: Key,
    bodies: Vec<Vec<u8>>,
}

impl ReplayCorpus {
    /// Build a history using a fixture authenticator, without touching local custody.
    /// # Errors
    /// Refuses zero operations, counts beyond the journal limit, or histories whose final
    /// reservations exceed the production capacity.
    pub fn new(format: ReplayFormat, operations: usize) -> io::Result<Self> {
        if operations == 0 || operations > LIMITS.records {
            return Err(invalid());
        }
        let key = Key {
            mac: hmac::Key::new(hmac::HMAC_SHA256, &[0; 32]),
            anchor: b"synthetic replay benchmark".to_vec(),
        };
        let mut view = View::default();
        let mut bodies = Vec::new();
        for i in 0..operations {
            let closed = matches!(format, ReplayFormat::MixedClosed);
            let id = u64::try_from(i).map_err(|_| invalid())?.to_le_bytes();
            let receipt = Receipt {
                operation: key.digest(b"operation\0", &id),
                prepare: key.digest(b"prepare\0", &id),
                capability: key.digest(b"capability\0", &id),
            };
            let format = match format {
                ReplayFormat::MixedClosed => match i % 3 {
                    0 => ReplayFormat::LegacyClosed,
                    1 => ReplayFormat::IssuerOpen,
                    _ => ReplayFormat::RecipientOpen,
                },
                other => other,
            };
            let epoch = [1; 32];
            match format {
                ReplayFormat::LegacyClosed => {
                    let body = view.encode(&key, &receipt, 1);
                    append(&mut view, &key, &mut bodies, body)?;
                }
                ReplayFormat::IssuerOpen => {
                    let body = view.encode_event(&key, &receipt, epoch, Event::Prepare, 1);
                    append(&mut view, &key, &mut bodies, body)?;
                    if closed {
                        let body = view.encode_event(&key, &receipt, epoch, Event::NoExec, 1);
                        append(&mut view, &key, &mut bodies, body)?;
                    }
                }
                ReplayFormat::RecipientOpen => {
                    let body = recipient_body(&view, &key, &receipt, 1, None);
                    append(&mut view, &key, &mut bodies, body)?;
                    if closed {
                        for (event, who) in [(2, Some(700)), (3, Some(701)), (5, None)] {
                            let who = who.map(|pid| Recipient {
                                pid,
                                birth: 80,
                                group: pid,
                                session: 400,
                            });
                            let body = recipient_body(&view, &key, &receipt, event, who);
                            append(&mut view, &key, &mut bodies, body)?;
                        }
                    }
                }
                ReplayFormat::MixedClosed => unreachable!(),
            }
        }
        if !view.within_capacity(LIMITS, 0) {
            return Err(invalid());
        }
        Ok(Self { key, bodies })
    }

    /// Authenticate and replay each record with the production prefix capacity check.
    /// # Errors
    /// Returns a journal validation or capacity error if the corpus cannot be replayed.
    pub fn replay(&self) -> io::Result<ReplaySummary> {
        let mut view = View::default();
        for body in &self.bodies {
            view.ingest_bounded(&self.key, body, LIMITS)?;
        }
        let (reserved_records, reserved_bytes) =
            view.entries.values().fold((0, 0), |sum, entry| {
                let reserve = entry.reservation();
                (sum.0 + reserve.0, sum.1 + reserve.1)
            });
        Ok(ReplaySummary {
            operations: view.entries.len(),
            records: view.sequence,
            committed_bytes: view.committed_bytes,
            reserved_records,
            reserved_bytes,
        })
    }
}

fn append(view: &mut View, key: &Key, bodies: &mut Vec<Vec<u8>>, body: Vec<u8>) -> io::Result<()> {
    // Setup authenticates each generated record, but checks capacity once at the end:
    // the measured replay must pay every production prefix check itself.
    view.ingest(key, &body)?;
    bodies.push(body);
    Ok(())
}

fn recipient_body(
    view: &View,
    key: &Key,
    receipt: &Receipt,
    event: u8,
    who: Option<Recipient>,
) -> Vec<u8> {
    let mut body = view.encode_event(key, receipt, [1; 32], Event::Prepare, 1);
    body.truncate(181);
    body[..4].copy_from_slice(b"ANG5");
    body[148] = event;
    body.extend_from_slice(&[2; 32]);
    let who = who.unwrap_or(Recipient {
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
    body
}
