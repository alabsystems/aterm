// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Strict catalog evidence for a retained application admission.

use super::*;

impl Directory {
    /// Project a supplied primary catalog only when every locally retained
    /// operation has authenticated payload and complete, unforked ancestry.
    /// Discovery may retain useful partial positives; receipt admission cannot
    /// let omitted declarations select an older application generation.
    ///
    /// The caller holds this catalog's data guard. This checks one retained
    /// context, never reopens storage or falls back to recovery, and
    /// makes no claim about history this replica has not received. Invalid
    /// authenticated descriptor payloads remain known inert catalog data.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbCatalogAdmission",
            action = "Case",
            reason = "The environment selects retained catalog evidence and custody."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCatalogAdmission",
            action = "Read",
            project = "aterm_db::catalog::admission::tests::transition"
        )
    )]
    pub(crate) fn snapshot_from_primary(
        &self,
        session: &Session<'_>,
        catalog: &Store,
    ) -> Result<Snapshot> {
        #[cfg(test)]
        let check_location = MUTANT.get() != 3;
        #[cfg(not(test))]
        let check_location = true;
        if self.is_recovery()
            || catalog.is_recovery()
            || catalog.scope() != self.catalog_scope()
            || (check_location && !catalog.is_primary_at(&self.path(self.catalog_scope())?))
        {
            return Err(Error::new(
                "primary catalog observation has another custody",
            ));
        }
        self.project_strict_store(session, catalog, self.retained()?)
    }

    /// Project complete evidence while the owned view retains both local regions.
    /// An absent catalog is unavailable, never an authenticated empty catalog.
    pub(crate) fn snapshot_from_owned(
        &self,
        catalog: &crate::table::store::OwnedTable<'_, '_, '_, '_>,
    ) -> Result<Snapshot> {
        if self.is_recovery() || catalog.scope() != self.catalog_scope() || !catalog.is_at(self)? {
            return Err(Error::new("local catalog observation has another custody"));
        }
        catalog.check()?;
        let session = catalog.session();
        let store = catalog.require_store()?;
        // With no recovery root, the exact-primary contract is the whole view.
        let snapshot = if catalog.only_primary() && !self.recovery().root.try_exists()? {
            self.snapshot_from_primary(session, store)?
        } else {
            self.project_strict_store(session, store, self.retained_local()?)?
        };
        catalog.check()?;
        Ok(snapshot)
    }

    fn project_strict_store(
        &self,
        session: &Session<'_>,
        catalog: &Store,
        (retained, count): (BTreeSet<Digest>, usize),
    ) -> Result<Snapshot> {
        let (mut records, evidence) = catalog.catalog_and_evidence(session)?;
        // catalog_and_evidence moves eligible application records out of the
        // metadata entries. Their None is consumption, not missing ciphertext.
        let supplied: BTreeSet<_> = records.iter().map(|record| record.header().id).collect();
        #[cfg(test)]
        let require_complete = MUTANT.get() != 1;
        #[cfg(not(test))]
        let require_complete = true;
        if require_complete {
            if !catalog.catalog_evidence_covers_retained(session, &evidence)? {
                return Err(Error::new("catalog admission history is incomplete"));
            }
            let mut index = astream_wire::sync::Index::new(crate::table::store::MAX_RECORDS * 2);
            let mut ids = BTreeSet::new();
            for item in &evidence {
                if item.record.is_none() && !supplied.contains(&item.header.id) {
                    return Err(Error::new("catalog admission payload is unavailable"));
                }
                index
                    .insert(item.header.clone())
                    .map_err(|_| Error::new("catalog admission history exceeds its bound"))?;
                ids.insert(item.header.id);
            }
            if index.project().ordered.len() != ids.len() {
                return Err(Error::new(
                    "catalog admission ancestry is incomplete or forked",
                ));
            }
            // A demoted writer's declarations still describe retained tables.
            // Preserve discovery's historical access to those slots; a hidden
            // declaration for an unretained slot cannot authorize a fallback.
            for item in evidence {
                if item.catalog_eligible || item.cutoff {
                    continue;
                }
                let record = item
                    .record
                    .ok_or_else(|| Error::new("catalog admission payload is unavailable"))?;
                let Ok(descriptor) = Descriptor::decode(self.trust, record.payload()) else {
                    continue;
                };
                if !retained.contains(&descriptor.scope.digest()) {
                    return Err(Error::new(
                        "catalog has an unretained historical declaration",
                    ));
                }
                records.push(record);
            }
        }
        #[cfg(test)]
        if MUTANT.get() == 2 {
            return Err(Error::new("injected complete catalog refusal"));
        }
        self.project_retained(session, records, &Policy::default(), &retained, count)
    }
}

#[cfg(test)]
thread_local! {
    static MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;
