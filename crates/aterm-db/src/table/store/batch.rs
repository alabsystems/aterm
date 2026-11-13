// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticate and preflight an entire bounded ingest before publishing its exact
//! recovery permission, checkpoint group, and independent receipt witness.

use super::*;

/// Metadata for a proposal; payload bytes stay borrowed until the group commits.
struct Prepared<'a> {
    wire: &'a [u8],
    header: Header,
    receipt: Receipt,
    cut: Option<cutoff::Cut>,
}

impl Store {
    /// Ingest up to 256 wires as one checkpoint group. Every wire is authenticated
    /// and checked against application policy before duplicate handling. New records
    /// share one durable journal checkpoint and one custody witness publication;
    /// their exact recovery boundary is published independently before any append.
    /// Exact duplicates return their original receipts, in input order.
    /// Restriction checks use the retained history before the group. Newly learned
    /// fork evidence is retained within the same quotas; the causal projection
    /// withdraws the equivocating actor's records together when the group appears.
    /// # Errors
    /// Authority, authentication, policy, identity collisions, and aggregate capacity
    /// failures refuse before persistent mutation. Any uncertain permission, append,
    /// or custody write fences the handle; reopen and retry the exact wires.
    pub fn ingest_batch(&mut self, session: &Session<'_>, wires: &[&[u8]]) -> Result<Vec<Receipt>> {
        self.ingest_batch_via(session, wires, Via::Local)
    }

    /// [`Self::ingest_batch`] for records arriving by `via`, reported to the append sink.
    /// # Errors
    /// As [`Self::ingest_batch`].
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "JournalGroupCustody",
            action = "Checkpoint",
            project = "aterm_db::table::store::batch_conformance_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "JournalGroupCustody",
            action = "Commit",
            project = "aterm_db::table::store::batch_conformance_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "JournalGroupCustody",
            action = "Witness",
            project = "aterm_db::table::store::batch_conformance_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "JournalGroupCustody",
            action = "FinishWitness",
            project = "aterm_db::table::store::batch_conformance_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "JournalGroupCustody",
            action = "Ack",
            project = "aterm_db::table::store::batch_conformance_tests::transition"
        )
    )]
    pub fn ingest_batch_via(
        &mut self,
        session: &Session<'_>,
        wires: &[&[u8]],
        via: Via,
    ) -> Result<Vec<Receipt>> {
        session.mutation_guard()?;
        self.check(session)?;
        if wires.len() > aterm_journal::MAX_APPEND_RECORDS {
            return Err(Error::new("table ingest batch exceeds its record bound"));
        }
        let mut staged = Vec::<Prepared<'_>>::new();
        let mut positions = BTreeMap::<Digest, usize>::new();
        let mut replies = Vec::<Receipt>::with_capacity(wires.len());
        let mut notify = [false; aterm_journal::MAX_APPEND_RECORDS];
        let mut restricted = None;
        for wire in wires.iter().copied() {
            let record = session.authenticate(self.scope, wire)?;
            operation_policy(self.scope, record.header(), record.payload().len())?;
            let id = record.header().id;
            if let Some(stored) = self.records.get(&id) {
                if stored.wire.as_slice() != wire {
                    return Err(Error::new("different bytes for a retained table digest"));
                }
                replies.push(stored.receipt);
                continue;
            }
            if let Some(&position) = positions.get(&id) {
                if staged[position].wire != wire {
                    return Err(Error::new("different bytes for a proposed table digest"));
                }
                replies.push(staged[position].receipt);
                continue;
            }
            if restricted
                .get_or_insert_with(|| self.conflicted())
                .contains(&record.header().actor)
            {
                return Err(Error::new("table actor has restricted signed history"));
            }
            let position = staged.len();
            positions.insert(id, position);
            notify[replies.len()] = true;
            let receipt = Receipt {
                id,
                received_ms: 0,
                offset: self.witness.count + position as u64,
            };
            replies.push(receipt);
            staged.push(Prepared {
                wire,
                header: record.header().clone(),
                receipt,
                cut: cutoff::decode(record.payload()),
            });
        }
        if staged.is_empty() {
            return Ok(replies);
        }
        if self.log.is_none() {
            return Err(Error::new("read-only table observation cannot append"));
        }
        let mut writers = BTreeMap::<Actor, WriterHeld>::new();
        let mut total_bytes = 0usize;
        for stored in &staged {
            total_bytes = total_bytes.saturating_add(stored.wire.len());
            let held = writers.entry(stored.header.actor).or_default();
            held.records += 1;
            held.bytes = held.bytes.saturating_add(stored.wire.len());
        }
        if self.records.len().saturating_add(staged.len()) > self.capacity.operations
            || self.operation_bytes.saturating_add(total_bytes) > self.capacity.bytes
        {
            return Err(Error::repair(
                "table operation capacity exhausted; conflict space is reserved",
                aterm_types::network_state::RepairCause::Quota,
            ));
        }
        for (actor, usage) in writers {
            self.admit_records(actor, usage.records, usage.bytes)?;
        }
        self.index()?
            .check_insert_many(staged.iter().map(|stored| &stored.header))
            .map_err(|_| Error::new("table index refused batch headers or capacity"))?;
        let received_ms =
            aterm_time::unix_millis().ok_or_else(|| Error::new("receipt clock unavailable"))?;
        let mut witness = self.witness;
        let mut records = Vec::with_capacity(staged.len());
        for stored in &mut staged {
            stored.receipt.received_ms = received_ms;
            witness = witness.advance(stored.receipt.offset, stored.receipt.id, received_ms)?;
            let mut body = Vec::with_capacity(8 + stored.wire.len());
            body.extend_from_slice(&received_ms.to_le_bytes());
            body.extend_from_slice(stored.wire);
            records.push((SUBJECT, body));
        }
        // Retained receipts precede this group's physical range. Fill every proposed
        // reply, including repeated inputs, without another reply allocation.
        for receipt in &mut replies {
            if receipt.offset >= self.witness.count {
                receipt.received_ms = received_ms;
            }
        }
        session.mutation_guard()?;
        self.check(session)?;
        if self
            .log
            .as_ref()
            .ok_or_else(|| Error::new("table journal unavailable"))?
            .check_append_many(&records)?
            .0
            != self.witness.count
        {
            return Err(Error::new("table log and custody witness diverged"));
        }
        self.poisoned = true;
        #[cfg(test)]
        if APPEND_FAILS.with(std::cell::Cell::get) {
            return Err(Error::new("injected append failure"));
        }
        group::prepare(
            &self.claim_dir,
            &self.claim_name,
            &self.claim,
            self.witness,
            witness,
        )?;
        let mut claim = self.claim.clone();
        claim[WITNESS_AT..].copy_from_slice(&witness.bytes());
        let expected = self.witness.count;
        let (claim_dir, claim_name) = (&self.claim_dir, &self.claim_name);
        self.log
            .as_mut()
            .ok_or_else(|| Error::new("table journal unavailable"))?
            .append_many_then(records, claim_dir, |offset, flush| {
                #[cfg(test)]
                super::batch_conformance_tests::journal_committed();
                if offset.0 != expected {
                    return Err(std::io::Error::other(
                        "table log and custody witness diverged",
                    ));
                }
                private_file::write_private_flushed(claim_dir, claim_name, &claim, flush)?;
                #[cfg(test)]
                super::batch_conformance_tests::witness_written(
                    claim_dir, claim_name, witness, flush,
                );
                Ok(())
            })?;
        self.claim = claim;
        self.witness = witness;
        for stored in &staged {
            self.index
                .get_mut()
                .ok_or_else(|| Error::new("table index unavailable after append"))?
                .insert(stored.header.clone())
                .map_err(|_| Error::new("table index refused witnessed batch header"))?;
        }
        self.operation_bytes += total_bytes;
        // Populate the complete group before any sink can observe this store's change.
        for prepared in staged {
            let receipt = prepared.receipt;
            let stored = Stored {
                wire: StoredWire::Owned(prepared.wire.into()),
                header: prepared.header,
                receipt,
                payload: PayloadState::Ready,
                cut: prepared.cut,
            };
            if !self.hold(receipt.id, stored) {
                return Err(Error::new("duplicate witnessed batch record"));
            }
        }
        self.poisoned = false;
        self.check(session)?;
        for (position, receipt) in replies.iter().enumerate() {
            // Each new physical record notifies once, including when inputs repeat it.
            if !notify[position] {
                continue;
            }
            let stored = self
                .records
                .get(&receipt.id)
                .ok_or_else(|| Error::new("witnessed batch record unavailable"))?;
            appended(
                &Appended {
                    table: self.scope.digest(),
                    recovery: self.is_recovery(),
                    kind: AppendKind::Operation,
                    offset: receipt.offset,
                    length: witness.count,
                    via,
                    wire: stored.wire.as_slice(),
                },
                &self.claim_dir,
            );
        }
        #[cfg(test)]
        super::batch_conformance_tests::acknowledged(self.witness);
        Ok(replies)
    }
}
