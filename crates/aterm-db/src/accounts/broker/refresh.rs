// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Inert grant revision proposals derived from a current authenticated base.

use super::*;

/// A point-in-time draft. Neither this value nor its base fingerprint grants
/// publication or execution rights. A publisher must revalidate its own base.
pub struct Refresh {
    before: Grant,
    after: Grant,
    base: Digest,
    policy: Policy,
}

impl Refresh {
    pub fn before(&self) -> &Grant {
        &self.before
    }
    pub fn after(&self) -> &Grant {
        &self.after
    }
    /// SHA-256 of the canonical public base policy; not a signature.
    pub fn base(&self) -> Digest {
        self.base
    }
    pub fn policy(&self) -> &Policy {
        &self.policy
    }
}

impl Broker {
    /// Rebind one local grant to current billing while retaining its durable
    /// producer and every routing/lifetime choice. This creates no capability.
    pub fn refresh_proposal_until(&self, id: [u8; 16], deadline: Instant) -> Result<Refresh> {
        self.refresh_proposal_with(id, deadline, || {})
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerRefresh",
            action = "Changed",
            project = "aterm_db::accounts::billing::observation::tests::refresh_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerRefresh",
            action = "Unchanged",
            project = "aterm_db::accounts::billing::observation::tests::refresh_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerRefresh",
            action = "Absent",
            project = "aterm_db::accounts::billing::observation::tests::refresh_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerRefresh",
            action = "ForeignActor",
            project = "aterm_db::accounts::billing::observation::tests::refresh_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerRefresh",
            action = "AdapterChanged",
            project = "aterm_db::accounts::billing::observation::tests::refresh_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerRefresh",
            action = "PolicyChanged",
            project = "aterm_db::accounts::billing::observation::tests::refresh_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerRefresh",
            action = "Retry",
            project = "aterm_db::accounts::billing::observation::tests::refresh_transition"
        )
    )]
    fn refresh_proposal_with(
        &self,
        id: [u8; 16],
        deadline: Instant,
        after_billing: impl FnOnce(),
    ) -> Result<Refresh> {
        let (paths, before, floor) = {
            let mut state = self.lock_until(deadline)?;
            self.read(&mut state, &Selection::Grant(id), None, deadline)?
        };
        let prepared = Billing::at(
            self.paths.clone(),
            self.expected.clone(),
            before.binding.account,
        )
        .prepare_until(deadline)?;
        if prepared.binding().account != before.binding.account
            || prepared.binding().adapter != before.binding.adapter
        {
            return Err(Error::new(
                "grant refresh cannot change its account or adapter",
            ));
        }
        after_billing();
        let mut state = self.lock_until(deadline)?;
        let (_, policy, actor, _) =
            self.read_policy(&mut state, Some((&paths, &floor)), deadline)?;
        if policy.select(&Selection::Grant(id), actor)? != &before {
            return Err(denied());
        }
        let base = aterm_digest::Sha256::digest(policy.text().as_bytes());
        let after = Grant {
            binding: prepared.binding().clone(),
            ..before.clone()
        };
        let policy = Policy::new(
            policy
                .grants()
                .map(|grant| {
                    if grant.id == id {
                        after.clone()
                    } else {
                        grant.clone()
                    }
                })
                .collect(),
        )?;
        Ok(Refresh {
            before,
            after,
            base,
            policy,
        })
    }

    #[cfg(test)]
    pub(crate) fn test_refresh_after_billing(
        &self,
        id: [u8; 16],
        deadline: Instant,
        after: impl FnOnce(),
    ) -> Result<Refresh> {
        self.refresh_proposal_with(id, deadline, after)
    }
}
