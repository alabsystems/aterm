// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Owned adapter inputs bound to durable admission. These types verify neither
//! provider identity nor authority; their caller must establish both first.

use super::{Allocation, Attempt, Observation, Resources};
use crate::{Error, Result};
use astream_wire::sync::Digest;
use aterm_digest::Sha256;
use aterm_uds::secret::Secret;

const MAX_PAYLOAD: usize = 1024 * 1024;

/// Already authenticated identities supplied by a trusted adapter. Constructing
/// this value authenticates nothing. A principal identifies shared capacity,
/// including all its account aliases, and must equal the allocation's resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestBinding {
    pub principal: Digest,
    pub account: Digest,
    /// Exact current account authorization, not a mutable account label.
    pub revision: Digest,
    /// Authenticated credential revision identity, never a hash of raw key bytes.
    pub credential: Digest,
    /// Exact authenticated billing policy that selected principal and allocation.
    pub policy: Digest,
    /// Versioned adapter contract defining every effect-relevant payload byte.
    pub adapter: String,
}

/// One bounded, immutable adapter request. Its body may be private, so it is
/// wiped on drop and omitted from diagnostics. The payload must encode every
/// effect-relevant input, including route, model, headers and output limits.
/// Neither validation nor a digest can establish those adapter semantics.
pub struct BoundRequest {
    binding: RequestBinding,
    request: [u8; 16],
    cost: u64,
    payload: Secret,
}

impl BoundRequest {
    /// Copy exact adapter inputs after verifying their structural bounds. Cost
    /// is an already established conservative maximum in the allocation's unit;
    /// this function performs no pricing, token counting or authority check.
    pub fn new(
        binding: RequestBinding,
        request: [u8; 16],
        cost: u64,
        payload: &[u8],
    ) -> Result<Self> {
        if [
            binding.principal,
            binding.account,
            binding.revision,
            binding.credential,
            binding.policy,
        ]
        .contains(&[0; 32])
            || request == [0; 16]
            || cost == 0
            || binding.adapter.len() > 64
            || !crate::crypto::name(&binding.adapter)
            || payload.len() > MAX_PAYLOAD
        {
            return Err(Error::new("invalid or oversized bound adapter request"));
        }
        Ok(Self {
            binding,
            request,
            cost,
            payload: Secret::new(payload),
        })
    }

    pub fn binding(&self) -> &RequestBinding {
        &self.binding
    }

    pub const fn id(&self) -> [u8; 16] {
        self.request
    }

    pub const fn cost(&self) -> u64 {
        self.cost
    }

    pub fn payload(&self) -> &[u8] {
        self.payload.expose()
    }

    pub(crate) fn operation(&self, allocation: &Allocation) -> Result<Digest> {
        if self.binding.principal != allocation.terms().resource {
            return Err(Error::new(
                "adapter principal differs from allocated resource",
            ));
        }
        let mut hash = Sha256::new();
        hash.update(b"aterm/commons/bound-adapter-request/1\0");
        // Allocation identity includes the Commons root, window, unit and shares.
        hash.update(allocation.id());
        hash.update(self.request);
        hash.update(self.cost.to_be_bytes());
        for identity in [
            self.binding.principal,
            self.binding.account,
            self.binding.revision,
            self.binding.credential,
            self.binding.policy,
        ] {
            hash.update(identity);
        }
        for bytes in [self.binding.adapter.as_bytes(), self.payload()] {
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        }
        Ok(hash.finalize())
    }
}

impl Resources {
    /// Bind exact owned adapter inputs to this allocation before durable
    /// admission, then lend those same inputs to one callback. Exact retries
    /// only return retained state; changing any bound field refuses the retry.
    ///
    /// The trusted caller must first authenticate the binding, coherently pin
    /// the allocation across all aliases, bound the complete request's cost,
    /// and recheck current authority before any credential use. This helper
    /// neither loads a credential nor makes a provider request. It inherits
    /// `dispatch_once`'s writer-scoped retry and ambiguous-result semantics.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBoundRequest",
            action = "Fresh",
            project = "aterm_db::resources::request::tests::bound_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBoundRequest",
            action = "Retry",
            project = "aterm_db::resources::request::tests::bound_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBoundRequest",
            action = "Principal",
            project = "aterm_db::resources::request::tests::bound_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBoundRequest",
            action = "Account",
            project = "aterm_db::resources::request::tests::bound_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBoundRequest",
            action = "Revision",
            project = "aterm_db::resources::request::tests::bound_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBoundRequest",
            action = "Credential",
            project = "aterm_db::resources::request::tests::bound_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBoundRequest",
            action = "Policy",
            project = "aterm_db::resources::request::tests::bound_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBoundRequest",
            action = "Adapter",
            project = "aterm_db::resources::request::tests::bound_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBoundRequest",
            action = "Payload",
            project = "aterm_db::resources::request::tests::bound_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBoundRequest",
            action = "Cost",
            project = "aterm_db::resources::request::tests::bound_transition"
        )
    )]
    pub fn dispatch_bound(
        &self,
        request: BoundRequest,
        run: impl FnOnce(&BoundRequest) -> Option<Observation>,
    ) -> Result<Attempt> {
        let operation = request.operation(&self.allocation)?;
        self.dispatch_once(request.id(), request.cost(), operation, || run(&request))
    }

    /// The same exact-input dispatch with a caller-owned admission deadline.
    /// The callback receives the captured request only after a durable debit.
    pub fn dispatch_bound_until(
        &self,
        request: BoundRequest,
        deadline: std::time::Instant,
        run: impl FnOnce(&BoundRequest) -> Option<Observation>,
    ) -> Result<Attempt> {
        let operation = request.operation(&self.allocation)?;
        self.dispatch_once_until(request.id(), request.cost(), operation, deadline, || {
            run(&request)
        })
    }
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
