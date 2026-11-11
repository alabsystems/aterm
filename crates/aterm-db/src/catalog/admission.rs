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
            let (retained, _) = self.retained()?;
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
        self.project(session, records, &Policy::default())
    }
}

#[cfg(test)]
thread_local! {
    static MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;
