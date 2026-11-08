// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit root publication of a reviewed, identity-preserving refresh.

use super::*;

#[derive(Debug)]
pub enum Publication {
    /// The complete requested policy is already the authenticated current one.
    AlreadyCurrent { sequence: u64 },
    /// At least one receiving store accepted the conditional envelope.
    Published(Box<client::Published>),
}

impl Broker {
    pub(super) fn check_publication_writer_until(
        &self,
        id: [u8; 16],
        deadline: Instant,
    ) -> Result<()> {
        let mut state = self.lock_until(deadline)?;
        let (_, policy, actor, _) = self.read_policy(&mut state, None, deadline)?;
        policy.select(&Selection::Grant(id), actor)?;
        Ok(())
    }

    /// Apply exactly the reviewed refresh, without rebasing or changing any
    /// other grant. Root admin custody and current local-writer checks are both
    /// required. Each receiving store compares the signed opened parent.
    ///
    /// # Errors
    /// A stale or mismatched policy, unsupported routing change, lost authority,
    /// changed custody, or conditional publication failure. Network failure may
    /// follow storage; refresh the reader and admin snapshot before retrying the
    /// same target and base. A no-op rechecks billing and custody, but cannot
    /// authenticate a historical base whose text is no longer in this snapshot.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPublication",
            action = "Apply",
            project = "aterm_db::accounts::billing::observation::tests::publication_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPublication",
            action = "Repeat",
            project = "aterm_db::accounts::billing::observation::tests::publication_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPublication",
            action = "Producer",
            project = "aterm_db::accounts::billing::observation::tests::publication_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPublication",
            action = "OtherGrant",
            project = "aterm_db::accounts::billing::observation::tests::publication_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPublication",
            action = "StaleBase",
            project = "aterm_db::accounts::billing::observation::tests::publication_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPublication",
            action = "MovedHead",
            project = "aterm_db::accounts::billing::observation::tests::publication_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPublication",
            action = "ForeignActor",
            project = "aterm_db::accounts::billing::observation::tests::publication_transition"
        )
    )]
    pub fn publish_refresh_until(
        &self,
        id: [u8; 16],
        base: Digest,
        target: &Policy,
        admin: &client::Admin,
        deadline: Instant,
    ) -> Result<Publication> {
        let (paths, current, actor, _) = {
            let mut state = self.lock_until(deadline)?;
            self.read_policy(&mut state, None, deadline)?
        };
        current.select(&Selection::Grant(id), actor)?;
        if client::config(&paths)
            .map_err(crate::table::failure)?
            .as_ref()
            != Some(&admin.config)
        {
            return Err(denied());
        }
        // Never let edits to Admin::doc, another audience, or a same-name entry
        // in a different audience become an implicit part of this operation.
        let mut entries = admin
            .published_document()
            .entries()
            .into_iter()
            .filter(|entry| entry.name == ENTRY);
        let entry = entries.next().ok_or_else(denied)?;
        if entries.next().is_some() {
            return Err(Error::new(
                "broker publication needs one unambiguous policy entry",
            ));
        }
        let current_text = current.text();
        if Policy::parse(entry.value.as_slice())?.text() != current_text {
            return Err(Error::new(
                "root snapshot differs from current broker policy",
            ));
        }
        let target_text = target.text();
        let already_current = current_text == target_text;
        if !already_current && aterm_digest::Sha256::digest(current_text.as_bytes()) != base {
            return Err(Error::new("broker publication base changed"));
        }
        let refreshed = self.refresh_proposal_until(id, deadline)?;
        if (!already_current && refreshed.base() != base)
            || refreshed.policy().text() != target_text
        {
            return Err(Error::new("target is not the exact current broker refresh"));
        }
        self.config(deadline)?;
        if crate::application_paths_until(&self.paths, deadline)? != paths {
            return Err(denied());
        }
        if already_current {
            return Ok(Publication::AlreadyCurrent {
                sequence: admin.seq(),
            });
        }
        let mut doc = admin.published_document().clone();
        doc.set_entry_with(
            &entry.audience,
            ENTRY,
            target_text.as_bytes(),
            entry.if_absent,
            entry.anchor.as_deref(),
        )
        .map_err(|_| Error::new("broker policy cannot be encoded"))?;
        client::admin_publish_conditional_until(&paths, admin, &doc, deadline)
            .map(Box::new)
            .map(Publication::Published)
            .map_err(crate::table::failure)
    }
}
