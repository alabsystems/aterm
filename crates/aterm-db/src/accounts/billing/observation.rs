// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    accounts::activations::{Activations, Preparation},
    resources::{
        Allocation, Attempt, BoundRequest, Claim, Observation, RequestBinding, RequestKey,
        RequestRegistry, Resources,
    },
};
use aterm_secrets::client::{self, Config, EntrySnapshot, Paths, VerifiedEntry};
use std::time::Instant;

pub const ENTRY: &str = aterm_secrets::entry_class::AI_BILLING;

/// Configured source and account to observe. Every preparation reopens current
/// root account history and current authenticated policy bytes locally.
#[derive(Clone)]
pub struct Billing {
    paths: Paths,
    expected: Config,
    account: Digest,
}

/// One owned point-in-time observation. It is not a renewable lease, provider
/// login proof, native execution assignment or guarantee of unseen revocations.
pub struct Prepared {
    source: Billing,
    active_paths: Paths,
    snapshot: EntrySnapshot,
    preparation: Preparation,
    allocation: Allocation,
    binding: RequestBinding,
}

/// Local dispatch evidence. `entered` describes this call's trusted adapter
/// callback, not provider acceptance or billing. A withheld call stays charged
/// and unknown in the resource ledger; the refusal is a local observation only.
pub struct Dispatch {
    pub claim: Claim,
    pub attempt: Attempt,
    pub entered: bool,
    pub refusal: Option<String>,
}

impl Billing {
    pub fn at(paths: Paths, expected: Config, account: Digest) -> Self {
        Self {
            paths,
            expected,
            account,
        }
    }

    /// Match a current root account's exact billing pin to an authenticated
    /// policy and addressed credential. This initializes no allocation, makes
    /// no provider request, and grants no native launch permission.
    pub fn prepare_until(&self, deadline: Instant) -> Result<Prepared> {
        self.capture(None, deadline)
    }

    /// Bind stable logical work across allocation changes, debit once, then
    /// revalidate the exact account and credential before lending them to the
    /// trusted adapter. The callback must implement the named adapter contract;
    /// this primitive supplies no transport, endpoint or native launch rights.
    /// The broker must authenticate and retain the producer in `key` separately.
    /// A failed refresh never invokes the callback, never refunds a debit, and
    /// cannot turn the same request into a fresh attempt later.
    pub fn dispatch_until(
        &self,
        key: RequestKey,
        adapter: &str,
        payload: &[u8],
        deadline: Instant,
        run: impl FnOnce(&BoundRequest, &[u8]) -> Option<Observation>,
    ) -> Result<Dispatch> {
        self.dispatch_with_until(key, adapter, payload, deadline, || {}, run)
    }

    /// The broker pins the complete request binding before any registry write.
    /// A concurrent root revision can withhold entry, never silently broaden it.
    pub(crate) fn dispatch_pinned_until(
        &self,
        key: RequestKey,
        binding: &RequestBinding,
        payload: &[u8],
        deadline: Instant,
        run: impl FnOnce(&BoundRequest, &[u8]) -> Option<Observation>,
    ) -> Result<Dispatch> {
        let prepared = self.prepare_until(deadline)?;
        if prepared.binding() != binding {
            return Err(Error::new("billing differs from broker grant"));
        }
        self.dispatch_prepared_until(prepared, key, payload, deadline, || {}, run)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMeteredDispatch",
            action = "Fresh",
            project = "aterm_db::accounts::billing::observation::tests::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMeteredDispatch",
            action = "Retry",
            project = "aterm_db::accounts::billing::observation::tests::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMeteredDispatch",
            action = "Window",
            project = "aterm_db::accounts::billing::observation::tests::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMeteredDispatch",
            action = "Account",
            project = "aterm_db::accounts::billing::observation::tests::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMeteredDispatch",
            action = "Adapter",
            project = "aterm_db::accounts::billing::observation::tests::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMeteredDispatch",
            action = "StoppedBefore",
            project = "aterm_db::accounts::billing::observation::tests::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMeteredDispatch",
            action = "CredentialAfter",
            project = "aterm_db::accounts::billing::observation::tests::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMeteredDispatch",
            action = "StoppedAfter",
            project = "aterm_db::accounts::billing::observation::tests::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMeteredDispatch",
            action = "Panic",
            project = "aterm_db::accounts::billing::observation::tests::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMeteredDispatch",
            action = "Exhausted",
            project = "aterm_db::accounts::billing::observation::tests::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMeteredDispatch",
            action = "Succession",
            project = "aterm_db::accounts::billing::observation::tests::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMeteredDispatch",
            action = "LostRegistry",
            project = "aterm_db::accounts::billing::observation::tests::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMeteredDispatch",
            action = "Expired",
            project = "aterm_db::accounts::billing::observation::tests::dispatch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMeteredDispatch",
            action = "PendingEnrollment",
            project = "aterm_db::accounts::billing::observation::tests::dispatch_transition"
        )
    )]
    fn dispatch_with_until(
        &self,
        key: RequestKey,
        adapter: &str,
        payload: &[u8],
        deadline: Instant,
        before_refresh: impl FnOnce(),
        run: impl FnOnce(&BoundRequest, &[u8]) -> Option<Observation>,
    ) -> Result<Dispatch> {
        let prepared = self.prepare_until(deadline)?;
        if prepared.binding.adapter != adapter {
            return Err(Error::new(
                "request adapter differs from root billing policy",
            ));
        }
        self.dispatch_prepared_until(prepared, key, payload, deadline, before_refresh, run)
    }

    fn dispatch_prepared_until(
        &self,
        prepared: Prepared,
        key: RequestKey,
        payload: &[u8],
        deadline: Instant,
        before_refresh: impl FnOnce(),
        run: impl FnOnce(&BoundRequest, &[u8]) -> Option<Observation>,
    ) -> Result<Dispatch> {
        let request = prepared.request(key.ledger_id(), payload)?;
        let registry = RequestRegistry::at(self.paths.clone());
        let claim = registry
            .bind_until(key, &prepared.allocation, &request, deadline)?
            .claim;
        let resources = prepared.resources();
        let mut entered = false;
        let mut refusal = None;
        let attempt = resources.dispatch_bound_until(request, deadline, |request| {
            before_refresh();
            match prepared.refresh_until(deadline) {
                Ok(current) => {
                    entered = true;
                    run(
                        request,
                        current.preparation.credential().unwrap().value().as_slice(),
                    )
                }
                Err(why) => {
                    refusal = Some(why.to_string());
                    None
                }
            }
        })?;
        Ok(Dispatch {
            claim,
            attempt,
            entered,
            refusal,
        })
    }

    fn policy(
        &self,
        floor: Option<(&Paths, &EntrySnapshot)>,
        deadline: Instant,
    ) -> Result<(Paths, VerifiedEntry)> {
        self.current(deadline)?;
        let paths = crate::application_paths_until(&self.paths, deadline)?;
        let config = client::config(&paths)
            .map_err(crate::table::failure)?
            .ok_or_else(|| Error::new("billing vault is unavailable"))?;
        if config.name != self.expected.name || !client::pin_admitted(&config, self.expected.signer)
        {
            return Err(Error::new("billing vault has another authority"));
        }
        let entry = match floor {
            Some((previous, snapshot)) => {
                if previous != &paths {
                    return Err(Error::new("billing custody changed"));
                }
                snapshot.read(&paths)
            }
            None => client::read_verified_entry(&paths, &config, ENTRY),
        }
        .map_err(crate::table::failure)?
        .ok_or_else(|| Error::new("billing policy is unavailable or changed"))?;
        if client::config(&paths)
            .map_err(crate::table::failure)?
            .as_ref()
            != Some(&config)
            || crate::application_paths_until(&self.paths, deadline)? != paths
        {
            return Err(Error::new("billing custody changed during observation"));
        }
        self.current(deadline)?;
        Ok((paths, entry))
    }

    fn current(&self, deadline: Instant) -> Result<()> {
        if Instant::now() >= deadline {
            return Err(Error::new("billing preparation deadline expired"));
        }
        if self.expected.signer.is_none()
            || client::config(&self.paths)
                .map_err(crate::table::failure)?
                .as_ref()
                != Some(&self.expected)
        {
            return Err(Error::new("billing configured authority changed"));
        }
        Ok(())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBillingPreparation",
            action = "Valid",
            project = "aterm_db::accounts::billing::observation::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBillingPreparation",
            action = "MissingPin",
            project = "aterm_db::accounts::billing::observation::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBillingPreparation",
            action = "ChangedPolicy",
            project = "aterm_db::accounts::billing::observation::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBillingPreparation",
            action = "ChangedCredential",
            project = "aterm_db::accounts::billing::observation::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBillingPreparation",
            action = "Stopped",
            project = "aterm_db::accounts::billing::observation::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBillingPreparation",
            action = "LocalEdit",
            project = "aterm_db::accounts::billing::observation::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBillingPreparation",
            action = "Rollback",
            project = "aterm_db::accounts::billing::observation::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBillingPreparation",
            action = "Expired",
            project = "aterm_db::accounts::billing::observation::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBillingPreparation",
            action = "Unrelated",
            project = "aterm_db::accounts::billing::observation::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBillingPreparation",
            action = "ForeignRoot",
            project = "aterm_db::accounts::billing::observation::tests::transition"
        )
    )]
    fn capture(
        &self,
        floor: Option<(&Paths, &EntrySnapshot)>,
        deadline: Instant,
    ) -> Result<Prepared> {
        self.current(deadline)?;
        let accounts = Activations::at_until(self.paths.clone(), deadline);
        let (record, configuration) = accounts.current_configuration(self.account)?;
        if configuration.account()["method"].as_str() != Some("api-key") {
            return Err(Error::new("billing v1 requires an API-key account"));
        }
        let reference = Reference::parse(&configuration.account()["billing"])?;
        let provider = configuration.account()["provider"]
            .as_str()
            .ok_or_else(|| Error::new("billing account provider is missing"))?;
        let (active_paths, policy) = self.policy(floor, deadline)?;
        let (allocation, adapter) = Policy::parse(policy.value().as_slice())?.select(
            reference,
            record.trust(),
            provider,
        )?;
        let preparation = accounts.prepare(self.account, &self.expected)?;
        if preparation.decision().id() != record.id()
            || preparation.configuration().digest() != configuration.digest()
            || preparation.credential().is_none()
        {
            return Err(Error::new("billing account or credential changed"));
        }
        let (_, policy) = self.policy(Some((&active_paths, policy.snapshot())), deadline)?;
        let binding = RequestBinding {
            principal: reference.principal,
            account: self.account,
            revision: record.id(),
            // This public root record contains the sealed credential binding.
            // It is not a commitment to raw key bytes or EntrySnapshot's secret hash.
            credential: record.id(),
            policy: reference.policy,
            adapter,
        };
        Ok(Prepared {
            source: self.clone(),
            active_paths,
            snapshot: policy.snapshot().clone(),
            preparation,
            allocation,
            binding,
        })
    }
}

impl Prepared {
    pub fn preparation(&self) -> &Preparation {
        &self.preparation
    }

    pub fn allocation(&self) -> &Allocation {
        &self.allocation
    }

    pub fn binding(&self) -> &RequestBinding {
        &self.binding
    }

    /// Construct a request-count charge of exactly one. Use a fresh preparation
    /// before each effect; retaining this object does not preserve authority.
    pub fn request(&self, id: [u8; 16], payload: &[u8]) -> Result<BoundRequest> {
        BoundRequest::new(self.binding.clone(), id, 1, payload)
    }

    /// Access the selected ledger without changing its retained allocation pin.
    /// Initial enrollment is explicit; this call initializes or reserves nothing.
    pub fn resources(&self) -> Resources {
        Resources::at(self.active_paths.clone(), self.allocation.clone())
    }

    /// Consume an observation and advance its vault floor only while the exact
    /// account decision, billing resource and custody are still current.
    pub fn refresh_until(self, deadline: Instant) -> Result<Self> {
        let next = self
            .source
            .capture(Some((&self.active_paths, &self.snapshot)), deadline)?;
        if next.binding != self.binding {
            return Err(Error::new("billing preparation changed"));
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests;
