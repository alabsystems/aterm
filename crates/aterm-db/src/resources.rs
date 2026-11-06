// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fixed finite-resource allocations. A trusted activation must pin one exact
//! allocation; discovery, root signatures, names and clocks cannot choose it.

mod allocation;
pub use allocation::{Allocation, Proposal, Terms};

mod ledger;
pub use ledger::Snapshot;
mod dispatch;
pub use dispatch::{Attempt, Observation, ObservationState, Outcome};
mod request;
pub use request::{BoundRequest, RequestBinding};
mod registry;
pub use registry::{Claim, Claimed, RequestKey, RequestRegistry};
pub(crate) mod command;

use crate::{
    Error, Result,
    catalog::Descriptor,
    table::{
        Scope, Session,
        store::{Receipt, Store},
    },
};
use astream_wire::sync::Digest;
use aterm_codec::hex;
use aterm_digest::Sha256;
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap, fsx};

/// One fixed allocation, provided by trusted credential activation. This type
/// performs no provider operation and never changes the selected allocation.
pub struct Resources {
    paths: Paths,
    allocation: Allocation,
}

/// Accounting receipt only. A retry is not permission to repeat an external
/// operation. Provider idempotency and ambiguous execution need a separate guard.
pub struct Reservation {
    pub receipt: Receipt,
    pub fresh: bool,
}

impl Resources {
    pub fn at(paths: Paths, allocation: Allocation) -> Self {
        Self { paths, allocation }
    }

    /// Retain this installation's immutable pin and enroll its resource table.
    /// A conflicting pin or existing debit never authorizes a fresh allocation.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceReservation",
            action = "LostCustody",
            project = "aterm_db::resources::tests::reservation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceReservation",
            action = "Reenrolled",
            project = "aterm_db::resources::tests::reservation_transition"
        )
    )]
    pub fn initialize(&self) -> Result<Scope> {
        let pinned = self.check_existing_pin()?.is_some();
        self.with_store(!pinned, |session, store, scope| {
            session.writer()?;
            ledger::Ledger::read(&self.allocation, session, store)?;
            self.pin(session, store, true)?;
            Ok(scope)
        })
    }

    /// Read conservative locally retained accounting. This is not a global
    /// provider balance and an absent peer's unused share is never available here.
    pub fn snapshot(&self) -> Result<Snapshot> {
        self.observe_store(|session, store, _| {
            self.pin(session, store, false)?;
            Ok(ledger::Ledger::read(&self.allocation, session, store)?
                .into_snapshot(&self.allocation))
        })
    }

    /// Durably reserve this writer's units before any external work. Exact
    /// retries return the original receipt; a lost reply can strand capacity,
    /// and this API never refunds or repeats an external operation.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceReservation",
            action = "Reserve",
            project = "aterm_db::resources::tests::reservation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceReservation",
            action = "Retry",
            project = "aterm_db::resources::tests::reservation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceReservation",
            action = "WrongCost",
            project = "aterm_db::resources::tests::reservation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceReservation",
            action = "Exhausted",
            project = "aterm_db::resources::tests::reservation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceReservation",
            action = "WrongPin",
            project = "aterm_db::resources::tests::reservation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceReservation",
            action = "BadHistory",
            project = "aterm_db::resources::tests::reservation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceReservation",
            action = "NoShare",
            project = "aterm_db::resources::tests::reservation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceReservation",
            action = "TwoOffline",
            project = "aterm_db::resources::tests::reservation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceReservation",
            action = "Uninitialized",
            project = "aterm_db::resources::tests::reservation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceReservation",
            action = "NoPin",
            project = "aterm_db::resources::tests::reservation_transition"
        )
    )]
    pub fn reserve(&self, request: [u8; 16], cost: u64) -> Result<Reservation> {
        if request == [0; 16] || cost == 0 {
            return Err(Error::new(
                "resource reservation needs a nonzero request and cost",
            ));
        }
        self.with_store(false, |session, store, _| {
            session.writer()?;
            let ledger = ledger::Ledger::read(&self.allocation, session, store)?;
            if let Some(old) = ledger.request(&session.actor(), &request) {
                if old.cost != cost || old.operation.is_some() {
                    return Err(Error::new(
                        "resource request was already used with another cost or kind",
                    ));
                }
                let receipt = store
                    .receipt(session, &old.id)?
                    .ok_or_else(|| Error::new("resource receipt is unavailable"))?;
                return Ok(Reservation {
                    receipt,
                    fresh: false,
                });
            }
            let debit = self.new_debit(session, &ledger, request, cost)?;
            let receipt = store.commit_liability(session, &debit.encode())?;
            Ok(Reservation {
                receipt,
                fresh: true,
            })
        })
    }

    fn new_debit(
        &self,
        session: &Session<'_>,
        ledger: &ledger::Ledger,
        request: [u8; 16],
        cost: u64,
    ) -> Result<ledger::Debit> {
        let limit = self
            .allocation
            .share(&session.actor())
            .ok_or_else(|| Error::new("this writer has no fixed resource share"))?;
        let previous = ledger.used(&session.actor());
        let cumulative = previous
            .checked_add(cost)
            .ok_or_else(|| Error::new("resource reservation overflows"))?;
        if !aterm_db_core::admissible(limit, previous, cumulative) {
            return Err(Error::new(
                "this writer's fixed resource share is exhausted",
            ));
        }
        Ok(ledger::Debit {
            allocation: self.allocation.id(),
            request,
            cost,
            cumulative,
        })
    }

    fn with_store<T>(
        &self,
        create: bool,
        action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
    ) -> Result<T> {
        self.with_store_deadline(create, None, action)
    }

    fn with_store_deadline<T>(
        &self,
        create: bool,
        deadline: Option<std::time::Instant>,
        action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
    ) -> Result<T> {
        let descriptor = |bootstrap: &Bootstrap| {
            if bootstrap.trust() != self.allocation.trust() {
                return Err(Error::new(
                    "resource activation belongs to another Commons root",
                ));
            }
            descriptor(bootstrap, &self.allocation)
        };
        let apply = |session: &Session<'_>, store: &mut Store, scope: Scope| {
            let name = self.pin_name();
            let existing = self.check_existing_pin_named(&name)?;
            if let Some(pinned) = existing
                && pinned != store.primary_custody_id(session)?
            {
                return Err(Error::new(
                    "resource table custody differs from its retained activation pin",
                ));
            }
            let directory = crate::catalog::Directory::new(session.paths(), scope.trust());
            if let Some(recovery) = directory
                .recovery()
                .observe_store_if_exists(session, scope)?
            {
                store.reconcile_recovery(session, recovery)?;
            }
            if !create && existing.is_none() {
                store.primary_custody_id(session)?;
                return Err(Error::new(
                    "resource activation pin is not retained locally",
                ));
            }
            action(session, store, scope)
        };
        match deadline {
            Some(deadline) => crate::documents::application::with_store_until(
                &self.paths,
                create,
                descriptor,
                deadline,
                apply,
            ),
            None => {
                crate::documents::application::with_store(&self.paths, create, descriptor, apply)
            }
        }
    }

    fn observe_store<T>(
        &self,
        action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
    ) -> Result<T> {
        crate::documents::application::observe_store(
            &self.paths,
            |bootstrap| {
                if bootstrap.trust() != self.allocation.trust() {
                    return Err(Error::new(
                        "resource activation belongs to another Commons root",
                    ));
                }
                descriptor(bootstrap, &self.allocation)
            },
            action,
        )
    }

    fn pin_name(&self) -> String {
        hex::encode_lower(&window_id(&self.allocation))
    }
    fn pin_path_named(&self, paths: &Paths, name: &str) -> std::path::PathBuf {
        paths.dir.join("commons/resource-pins").join(name)
    }
    #[cfg(test)]
    fn pin_path(&self) -> std::path::PathBuf {
        self.pin_path_named(&self.paths, &self.pin_name())
    }
    fn check_existing_pin(&self) -> Result<Option<Digest>> {
        self.check_existing_pin_named(&self.pin_name())
    }
    fn check_existing_pin_named(&self, name: &str) -> Result<Option<Digest>> {
        // Succession changes writer custody, not the allocation for an already
        // pinned resource window. Ancestor pins only constrain identity; they
        // never supply the successor's local custody or an inherited share.
        for predecessor in crate::generation::predecessors(&self.paths)? {
            self.check_pin_at(&self.pin_path_named(&predecessor, name))?;
        }
        self.check_pin_at(&self.pin_path_named(&self.paths, name))
    }
    fn check_pin_at(&self, path: &std::path::Path) -> Result<Option<Digest>> {
        let Some(bytes) = fsx::read_private(path, 69).map_err(crate::table::failure)? else {
            return Ok(None);
        };
        let bytes = bytes.as_slice();
        if bytes.len() == 37 && bytes.starts_with(b"ARAP1") {
            return Err(Error::new(
                "legacy resource pin has no custody binding; audited migration is required",
            ));
        }
        if bytes.len() != 69 || !bytes.starts_with(b"ARAP2") || bytes[5..37] != self.allocation.id()
        {
            return Err(Error::new(
                "this resource window already has another retained allocation pin",
            ));
        }
        Ok(Some(bytes[37..].try_into().unwrap()))
    }
    fn pin_bytes(&self, custody: Digest) -> [u8; 69] {
        let mut bytes = [0u8; 69];
        bytes[..5].copy_from_slice(b"ARAP2");
        bytes[5..37].copy_from_slice(&self.allocation.id());
        bytes[37..69].copy_from_slice(&custody);
        bytes
    }
    fn pin(&self, session: &Session<'_>, store: &Store, create: bool) -> Result<()> {
        let name = self.pin_name();
        let path = self.pin_path_named(&self.paths, &name);
        let custody = store.primary_custody_id(session)?;
        let pinned = match self.check_existing_pin_named(&name)? {
            Some(pinned) => pinned,
            None => {
                if !create {
                    return Err(Error::new(
                        "resource activation pin is not retained locally",
                    ));
                }
                let dir = path
                    .parent()
                    .ok_or_else(|| Error::new("invalid resource pin directory"))?;
                aterm_journal::create_private_dir_durable(dir)?;
                fsx::write_new(&path, &self.pin_bytes(custody), 0o600)
                    .map_err(crate::table::failure)?;
                self.check_existing_pin_named(&name)?
                    .ok_or_else(|| Error::new("resource activation pin was not retained"))?
            }
        };
        if pinned != custody {
            return Err(Error::new(
                "resource table custody differs from its retained activation pin",
            ));
        }
        if create {
            // Finish a possible earlier link whose directory barrier failed.
            aterm_durable::durable_dir(path.parent().unwrap())?;
        }
        Ok(())
    }
}

fn window_id(allocation: &Allocation) -> Digest {
    let trust = allocation.trust();
    let mut hash = Sha256::new();
    hash.update(b"aterm/commons/resource-window/1\0");
    hash.update(trust.network);
    hash.update(trust.authority);
    hash.update(trust.genesis);
    hash.update(allocation.terms().resource);
    hash.update(allocation.terms().window);
    hash.finalize()
}
fn descriptor(bootstrap: &Bootstrap, allocation: &Allocation) -> Result<Descriptor> {
    Descriptor::application(
        bootstrap.trust(),
        "resources",
        &hex::encode_lower(&window_id(allocation)),
        1,
    )
}

#[cfg(test)]
mod tests;
