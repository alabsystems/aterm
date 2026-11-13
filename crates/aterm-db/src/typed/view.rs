// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Portable typed table snapshots; storage acquisition lives in the platform's ATT module.

use super::{
    self as typed, Collection, Table,
    cell::{self, Cells, Projection},
    grid,
    owned::{self, Op, OwnedRead, Quota, SchemaState},
};
use crate::Result;
use astream_wire::sync::Digest;
use std::collections::BTreeMap;

/// One read of a typed table, the quota applied.
#[derive(Clone, Debug)]
pub struct View {
    /// The authenticated ancestry used to distinguish an observed claim from a concurrent one.
    pub(crate) causal: astream_wire::sync::Index,
    /// Every eligible operation, past-quota ones included (the schema reader and history read
    /// them).
    pub ops: Vec<Op>,
    /// Each writer's automatic records, and the ones past the table's `self_quota`, which never
    /// project.
    pub quota: Quota,
    /// The projected cells without the past-quota records.
    pub cells: Cells,
    /// The owned collections' rows, past-quota records left out, each at its signer's own key.
    pub owned: BTreeMap<&'static str, OwnedRead>,
    /// No retained operation could still become visible here (`Log::awaited`: none waits on a
    /// causal parent that has not arrived, and this Mac's own writer has nothing withheld), so
    /// a comparison against these cells misses nothing a later projection would add.
    pub complete: bool,
    /// Retained operations this build never projects and that cannot become visible: refused
    /// payloads (a newer build's encoding, a reused request id), cut-off or forked writers'
    /// records and quarantined payloads. They never hold a grid save or an import back;
    /// `check` lists them.
    pub refused: Vec<Digest>,
}

impl View {
    /// A complete empty read for test fixtures, without opening storage or enrollment.
    #[cfg(any(test, debug_assertions))]
    pub fn empty_for_test() -> Self {
        Self {
            causal: astream_wire::sync::Index::new(crate::database::MAX_RECORDS),
            ops: Vec::new(),
            quota: Quota::default(),
            cells: Cells::new(),
            owned: BTreeMap::new(),
            complete: true,
            refused: Vec::new(),
        }
    }

    /// One cells collection's rows.
    pub fn project(&self, collection: &Collection) -> Projection {
        if collection
            .columns
            .iter()
            .any(|column| column.claim.is_some())
        {
            typed::claim::project(
                collection,
                &self.cells,
                self.ops
                    .iter()
                    .filter(|op| !self.quota.over.contains(&op.id)),
                &self.causal,
            )
        } else {
            cell::project(collection, &self.cells)
        }
    }

    /// Export canonical values, preserving original cell identities for a later grid save.
    ///
    /// # Errors
    /// The collection is not editable or the grid layout cannot be encoded.
    pub fn export_grid(
        &self,
        table: &Table,
        collection: &'static Collection,
        filters: &[grid::Where],
        reveal: bool,
        opened: Option<&grid::Opened>,
    ) -> Result<grid::Export> {
        grid::export_projected(
            table,
            collection,
            &self.cells,
            &self.project(collection),
            filters,
            reveal,
            opened,
        )
    }

    /// A roll preserves raw unknown contexts and refreshes the known columns' current claims.
    #[cfg(unix)]
    pub(crate) fn claim_provenance(&self, table: &Table) -> Result<typed::claim::CarriedClaims> {
        let mut out = typed::claim::carried_claims(&self.cells)?;
        for collection in table.collections {
            if collection
                .columns
                .iter()
                .any(|column| column.claim.is_some())
            {
                out.extend(typed::claim::snapshot(
                    collection,
                    &self.cells,
                    self.ops
                        .iter()
                        .filter(|operation| !self.quota.over.contains(&operation.id)),
                    &self.causal,
                )?);
            }
        }
        Ok(out)
    }

    /// Retirement needs the source cover and readable claim-bearing collections: opaque
    /// fields may carry identity rules whose ancestry this build cannot safely restate.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AlabIdentityClaims",
            action = "Judge",
            project = "aterm_db::typed::claim::model_tests::claim_transition"
        )
    )]
    #[cfg(any(unix, test))]
    pub(crate) fn require_roll_source(&self, table: &Table) -> Result<()> {
        if !self.complete {
            return Err(crate::Error::new(
                "the source registry has no complete generation cover; its history remains held",
            ));
        }
        for collection in table.collections {
            if collection
                .columns
                .iter()
                .any(|column| column.claim.is_some())
                && cell::project(collection, &self.cells)
                    .rows
                    .values()
                    .any(|row| !row.raw.is_empty())
            {
                return Err(crate::Error::new(
                    "an identity claim source needs a newer reader; its history remains held",
                ));
            }
        }
        Ok(())
    }

    /// The stored schema row of `collection`: the highest version any eligible operation wrote.
    pub fn schema(&self, collection: &str) -> SchemaState {
        owned::schema_state(&self.ops, collection)
    }
}
