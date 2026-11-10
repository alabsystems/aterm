// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Canonical high-water evidence for one global native issuer catalogue entry.
//! Serialized witnesses are correlation data, never a transition capability.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::harness) enum CataloguePhase {
    Prepared,
    Attempted,
    Received,
    Quarantined,
    NotDispatched,
    Rejected,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::harness) struct CatalogueWitness {
    pub(in crate::harness) namespace: String,
    pub(in crate::harness) sequence: u64,
    pub(in crate::harness) digest: String,
    pub(in crate::harness) phase: CataloguePhase,
}
impl std::fmt::Debug for CatalogueWitness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CatalogueWitness(redacted)")
    }
}
impl CatalogueWitness {
    pub(in crate::harness) fn valid(&self) -> bool {
        super::hex(&self.namespace, 64) && self.sequence > 0 && super::hex(&self.digest, 64)
    }
}
pub(in crate::harness) struct CatalogueAnchorWriter<'a> {
    store: &'a super::accounts::Store,
    held: &'a super::accounts::Lock,
    request: &'a super::Request,
    now: i64,
}
impl<'a> CatalogueAnchorWriter<'a> {
    pub(in crate::harness) fn new(
        store: &'a super::accounts::Store,
        held: &'a super::accounts::Lock,
        request: &'a super::Request,
        now: i64,
    ) -> Self {
        Self {
            store,
            held,
            request,
            now,
        }
    }
    pub(in crate::harness) fn read(&self) -> Result<Option<CatalogueWitness>, String> {
        let (logins, warning) = self.store.load_logins(self.now)?;
        if warning.is_some() {
            return Err("catalogue anchor unavailable".into());
        }
        logins.enrollment.catalogue_witness(self.request)
    }
    pub(in crate::harness) fn compare_exchange(
        &mut self,
        transition: &super::super::codex_grant_owner::CatalogueTransition,
    ) -> Result<(), String> {
        if transition.request() != self.request
            || transition.staging().store
                != std::fs::canonicalize(&self.store.dir)
                    .map_err(|_| "catalogue source unavailable")?
            || !transition.staging().store_current()
        {
            return Err("catalogue anchor belongs to another source".into());
        }
        self.store
            .transact_logins_held(self.held, self.now, &mut None, |_, logins| {
                logins.enrollment.cas_catalogue_witness(transition)?;
                Ok(Vec::new())
            })?;
        Ok(())
    }
}

/// A pure canonical observation for catalogue verification. This grants no
/// credential read, publication, source, or provider-use capability.
pub(in crate::harness) struct CatalogueRecordState {
    pub(in crate::harness) witness: Option<CatalogueWitness>,
    pub(in crate::harness) reference: Option<super::super::codex_grant_owner::GrantReference>,
    pub(in crate::harness) published: Option<super::super::codex_grant_owner::GrantBinding>,
    pub(in crate::harness) owner_witness: Option<super::OwnerWitness>,
    pub(in crate::harness) publication:
        Option<super::super::codex_native_row::PublicationReceiptData>,
}
