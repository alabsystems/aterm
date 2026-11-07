// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A generation-derived native grant publication before any canonical image write.

use super::{
    super::codex_enrollment::{Phase, Request},
    super::codex_grant_owner::{GrantBinding, RetainedGrant},
    Logins, Store,
};
use crate::harness::codex_native_row::{RecordPublicationReceipt, RowPublicationReceipt};
use std::cell::RefCell;
use std::sync::Arc;

/// Transaction identity, never serialized data or grant authority.
#[derive(Clone, Debug)]
pub(in crate::harness) struct ActualPublicationStamp(Arc<()>);

impl ActualPublicationStamp {
    pub(in crate::harness) fn same_transaction(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Created only by the canonical writer for its one publication callback.
pub(super) struct PublicationChannel {
    stamp: ActualPublicationStamp,
    receipt: RefCell<Option<RecordPublicationReceipt>>,
}

impl PublicationChannel {
    pub(super) fn new() -> Self {
        Self {
            stamp: ActualPublicationStamp(Arc::new(())),
            receipt: RefCell::new(None),
        }
    }

    pub(super) fn into_receipt(self) -> Option<RecordPublicationReceipt> {
        self.receipt.into_inner()
    }
}

/// No constructor outside the real account transaction, no serialization or clone.
/// The native owner still verifies source/launch custody and the exact fresh doc.
pub(in crate::harness) struct ActualPublicationPermit<'a> {
    store: &'a Store,
    before: &'a Logins,
    after: &'a Logins,
    channel: &'a PublicationChannel,
}
impl<'a> ActualPublicationPermit<'a> {
    pub(super) fn new(
        store: &'a Store,
        before: &'a Logins,
        after: &'a Logins,
        channel: &'a PublicationChannel,
    ) -> Self {
        Self {
            store,
            before,
            after,
            channel,
        }
    }

    pub(in crate::harness) fn owns_operations(
        &self,
        operations: &crate::harness::codex_enrollment::Operations,
    ) -> bool {
        std::ptr::eq(&self.after.enrollment, operations)
    }

    pub(in crate::harness) fn require_staging_store(
        &self,
        staging: &crate::harness::codex_enrollment::Staging,
    ) -> Result<(), String> {
        let directory = std::fs::canonicalize(&self.store.dir)
            .map_err(|_| "native publication store unavailable")?;
        let identity = crate::harness::codex_enrollment::DirectoryIdentity::capture(&directory)?;
        if self.store.cli != super::Cli::Codex
            || directory != staging.store
            || staging.store_identity.as_ref() != Some(&identity)
            || !staging.directories_current()
        {
            return Err("native publication store does not match issuer".into());
        }
        Ok(())
    }

    pub(in crate::harness) fn stamp(&self) -> ActualPublicationStamp {
        self.channel.stamp.clone()
    }

    pub(in crate::harness) fn attach_row_receipt(
        &self,
        receipt: RowPublicationReceipt,
    ) -> Result<(), String> {
        if !receipt.stamp().same_transaction(&self.channel.stamp) {
            return Err("native row receipt belongs to another transaction".into());
        }
        let mut slot = self
            .channel
            .receipt
            .try_borrow_mut()
            .map_err(|_| "native row receipt channel is busy")?;
        if slot.is_some() {
            return Err("native row receipt channel is already complete".into());
        }
        *slot = Some(receipt.into_record_proof(&self.channel.stamp)?);
        Ok(())
    }

    pub(in crate::harness) fn after_binding_digest(
        &self,
        request: &Request,
        fresh: &RetainedGrant,
    ) -> Result<String, String> {
        self.binding(request, fresh)?;
        crate::harness::codex_native_row::binding_digest(self.after, &request.label)
    }

    pub(in crate::harness) fn binding(
        &self,
        request: &Request,
        fresh: &RetainedGrant,
    ) -> Result<GrantBinding, String> {
        if !fresh.matches_request(request)
            || !self
                .before
                .enrollment
                .lookup(request)?
                .is_some_and(|status| status.phase == Phase::Running)
        {
            return Err("native publication is not a fresh admitted completion".into());
        }
        let generation = self
            .after
            .get(&request.label)
            .map(|bound| bound.last_success_generation)
            .filter(|generation| *generation > 0)
            .ok_or("native publication generation unavailable")?;
        let binding = self
            .after
            .enrollment
            .provisional_published_for_permit(&request.label, generation, self)
            .filter(|binding| binding.request == *request && binding.reference == fresh.reference())
            .ok_or("native publication has no matching verified receipt")?;
        if self
            .after
            .enrollment
            .provisional_owner_for_permit(&binding, self)?
            .is_some()
        {
            return Err("native publication already has an owner anchor".into());
        }
        Ok(binding)
    }
}

#[cfg(test)]
mod tests {
    use crate::harness::accounts::{self, Cli, Store};
    use std::cell::Cell;

    #[test]
    fn derived_generation_hook_precedes_canonical_write_and_failure_preserves_old_binding() {
        let world = aterm_tempfile::tempdir().unwrap();
        let home = world.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let store = Store::at_for(world.path(), Cli::Codex, &home);
        let old_dir = world.path().join("old");
        let new_dir = world.path().join("new");
        let now = 1_800_000_000;
        store
            .transact_logins(now, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "work",
                    None,
                    old_dir.to_str().unwrap(),
                    None,
                    now,
                ))
            })
            .unwrap();
        let path = world.path().join("roster.toml");
        std::fs::write(
            &path,
            "[[account]]\nlabel = 'work'\nemail = 'work@example.test'\nclass = 'dedicated'\n",
        )
        .unwrap();
        let (before, _) = store.load_logins(now).unwrap();
        let old_generation = before.get("work").unwrap().last_success_generation;
        let old_image = std::fs::read(store.logins_file()).unwrap();
        let called = Cell::new(0);
        let attempt = |refuse: bool| {
            store.transact_roster_logins_native_within(
                &[],
                &path,
                now + 1,
                &mut None,
                |_, state, logins| {
                    Ok(accounts::note_renewed(
                        state,
                        logins,
                        "work",
                        None,
                        new_dir.to_str().unwrap(),
                        None,
                        now + 1,
                    ))
                },
                |permit| {
                    called.set(called.get() + 1);
                    assert_eq!(
                        permit.after.get("work").unwrap().last_success_generation,
                        old_generation + 1
                    );
                    assert_eq!(
                        permit.before.get("work").unwrap().last_success_generation,
                        old_generation
                    );
                    let (on_disk, _) = store.load_logins(now + 1).unwrap();
                    assert_eq!(
                        on_disk.get("work").unwrap().last_success_generation,
                        old_generation
                    );
                    if refuse {
                        Err("native owner baseline unavailable".into())
                    } else {
                        Ok(None)
                    }
                },
            )
        };
        assert!(attempt(true).is_err());
        assert_eq!(std::fs::read(store.logins_file()).unwrap(), old_image);
        attempt(false).unwrap();
        assert_eq!(called.get(), 2);
        let (published, _) = store.load_logins(now + 1).unwrap();
        assert_eq!(
            published.get("work").unwrap().last_success_generation,
            old_generation + 1
        );
        assert_eq!(
            published.get("work").unwrap().dir.as_deref(),
            new_dir.to_str()
        );
    }
}
