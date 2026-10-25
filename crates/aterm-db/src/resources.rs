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
    pub fn initialize(&self) -> Result<Scope> {
        self.check_existing_pin()?;
        self.with_store(true, |session, store, scope| {
            session.writer()?;
            ledger::Ledger::read(&self.allocation, session, store)?;
            self.pin(true)?;
            Ok(scope)
        })
    }

    /// Read conservative locally retained accounting. This is not a global
    /// provider balance and an absent peer's unused share is never available here.
    pub fn snapshot(&self) -> Result<Snapshot> {
        self.with_store(false, |session, store, _| {
            self.pin(false)?;
            Ok(ledger::Ledger::read(&self.allocation, session, store)?.snapshot(&self.allocation))
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
            self.pin(false)?;
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
            let receipt = store.commit_catalog(session, &debit.encode())?;
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
        crate::documents::application::with_store(
            &self.paths,
            create,
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

    fn pin_path(&self) -> std::path::PathBuf {
        self.paths
            .dir
            .join("commons/resource-pins")
            .join(hex::encode_lower(&window_id(&self.allocation)))
    }
    fn check_existing_pin(&self) -> Result<()> {
        if let Some(bytes) =
            fsx::read_private(&self.pin_path(), 37).map_err(crate::table::failure)?
            && bytes.as_slice() != self.pin_bytes()
        {
            return Err(Error::new(
                "this resource window already has another retained allocation pin",
            ));
        }
        Ok(())
    }
    fn pin_bytes(&self) -> Vec<u8> {
        let mut bytes = b"ARAP1".to_vec();
        bytes.extend_from_slice(&self.allocation.id());
        bytes
    }
    fn pin(&self, create: bool) -> Result<()> {
        let path = self.pin_path();
        self.check_existing_pin()?;
        if fsx::read_private(&path, 37)
            .map_err(crate::table::failure)?
            .is_none()
        {
            if !create {
                return Err(Error::new(
                    "resource activation pin is not retained locally",
                ));
            }
            let dir = path
                .parent()
                .ok_or_else(|| Error::new("invalid resource pin directory"))?;
            aterm_journal::create_private_dir_durable(dir)?;
            fsx::write_new(&path, &self.pin_bytes(), 0o600).map_err(crate::table::failure)?;
            aterm_durable::durable_dir(dir)?;
        }
        let bytes = fsx::read_private(&path, 37)
            .map_err(crate::table::failure)?
            .ok_or_else(|| Error::new("resource activation pin was not retained"))?;
        if bytes.as_slice() != self.pin_bytes() {
            return Err(Error::new("resource activation pin changed"));
        }
        Ok(())
    }
}

fn window_id(allocation: &Allocation) -> Digest {
    let trust = allocation.trust();
    let mut bytes = b"aterm/commons/resource-window/1\0".to_vec();
    bytes.extend_from_slice(&trust.network);
    bytes.extend_from_slice(&trust.authority);
    bytes.extend_from_slice(&trust.genesis);
    bytes.extend_from_slice(&allocation.terms().resource);
    bytes.extend_from_slice(&allocation.terms().window);
    Sha256::digest(&bytes)
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
