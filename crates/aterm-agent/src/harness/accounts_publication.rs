// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Owned phases of the local account receipt protocol. These are not source
//! admission capabilities: authenticated policy must still be decided freshly.

use super::*;

struct OwnedScope {
    roster: Option<PathBuf>,
    fleet: Option<PathBuf>,
}

impl OwnedScope {
    fn new(scope: Scope<'_>) -> Self {
        Self {
            roster: scope.roster.map(Path::to_path_buf),
            fleet: scope.fleet.map(Path::to_path_buf),
        }
    }

    fn borrow(&self) -> Scope<'_> {
        Scope {
            roster: self.roster.as_deref(),
            fleet: self.fleet.as_deref(),
        }
    }
}

/// Validated images with fixed caller scope, still holding their original writer.
/// Dropping this value publishes nothing and creates no receipt or witness.
pub(super) struct PreparedPublication<'a> {
    store: &'a Store,
    held: &'a Lock,
    scope: OwnedScope,
    receipt: Receipt,
    encoded: EncodedReceipt,
    previous: Option<String>,
    changed: bool,
}

/// A retained intent. Dropping this value leaves recovery to the existing journal.
/// It carries neither a source guard nor arbitrary auxiliary callbacks.
pub(super) struct PendingPublication<'a> {
    store: &'a Store,
    held: &'a Lock,
    scope: OwnedScope,
    receipt: Receipt,
    encoded: EncodedReceipt,
}

fn require_writer(store: &Store, held: &Lock) -> Result<(), String> {
    #[cfg(test)]
    if MUTANT.get() == 2 {
        return Ok(());
    }
    if !store.native_writer_matches(held) {
        return Err("account publication requires this account store's writer".into());
    }
    Ok(())
}

impl<'a> PreparedPublication<'a> {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountPublicationPhases",
            action = "Scenario",
            reason = "The caller chooses a bounded phase interruption or competing intent."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountPublicationPhases",
            action = "Check",
            project = "aterm_agent::harness::accounts::sync_commit::publication::tests::transition"
        )
    )]
    pub(super) fn new(
        store: &'a Store,
        held: &'a Lock,
        scope: Scope<'_>,
        plan: Plan,
        has_events: bool,
    ) -> Result<Self, String> {
        require_writer(store, held)?;
        let previous = store.sync_read_receipt()?;
        plan.validate(store, scope)?;
        // A bounded ledger can retain identical bytes after an identical event.
        // That event still needs an intent and notification.
        let changed = has_events || plan.images().any(|image| image.before != image.after);
        let receipt = Receipt {
            schema: 1,
            id: aterm_uds::rand::hex_token::<16>().map_err(|why| why.to_string())?,
            plan: Some(plan),
        };
        // Entropy, size and serialization fail before any auxiliary work.
        let encoded = EncodedReceipt::new(&receipt)?;
        if let Some(plan) = &receipt.plan {
            plan.check_before(store, scope)?;
        }
        Ok(Self {
            store,
            held,
            scope: OwnedScope::new(scope),
            receipt,
            encoded,
            previous,
            changed,
        })
    }

    /// Retain only the bounded intent. Image replay is a separate consuming step.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountPublicationPhases",
            action = "Check",
            project = "aterm_agent::harness::accounts::sync_commit::publication::tests::transition"
        )
    )]
    pub(super) fn persist(self) -> Result<Option<PendingPublication<'a>>, String> {
        require_writer(self.store, self.held)?;
        #[cfg(test)]
        let check_previous = MUTANT.get() != 3;
        #[cfg(not(test))]
        let check_previous = true;
        if check_previous {
            self.store.check_sync_read_receipt(&self.previous)?;
        }
        let scope = self.scope.borrow();
        if let Some(plan) = &self.receipt.plan {
            plan.validate(self.store, scope)?;
            #[cfg(test)]
            let check_images = MUTANT.get() != 5;
            #[cfg(not(test))]
            let check_images = true;
            if check_images {
                plan.check_before(self.store, scope)?;
            }
        }
        #[cfg(test)]
        if EARLY_ROSTER.with(std::cell::Cell::take)
            && let Some(path) = scope.roster
            && let Some(plan) = &self.receipt.plan
            && let Some(text) = plan.roster.after.as_deref()
        {
            super::super::super::upgrade_models::write_atomic_mode(path, text, plan.roster_mode)
                .map_err(|why| why.to_string())?;
        }
        if !self.changed {
            return Ok(None);
        }
        // A clean receipt precedes its retained witness, so an interruption
        // before intent persistence does not invent an incomplete transaction.
        if self.previous.is_none() {
            self.store.save_receipt(&Receipt {
                schema: 1,
                id: "0".repeat(32),
                plan: None,
            })?;
        }
        durable_ancestry(
            std::iter::once(self.store.dir.as_path())
                .chain(scope.roster.map(publication_parent))
                .chain(scope.fleet.map(publication_parent)),
        )?;
        self.store
            .retain_witness(&self.store.dir.join(WITNESS))
            .map_err(|why| why.to_string())?;
        self.store.save_encoded_receipt(&self.encoded)?;
        let pending = PendingPublication {
            store: self.store,
            held: self.held,
            scope: self.scope,
            receipt: self.receipt,
            encoded: self.encoded,
        };
        #[cfg(test)]
        if MUTANT.get() == 1 {
            // Real negative control: install images in the intent phase.
            pending.store.replay(
                pending.held,
                pending.receipt.clone(),
                pending.scope.borrow(),
                &mut |_| Ok(()),
            )?;
        }
        Ok(Some(pending))
    }
}

impl PendingPublication<'_> {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountPublicationPhases",
            action = "Check",
            project = "aterm_agent::harness::accounts::sync_commit::publication::tests::transition"
        )
    )]
    pub(super) fn replay(self) -> Result<(), String> {
        require_writer(self.store, self.held)?;
        #[cfg(test)]
        let check_intent = MUTANT.get() != 4;
        #[cfg(not(test))]
        let check_intent = true;
        if check_intent
            && read(&self.store.dir.join(RECEIPT), MAX_RECEIPT)?.as_deref()
                != Some(self.encoded.0.as_str())
        {
            return Err("account publication intent was consumed or replaced".into());
        }
        #[cfg(test)]
        let validate_plan = MUTANT.get() != 7;
        #[cfg(not(test))]
        let validate_plan = true;
        if validate_plan && let Some(plan) = &self.receipt.plan {
            plan.validate(self.store, self.scope.borrow())?;
        }
        #[cfg(test)]
        if MUTANT.get() == 6 {
            return Ok(());
        }
        #[cfg(test)]
        let cut = CUT.with(std::cell::Cell::take);
        #[cfg(not(test))]
        let cut: Option<usize> = None;
        self.store
            .replay(self.held, self.receipt, self.scope.borrow(), &mut |step| {
                if cut == Some(step) {
                    Err("injected interruption".to_owned())
                } else {
                    Ok(())
                }
            })
    }
}

#[cfg(test)]
thread_local! {
    static MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
#[path = "accounts_publication_tests.rs"]
mod tests;
