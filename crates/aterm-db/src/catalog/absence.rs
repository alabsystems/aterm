// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Strict local catalog evidence for a negative application decision.

use super::*;
use crate::table::store::OwnedTable;
use aterm_digest::Sha256;
use aterm_uds::private_file::{PrivateDir, judge_private_dir};

#[cfg(test)]
thread_local! {
    pub(crate) static ABSENCE_MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// The disk-only part can be rechecked inside an already admitted EntryReader.
pub(crate) struct AbsenceInventory {
    fingerprint: Digest,
    storage: StorageInventory,
}

impl AbsenceInventory {
    pub(crate) fn fingerprint(&self) -> Digest {
        self.fingerprint
    }

    pub(crate) fn check_storage(&self) -> Result<()> {
        #[cfg(test)]
        if ABSENCE_MUTANT.get() == 3 {
            return Ok(());
        }
        if self.storage.read()? != self.storage.fingerprint {
            return Err(Error::snapshot_changed());
        }
        Ok(())
    }
}

struct StorageInventory {
    allowed: BTreeSet<Digest>,
    metadata: BTreeMap<String, usize>,
    roots: [(u8, PathBuf, bool); 4],
    fingerprint: Digest,
}

impl StorageInventory {
    fn read(&self) -> Result<Digest> {
        let mut hash = Sha256::new();
        hash.update(b"aterm/local-absence-storage/1\0");
        for (tag, root, claims) in &self.roots {
            hash.update([*tag]);
            let entries = understood_root(root, *claims, &self.allowed, &self.metadata)?;
            hash.update((entries.len() as u64).to_be_bytes());
            for (name, digest) in entries {
                hash.update((name.len() as u64).to_be_bytes());
                hash.update(name.as_bytes());
                hash.update(digest);
            }
        }
        Ok(hash.finalize())
    }
}

impl Directory {
    /// Fingerprint an understood catalog and every local storage/claim name.
    /// Opaque payloads or unexplained custody cannot establish application absence.
    /// The caller retains the catalog and control context throughout the scan.
    pub(crate) fn local_absence_inventory(
        &self,
        retained: &crate::documents::application::RetainedCatalog<'_, '_, '_, '_>,
        absent: &[OwnedTable<'_, '_, '_, '_>],
    ) -> Result<AbsenceInventory> {
        let catalog = retained.table();
        let snapshot = retained.snapshot();
        if self.is_recovery() || catalog.scope() != self.catalog_scope() || !catalog.is_at(self)? {
            return Err(Error::new("absence catalog has another local custody"));
        }
        catalog.check()?;
        let session = catalog.session();
        let store = catalog.require_store()?;
        let (records, evidence) = store.catalog_and_evidence(session)?;
        #[cfg(test)]
        let require_understood = ABSENCE_MUTANT.get() != 2;
        #[cfg(not(test))]
        let require_understood = true;
        let supplied: BTreeSet<_> = records.iter().map(|record| record.header().id).collect();
        for record in records.iter().filter(|_| require_understood) {
            Descriptor::decode(self.trust, record.payload())
                .map_err(|_| Error::new("opaque catalog record cannot establish absence"))?;
        }
        for item in evidence.iter().filter(|_| require_understood) {
            if item.cutoff {
                continue;
            }
            if let Some(record) = &item.record {
                Descriptor::decode(self.trust, record.payload())
                    .map_err(|_| Error::new("opaque catalog history cannot establish absence"))?;
            } else if !supplied.contains(&item.header.id) {
                return Err(Error::new("catalog history is unavailable for absence"));
            }
        }
        if require_understood && snapshot.inert != 0 {
            return Err(Error::new("inert catalog data cannot establish absence"));
        }
        let mut allowed = self.ring_storage(session);
        allowed.insert(self.catalog_scope().digest());
        allowed.extend(snapshot.entries.keys().copied());
        for table in absent {
            if !std::ptr::eq(catalog.context(), table.context()) || !table.is_absent() {
                return Err(Error::new(
                    "absence storage has another retained context or is present",
                ));
            }
            table.check()?;
            allowed.insert(table.scope().digest());
        }
        let mut hash = Sha256::new();
        hash.update(b"aterm/local-catalog-absence/1\0");
        let operations: Vec<_> = store.inventory(session)?.copied().collect();
        hash.update((operations.len() as u64).to_be_bytes());
        for id in operations {
            hash.update(id);
        }
        let proofs = store.proof_inventory(session)?;
        hash.update((proofs.len() as u64).to_be_bytes());
        for key in proofs {
            let wire = store
                .proof_wire(session, &key)?
                .ok_or_else(|| Error::new("catalog proof disappeared during absence scan"))?;
            hash.update(Sha256::digest(&wire));
        }
        let commons = session.paths().dir.join("commons");
        let mut metadata = super::ring::claim_metadata();
        let (clock_name, clock_limit) = crate::clock::claim_metadata();
        metadata.insert(clock_name.to_owned(), clock_limit);
        metadata
            .extend(super::release::claim_metadata().map(|(name, limit)| (name.to_owned(), limit)));
        let mut storage = StorageInventory {
            allowed,
            metadata,
            roots: [
                (2, self.primary().root, false),
                (3, self.recovery().root, false),
                (4, commons.join("table-claims"), true),
                (5, commons.join("recovery/table-claims"), true),
            ],
            fingerprint: [0; 32],
        };
        storage.fingerprint = storage.read()?;
        hash.update(storage.fingerprint);
        catalog.check()?;
        for table in absent {
            table.check()?;
        }
        Ok(AbsenceInventory {
            fingerprint: hash.finalize(),
            storage,
        })
    }
}

fn understood_root(
    root: &Path,
    claims: bool,
    allowed: &BTreeSet<Digest>,
    metadata: &BTreeMap<String, usize>,
) -> Result<BTreeMap<String, Digest>> {
    match judge_private_dir(root)? {
        PrivateDir::Missing => return Ok(BTreeMap::new()),
        PrivateDir::Private => {}
        _ => return Err(Error::new("absence storage root is not private")),
    }
    let mut found = BTreeMap::new();
    let mut volumes = Vec::new();
    let limit = if claims {
        2 * (MAX_TABLES + 1) + metadata.len()
    } else {
        MAX_TABLES + 1
    };
    for (index, entry) in std::fs::read_dir(root)?.enumerate() {
        if index >= limit {
            return Err(Error::new("absence storage inventory exceeds its bound"));
        }
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() || (claims && !kind.is_file()) || (!claims && !kind.is_dir()) {
            return Err(Error::new("unexpected storage kind prevents absence"));
        }
        let raw = entry.file_name();
        let name = raw
            .to_str()
            .ok_or_else(|| Error::new("unknown storage name prevents absence"))?;
        if claims && let Some(limit) = metadata.get(name) {
            let bytes = aterm_uds::private_file::read_private(root, name, *limit)
                .map_err(|_| Error::new("claim metadata is unavailable"))?;
            found.insert(name.to_owned(), Sha256::digest(&bytes));
            continue;
        }
        let claim = if claims {
            name.strip_suffix(".volume").unwrap_or(name)
        } else {
            name
        };
        let hex = if claims {
            claim
                .strip_suffix(".claim")
                .ok_or_else(|| Error::new("unknown claim name prevents absence"))?
        } else {
            name
        };
        let id = crate::database::decode_hex::<32>(hex)
            .map_err(|_| Error::new("unknown storage identity prevents absence"))?;
        if aterm_codec::hex::encode_lower(&id) != hex || !allowed.contains(&id) {
            #[cfg(test)]
            if ABSENCE_MUTANT.get() == 3 {
                continue;
            }
            return Err(Error::new("unexplained local custody prevents absence"));
        }
        if claims {
            let bytes = aterm_uds::private_file::read_private(root, name, 4096)
                .map_err(|_| Error::new("claim evidence is unavailable"))?;
            found.insert(name.to_owned(), Sha256::digest(&bytes));
            if name != claim {
                volumes.push(claim.to_owned());
            }
        } else {
            if !matches!(judge_private_dir(&entry.path())?, PrivateDir::Private) {
                return Err(Error::new("table storage is not private"));
            }
            found.insert(name.to_owned(), [0; 32]);
        }
    }
    if volumes.iter().any(|claim| !found.contains_key(claim)) {
        return Err(Error::new("orphan table volume evidence prevents absence"));
    }
    Ok(found)
}
