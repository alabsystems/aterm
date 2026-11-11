// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retained native registry admission for local application receipts.

use super::super::{
    mail,
    registry::{ADDRESS, AddressIndex, MAILBOX},
};
use super::*;
use std::sync::Arc;

/// The exact native source of a local projection. This is an equality token,
/// never authority: each use reopens and authenticates its source. An inventory
/// alone misses a changed local actor, membership, or table generation.
#[derive(Clone, PartialEq)]
pub struct SourceRevision {
    catalog: Digest,
    scope: Digest,
    actor: astream_wire::sync::Actor,
    control: [u8; 64],
    inventory: Inventory,
    projection: Arc<LabView>,
    mailboxes: Arc<Vec<mail::Mailbox>>,
    addresses: Arc<AddressIndex>,
}

impl std::fmt::Debug for SourceRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Keep account addresses out of a revision's diagnostic representation.
        f.debug_struct("SourceRevision")
            .field("catalog", &self.catalog)
            .field("scope", &self.scope)
            .field("actor", &self.actor)
            .field("control", &self.control)
            .field("inventory", &self.inventory)
            .finish_non_exhaustive()
    }
}

/// A complete native registry projection paired with its exact source revision.
/// This value retains no lock or authority; persistence must admit it again.
#[derive(Clone, Debug)]
pub struct SourceRead {
    revision: SourceRevision,
}

impl SourceRead {
    /// The immutable projection captured by this revision. Keeping a clone
    /// preserves the observation, never the authority to save a receipt.
    #[must_use]
    pub fn view(&self) -> &Arc<LabView> {
        &self.revision.projection
    }

    /// The comparison token for a later retained admission.
    #[must_use]
    pub fn revision(&self) -> &SourceRevision {
        &self.revision
    }

    /// The registry table this observation belongs to, including its generation.
    #[must_use]
    pub fn table(&self) -> Digest {
        self.revision.scope
    }

    /// Whether current local subscription policy includes this source's exact
    /// catalog and table. Inclusion alone grants no receipt authority.
    #[must_use]
    pub fn included_by(&self, policy: &crate::catalog::Policy) -> bool {
        policy.includes(self.revision.catalog, self.revision.scope)
    }

    /// Mailbox metadata from the same verified view as the account projection.
    /// Credential values are never included.
    #[must_use]
    pub fn mailboxes(&self) -> &[mail::Mailbox] {
        &self.revision.mailboxes
    }

    /// The address lookup from this same source view.
    #[must_use]
    pub fn addresses(&self) -> &AddressIndex {
        &self.revision.addresses
    }

    fn read(session: &Session<'_>, scope: Scope, view: &View) -> Result<Self> {
        if !view.complete {
            return Err(Error::new("native registry source history is incomplete"));
        }
        let ids = view.ops.iter().map(|op| op.id).collect();
        let actor = session.actor();
        let uuid = session
            .members()?
            .iter()
            .find(|binding| binding.actor() == actor)
            .map(|binding| keys::uuid32(binding.machine()));
        let empty = crate::typed::owned::OwnedRead::default();
        let owned = |name: &str| view.owned.get(name).unwrap_or(&empty);
        let projection = Arc::new(LabView::read(
            &view.cells,
            owned(MACHINE_FACTS.name),
            owned(MAILBOX_OBS.name),
            uuid.as_deref(),
        ));
        let addresses = view.project(&ADDRESS);
        let mailboxes = mail::mailboxes(&addresses, &view.project(&MAILBOX));
        Ok(Self {
            revision: SourceRevision {
                catalog: crate::catalog::scope(session.trust()).digest(),
                scope: scope.digest(),
                actor,
                control: session.observation()?,
                inventory: Inventory(crate::replica::InventorySummary::of(&ids).fingerprint),
                projection,
                mailboxes: Arc::new(mailboxes),
                addresses: Arc::new(AddressIndex::of(&addresses)),
            },
        })
    }
}

impl Registry {
    /// Observe a complete projection and its revision together, without taking
    /// a control or table writer or initializing missing custody. A reader may
    /// capture this token, but only a current writer can later admit it.
    ///
    /// `None` means no Commons on a new installation or no declaration.
    /// Excluded or incomplete sources are errors, never evidence of absence.
    ///
    /// # Errors
    /// Excluded source, invalid or unestablished custody, unreadable policy, unavailable storage,
    /// an incompatible descriptor, partial history, or a changed generation.
    pub fn observe_source(&self, config: &Path) -> Result<Option<SourceRead>> {
        let policy = crate::catalog::policy::Policy::read(config)?;
        if !policy.enabled {
            return Err(Error::new("native registry source is excluded"));
        }
        let deadline = crate::admission::deadline();
        application::observe_existing_session(&self.paths, deadline, |session| {
            let directory = crate::catalog::Directory::new(session.paths(), session.trust());
            let catalog = directory.observe_with_policy(session, &Default::default())?;
            let compiled =
                crate::att::current_in(&catalog, session.trust(), &crate::att::REGISTRY.chain());
            let descriptor = compiled.descriptor(session.trust())?;
            if !policy.includes(
                directory.catalog_scope().digest(),
                descriptor.scope.digest(),
            ) {
                return Err(Error::new("native registry source is excluded"));
            }
            if !catalog.entries.contains_key(&descriptor.scope.digest()) {
                return Ok(None);
            }
            application::observe_declared_in_session(
                session,
                descriptor,
                deadline,
                |session, store, scope| {
                    let log = Log::read_with(session, store, scope, decode)?;
                    let view = View::read(compiled.table, session, &log);
                    let source = SourceRead::read(session, scope, &view)?;
                    include_source(config, session.paths(), session.trust(), scope)?;
                    Ok(source)
                },
            )
        })
        .map(Option::flatten)
    }

    /// Derive a complete current source under retained vault, lineage control,
    /// and table locks, and keep them through `persist`. With `expected`, a
    /// changed source refuses before calling it; without one this captures a
    /// first observation. No declaration or application is created.
    ///
    /// A caller may persist a bounded local receipt here. Acquire its local
    /// account writer first. Do not reenter Commons, acquire another account
    /// writer, mutate vault state or use the network inside `persist`; replay
    /// images and dispatch only after this function returns. The configuration
    /// policy is freshly observed just before the callback, not locked against
    /// an external editor; delivery must independently check its current policy.
    ///
    /// # Errors
    /// Missing, partial, excluded, changed or incompatible source; unavailable
    /// custody or a non-writer identity; expired local admission; or `persist`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbNativeSource",
            action = "Scenario",
            reason = "The environment selects the captured revision, local policy and persistence outcome."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbNativeSource",
            action = "Admit",
            project = "aterm_db::alab::store::source::tests::transition"
        )
    )]
    pub fn with_admitted_source<T>(
        &self,
        config: &Path,
        expected: Option<&SourceRevision>,
        persist: impl FnOnce(&SourceRead) -> Result<T>,
    ) -> Result<T> {
        let deadline = crate::admission::deadline();
        application::with_admitted_store(
            &self.paths,
            |session| {
                let directory = crate::catalog::Directory::new(session.paths(), session.trust());
                let catalog = directory.snapshot_existing(session)?;
                let compiled = crate::att::current_in(
                    &catalog,
                    session.trust(),
                    &crate::att::REGISTRY.chain(),
                );
                let descriptor = compiled.descriptor(session.trust())?;
                include_source(config, session.paths(), session.trust(), descriptor.scope)?;
                Ok((descriptor, compiled))
            },
            deadline,
            |session, store, scope, compiled| {
                let log = Log::read_with(session, store, scope, decode)?;
                let view = View::read(compiled.table, session, &log);
                let source = SourceRead::read(session, scope, &view)?;
                #[cfg(test)]
                let check_revision = MUTANT.get() != 1;
                #[cfg(not(test))]
                let check_revision = true;
                if check_revision && expected.is_some_and(|expected| expected != &source.revision) {
                    return Err(Error::new("native registry source changed after capture"));
                }
                include_source(config, session.paths(), session.trust(), scope)?;
                if std::time::Instant::now() >= deadline {
                    return Err(Error::new("native registry source admission expired"));
                }
                persist(&source)
            },
        )
    }

    /// Admit a primary native projection and ordinary vault candidates under
    /// one retained local context. Current Readers and Writers may project;
    /// this grants no shared-table, provider or budget mutation capability.
    ///
    /// The caller already holds its local account writer. Compare every
    /// relevant candidate through `reader` before saving bounded prepared
    /// intent. Do not reenter Commons, acquire another writer, use the network
    /// or replay images inside `persist`. Source guards remain held through
    /// the callback and release before this function returns; replay follows.
    ///
    /// `entry_vault` must be the current generation or a retained ancestor.
    /// Missing or recovery-only native custody is unavailable, not absent.
    ///
    /// # Errors
    /// Stale or denied membership, changed/partial/excluded source, missing or
    /// busy custody, wrong vault/configuration, expired admission, or `persist`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbNativeProjection",
            action = "Case",
            reason = "The environment selects membership, source, policy and callback outcome."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbNativeProjection",
            action = "Read",
            project = "aterm_db::alab::store::source::projection_tests::transition"
        )
    )]
    pub fn with_admitted_projection<T>(
        &self,
        config: &Path,
        entry_vault: &Paths,
        vault_config: &aterm_secrets::client::Config,
        expected: Option<&SourceRevision>,
        persist: impl FnOnce(&SourceRead, &aterm_secrets::client::EntryReader<'_>) -> Result<T>,
    ) -> Result<T> {
        #[cfg(test)]
        if PROJECTION_MUTANT.get() == 3 {
            return Err(Error::new("injected projection unavailability"));
        }
        let deadline = crate::admission::deadline();
        application::with_retained_store(
            &self.paths,
            entry_vault,
            deadline,
            |session, catalog| {
                let compiled =
                    crate::att::current_in(catalog, session.trust(), &crate::att::REGISTRY.chain());
                let descriptor = compiled.descriptor(session.trust())?;
                include_source(config, session.paths(), session.trust(), descriptor.scope)?;
                Ok((descriptor, compiled))
            },
            |session, store, scope, compiled, look, validate| {
                let log = Log::read_with(session, store, scope, decode)?;
                let view = View::read(compiled.table, session, &log);
                let source = SourceRead::read(session, scope, &view)?;
                #[cfg(test)]
                let compare = PROJECTION_MUTANT.get() != 1;
                #[cfg(not(test))]
                let compare = true;
                if compare && expected.is_some_and(|expected| expected != source.revision()) {
                    return Err(Error::snapshot_changed());
                }
                include_source(config, session.paths(), session.trust(), scope)?;
                (validate.current)()?;
                match look
                    .with_entry_reader(entry_vault, vault_config, |reader| {
                        if std::time::Instant::now() >= deadline {
                            return Err(Error::new("native projection admission expired"));
                        }
                        include_source(config, session.paths(), session.trust(), scope)?;
                        (validate.custody)()?;
                        persist(&source, reader)
                    })
                    .map_err(crate::table::failure)?
                {
                    aterm_secrets::client::EntryAdmission::Accepted(result) => result,
                    aterm_secrets::client::EntryAdmission::Busy => {
                        Err(Error::busy().context("ordinary vault projection source"))
                    }
                    aterm_secrets::client::EntryAdmission::Changed => {
                        Err(Error::snapshot_changed())
                    }
                    aterm_secrets::client::EntryAdmission::Unavailable => Err(Error::new(
                        "ordinary vault source is unavailable for projection",
                    )),
                }
            },
        )
    }
}

/// Check before opening the table (which may recover an interrupted append),
/// and again after projection immediately before local persistence.
fn include_source(
    config: &Path,
    paths: &Paths,
    trust: aterm_secrets::commons::journal::Trust,
    scope: Scope,
) -> Result<()> {
    let policy = crate::catalog::policy::Policy::read(config)?;
    let catalog = crate::catalog::Directory::new(paths, trust)
        .catalog_scope()
        .digest();
    #[cfg(test)]
    let check_policy = MUTANT.get() != 2;
    #[cfg(not(test))]
    let check_policy = true;
    if check_policy && !policy.includes(catalog, scope.digest()) {
        return Err(Error::new("native registry source is excluded"));
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    static MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static PROJECTION_MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "projection_tests.rs"]
mod projection_tests;
