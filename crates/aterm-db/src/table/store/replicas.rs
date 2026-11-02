// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read both independently witnessed stores without manufacturing custody.
//! Local reconciliation uses the same strict ingestion as peer repair.

use super::*;

fn proof_retained(proofs: &BTreeSet<ProofKey>, key: &ProofKey) -> bool {
    proofs.contains(key) || proofs.contains(&proof_key(key[..48].try_into().unwrap(), None))
}

impl Store {
    /// Identify the original primary enrollment while retaining both observation
    /// checks. A recovery-only replica cannot stand in for original custody.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceRecovery",
            action = "Observe",
            project = "aterm_db::resources::tests::resource_recovery_transition"
        )
    )]
    pub(crate) fn primary_custody_id(&self, session: &Session<'_>) -> Result<Digest> {
        self.check(session)?;
        match (&self.observed_primary, self.region) {
            (Some(primary), Region::Recovery) => primary.custody_id(session),
            (None, Region::Primary) => self.custody_id(session),
            _ => Err(Error::new("original primary table custody is unavailable")),
        }
    }

    /// Import all retained recovery evidence before primary resource admission.
    /// The caller holds the shared control writer throughout this copy and the
    /// subsequent admission, so repair cannot append a competing local receipt.
    /// Strict ingestion preserves negative evidence; an incomplete copy refuses
    /// admission, and no recovery file or primary enrollment is replaced.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceRecovery",
            action = "Reserve",
            project = "aterm_db::resources::tests::resource_recovery_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceRecovery",
            action = "InvalidReserve",
            project = "aterm_db::resources::tests::resource_recovery_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceRecovery",
            action = "InvalidDispatch",
            project = "aterm_db::resources::tests::resource_recovery_transition"
        )
    )]
    pub(crate) fn reconcile_recovery(&mut self, session: &Session<'_>, source: Self) -> Result<()> {
        self.check(session)?;
        source.check(session)?;
        if self.region != Region::Primary
            || source.region != Region::Recovery
            || self.scope != source.scope
            || self.log.is_none()
            || source.log.is_some()
            || self.observed_primary.is_some()
            || source.observed_primary.is_some()
            || !session.has_control_writer()
        {
            return Err(Error::new(
                "resource reconciliation requires original writer custody",
            ));
        }
        for key in source.proof_inventory(session)? {
            let wire = source
                .proof_wire(session, &key)?
                .ok_or_else(|| Error::new("recovery proof disappeared"))?;
            self.ingest_conflict(session, &wire)?;
        }
        for record in source.records.values() {
            if record.quarantine.is_some() {
                return Err(Error::new("recovery liability evidence is quarantined"));
            }
            self.ingest(session, &record.wire)?;
        }
        // Replication omits inactive proofs that may still name a valid debit.
        // Validate all evidence, including those proofs, in the combined view.
        // A parent already in primary can complete recovery's partial history.
        let primary = Self::observe(session, &self.data_dir, self.scope)?;
        source
            .with_observed_primary(session, primary)?
            .liability_evidence(session)?;
        self.check(session)
    }

    pub(crate) fn with_observed_primary(
        mut self,
        session: &Session<'_>,
        mut primary: Self,
    ) -> Result<Self> {
        self.check(session)?;
        primary.check(session)?;
        if !self.is_recovery()
            || primary.is_recovery()
            || self.scope != primary.scope
            || self.log.is_some()
            || primary.log.is_some()
            || self.observed_primary.is_some()
            || primary.observed_primary.is_some()
        {
            return Err(Error::new(
                "combined table view requires two independent read observations",
            ));
        }
        for (id, stored) in std::mem::take(&mut primary.records) {
            if let Some(existing) = self.records.get(&id) {
                if existing.wire != stored.wire || existing.quarantine != stored.quarantine {
                    return Err(Error::new(
                        "local table replicas disagree on retained evidence",
                    ));
                }
                continue;
            }
            self.admit(&stored.header, stored.wire.len())?;
            self.index
                .insert(stored.header.clone())
                .map_err(|_| Error::new("combined table index exceeds its bound"))?;
            self.operation_bytes += stored.wire.len();
            self.records.insert(id, stored);
        }
        for (key, stored) in std::mem::take(&mut primary.conflicts) {
            if self.conflicts.contains_key(&key) {
                continue;
            }
            self.admit_proof(&stored.conflict)?;
            if let Some(source) = stored.conflict.discarded() {
                self.index
                    .insert(source.header.clone())
                    .map_err(|_| Error::new("combined negative evidence exceeds its bound"))?;
            }
            self.conflicts.insert(key, stored);
        }
        // The rows moved into the combined view; this private handle retains
        // both original path/claim/witness checks and has no append handle.
        self.observed_primary = Some(Box::new(primary));
        self.check(session)?;
        Ok(self)
    }

    pub(crate) fn retains_replica_data(
        &self,
        session: &Session<'_>,
        source: &Self,
    ) -> Result<bool> {
        self.check(session)?;
        source.check(session)?;
        if self.scope != source.scope
            || self.observed_primary.is_some()
            || source.observed_primary.is_some()
        {
            return Err(Error::new(
                "replica comparison requires single stores of the same scope",
            ));
        }
        let retained: BTreeSet<_> = self.inventory(session)?.copied().collect();
        if source
            .replication_inventory(session)?
            .any(|id| !retained.contains(id))
        {
            return Ok(false);
        }
        let proofs = self.proof_inventory(session)?;
        Ok(source
            .proof_inventory(session)?
            .iter()
            .all(|key| proof_retained(&proofs, key)))
    }

    /// Copy at most `limit` proofs, then at most `limit` transferable operations.
    /// False means negative evidence remains: no positive exchange may start yet.
    pub(crate) fn retain_replica_batch(
        &mut self,
        session: &Session<'_>,
        source: &Self,
        limit: usize,
    ) -> Result<bool> {
        self.retains_replica_data(session, source)?;
        if !self.is_recovery() || self.log.is_none() || limit == 0 || limit > MAX_RECORDS {
            return Err(Error::new(
                "local retention requires a bounded recovery writer",
            ));
        }
        let retained = self.proof_inventory(session)?;
        let missing: Vec<_> = source
            .proof_inventory(session)?
            .into_iter()
            .filter(|key| !proof_retained(&retained, key))
            .take(limit + 1)
            .collect();
        for key in missing.iter().take(limit) {
            let wire = source
                .proof_wire(session, key)?
                .ok_or_else(|| Error::new("local negative evidence disappeared"))?;
            self.ingest_conflict(session, &wire)?;
        }
        if missing.len() > limit {
            return Ok(false);
        }
        let retained: BTreeSet<_> = self.inventory(session)?.copied().collect();
        let restricted = self.conflicted();
        let mut copied = 0;
        for id in source.replication_inventory(session)? {
            if copied == limit {
                break;
            }
            let record = &source.records[id];
            // A target's stronger restriction or exhausted positive quota must
            // not prevent its negative evidence reaching the next peer. Keep
            // the source unchanged; an ineligible row acquires no receipt here.
            if retained.contains(id)
                || restricted.contains(&record.header.actor)
                || self.admit(&record.header, record.wire.len()).is_err()
            {
                continue;
            }
            let wire = source
                .wire(session, id)?
                .ok_or_else(|| Error::new("local table operation disappeared"))?;
            self.ingest(session, wire)?;
            copied += 1;
        }
        Ok(true)
    }
}
