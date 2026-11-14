// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A reusable mesh entry retains configuration, never an admitted view or deadline.

use super::{Digest, Kind, MeshTableSummary, ProofKey, Replica, Result, Verdict};
use aterm_secrets::client::Paths;
use std::{collections::BTreeSet, path::PathBuf, time::Duration};

/// Reusable access to Commons tables. Each call opens a fresh admission view and
/// starts its own relative wait; retaining this object across network I/O retains
/// neither authority nor an expired absolute deadline.
#[derive(Clone, Debug)]
pub struct MeshAccess {
    paths: Paths,
    config: PathBuf,
    wait: Duration,
}

impl MeshAccess {
    /// Configure table access without opening custody or initializing a table.
    #[must_use]
    pub const fn new(paths: Paths, config: PathBuf, wait: Duration) -> Self {
        Self {
            paths,
            config,
            wait,
        }
    }

    /// The vault paths used by this entry's table and membership access.
    #[must_use]
    pub const fn paths(&self) -> &Paths {
        &self.paths
    }

    fn replica(&self) -> Result<Replica> {
        let deadline = std::time::Instant::now()
            .checked_add(self.wait)
            .ok_or_else(|| super::Error::new("mesh admission deadline exceeds this clock"))?;
        Replica::commons_until(self.paths.clone(), self.config.clone(), deadline)
    }

    /// Current table selection.
    /// # Errors
    /// Fresh admission or [`Replica::mesh_tables`] refuses.
    pub fn tables(&self) -> Result<BTreeSet<Digest>> {
        self.replica()?.mesh_tables()
    }

    /// Current table identities and proof keys under a fresh local view.
    /// # Errors
    /// Fresh admission or [`Replica::mesh_tables_summary`] refuses.
    pub fn tables_summary(&self) -> Result<Vec<MeshTableSummary>> {
        self.replica()?.mesh_tables_summary()
    }

    /// Whether the current selection takes a table, adopting its slot where allowed.
    /// # Errors
    /// Fresh admission or [`Replica::mesh_takes`] refuses.
    pub fn takes(&self, table: Digest) -> Result<bool> {
        self.replica()?.mesh_takes(table)
    }

    /// A table's current operation identities, where selected.
    /// # Errors
    /// Fresh admission or [`Replica::mesh_inventory`] refuses.
    pub fn table_inventory(&self, table: Digest) -> Result<Option<BTreeSet<Digest>>> {
        self.replica()?.mesh_inventory(table)
    }

    /// Read outgoing operations under current custody, history and key eligibility.
    /// # Errors
    /// Fresh admission or [`Replica::mesh_wires`] refuses.
    pub fn table_wires(&self, table: Digest, ids: &[Digest]) -> Result<Vec<Vec<u8>>> {
        self.replica()?.mesh_wires(table, ids)
    }

    /// A selected table's current conflict-proof keys.
    /// # Errors
    /// Fresh admission or [`Replica::mesh_proofs`] refuses.
    pub fn table_proofs(&self, table: Digest) -> Result<Option<BTreeSet<ProofKey>>> {
        self.replica()?.mesh_proofs(table)
    }

    /// Read outgoing proofs under current custody, history and key eligibility.
    /// # Errors
    /// Fresh admission or [`Replica::mesh_proof_wires`] refuses.
    pub fn table_proof_wires(&self, table: Digest, keys: &[ProofKey]) -> Result<Vec<Vec<u8>>> {
        self.replica()?.mesh_proof_wires(table, keys)
    }

    /// Admit a delivered table batch under a fresh local view.
    /// # Errors
    /// Fresh admission or [`Replica::mesh_ingest`] refuses.
    pub fn ingest_table(
        &self,
        table: Digest,
        kind: Kind,
        wires: &[Vec<u8>],
    ) -> Result<Vec<Verdict>> {
        self.replica()?.mesh_ingest(table, kind, wires)
    }
}
