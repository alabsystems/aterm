// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Strict physical contributors under a caller's retained control observation.

use super::*;

#[cfg(test)]
thread_local! {
    pub(crate) static OMIT_REGION: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// One canonical local replica, without initializing or repairing its storage.
/// A missing replica is a local storage fact, never authenticated table absence.
/// The caller retains all applicable control guards until this value is dropped;
/// those guards exclude enrollment, and a present contributor retains its own
/// data guard because an older Store can outlive the Session that opened it.
pub(crate) struct Contributor {
    path: PathBuf,
    scope: Scope,
    region: Region,
    claim_dir: PathBuf,
    parents: Vec<PathBuf>,
    present: Option<Present>,
    store: Option<Store>,
}

struct Present {
    guard: Option<aterm_journal::ReadGuard>,
    identity: Digest,
    fence: Fence,
}

/// Stable identities of the two local enrollments, not their advancing heads.
/// Missing and empty unclaimed directories have the same absent identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReplicaRevision {
    primary: Option<Digest>,
    recovery: Option<Digest>,
}

/// A bounded union that cannot outlive its retained control context.
/// Moving records into the union never releases either physical data guard.
pub(crate) struct OwnedTable<'c, 'a, 's, 'v> {
    context: &'c crate::generation::RetainedContext<'a, 's, 'v>,
    contributors: [Contributor; 2],
    revision: ReplicaRevision,
    scope: Scope,
    store: Option<Store>,
}

impl<'c, 'a, 's, 'v> OwnedTable<'c, 'a, 's, 'v> {
    pub(crate) fn observe(
        context: &'c crate::generation::RetainedContext<'a, 's, 'v>,
        scope: Scope,
    ) -> Result<Self> {
        context.check()?;
        let session = context.session();
        let mut primary = Contributor::observe(
            session,
            &Contributor::location(session, scope, Region::Primary)?,
            scope,
            Region::Primary,
        )?;
        let mut recovery = Contributor::observe(
            session,
            &Contributor::location(session, scope, Region::Recovery)?,
            scope,
            Region::Recovery,
        )?;
        let revision = ReplicaRevision {
            primary: primary.present.as_ref().map(|p| p.identity),
            recovery: recovery.present.as_ref().map(|p| p.identity),
        };
        #[cfg(test)]
        match OMIT_REGION.get() {
            1 => {
                primary.store.take();
            }
            2 => {
                recovery.store.take();
            }
            _ => {}
        }
        let store = match (primary.store.take(), recovery.store.take()) {
            (Some(primary), Some(recovery)) => {
                Some(recovery.with_observed_primary(session, primary)?)
            }
            (primary, recovery) => primary.or(recovery),
        };
        let table = Self {
            context,
            contributors: [primary, recovery],
            revision,
            scope,
            store,
        };
        table.check()?;
        Ok(table)
    }

    pub(crate) fn scope(&self) -> Scope {
        self.scope
    }

    pub(crate) fn session(&self) -> &Session<'s> {
        self.context.session()
    }

    pub(crate) fn context(&self) -> &'c crate::generation::RetainedContext<'a, 's, 'v> {
        self.context
    }

    pub(crate) fn is_absent(&self) -> bool {
        self.store.is_none() && self.revision.primary.is_none() && self.revision.recovery.is_none()
    }

    pub(crate) fn revision(&self) -> ReplicaRevision {
        self.revision
    }

    pub(crate) fn require_store(&self) -> Result<&Store> {
        self.store
            .as_ref()
            .ok_or_else(|| Error::new("required local table is absent"))
    }

    pub(crate) fn only_primary(&self) -> bool {
        self.revision.primary.is_some() && self.revision.recovery.is_none()
    }

    pub(crate) fn is_at(&self, directory: &crate::catalog::Directory) -> Result<bool> {
        Ok(!directory.is_recovery()
            && self.contributors[0].path == directory.path(self.scope)?
            && self.contributors[1].path == directory.recovery().path(self.scope)?)
    }

    pub(crate) fn check(&self) -> Result<()> {
        self.context.check()?;
        for contributor in &self.contributors {
            contributor.check(self.context.session())?;
        }
        if let Some(store) = &self.store {
            store.check(self.context.session())?;
        }
        Ok(())
    }

    pub(crate) fn check_custody(&self) -> Result<()> {
        self.context.check_custody()?;
        for contributor in &self.contributors {
            contributor.check_custody()?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn release_guards_for_test(&mut self) {
        for contributor in &mut self.contributors {
            contributor.release_guard_for_test();
        }
    }
}

impl Contributor {
    fn location(session: &Session<'_>, scope: Scope, region: Region) -> Result<PathBuf> {
        let directory = crate::catalog::Directory::new(session.paths(), session.trust());
        match region {
            Region::Primary => directory.path(scope),
            Region::Recovery => directory.recovery().path(scope),
        }
    }

    fn check_parents(parents: &[PathBuf]) -> Result<()> {
        for parent in parents {
            if !matches!(
                private_file::judge_private_dir(parent)?,
                private_file::PrivateDir::Missing | private_file::PrivateDir::Private
            ) {
                return Err(Error::new("replica contributor parent is not private"));
            }
        }
        Ok(())
    }

    fn absent(claim_dir: &Path, path: &Path, scope: Scope) -> Result<bool> {
        let empty = match private_file::read_private(claim_dir, &claim_name(scope), CLAIM_LEN) {
            Err(PrivateFileError::Missing) => match private_file::judge_private_dir(path)? {
                private_file::PrivateDir::Missing => Ok(true),
                private_file::PrivateDir::Private => {
                    Ok(std::fs::read_dir(path)?.next().transpose()?.is_none())
                }
                _ => Ok(false),
            },
            Ok(_) => Ok(false),
            Err(error) => Err(Error::new(&format!(
                "replica claim is unavailable: {error}"
            ))),
        }?;
        // Even an empty directory read must not hide a newly visible claim.
        Ok(empty
            && matches!(
                private_file::read_private(claim_dir, &claim_name(scope), CLAIM_LEN),
                Err(PrivateFileError::Missing)
            ))
    }

    /// Observe exactly one primary or recovery slot. Pending, malformed or
    /// damaged custody refuses, even when ordinary discovery would return None.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbPhysicalContributor",
            action = "Case",
            reason = "The environment chooses physical custody and changes after observation."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbPhysicalContributor",
            action = "Read",
            project = "aterm_db::table::store::contributor::tests::transition"
        )
    )]
    pub(crate) fn observe(
        session: &Session<'_>,
        path: &Path,
        scope: Scope,
        region: Region,
    ) -> Result<Self> {
        session.context(scope)?;
        if path != Self::location(session, scope, region)? {
            return Err(Error::new("replica contributor has another local location"));
        }
        let claim_dir = region.claims(session)?;
        let mut parents = vec![session.paths().dir.join("commons")];
        if region == Region::Recovery {
            parents.push(session.paths().dir.join("commons/recovery"));
        }
        parents.push(
            path.parent()
                .ok_or_else(|| Error::new("replica path has no parent"))?
                .into(),
        );
        parents.push(claim_dir.clone());
        Self::check_parents(&parents)?;
        let (present, store) = if Self::absent(&claim_dir, path, scope)? {
            (None, None)
        } else {
            let guard = aterm_journal::ReadGuard::take(path, "table", std::time::Instant::now())
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        Error::busy().context("replica contributor")
                    } else {
                        error.into()
                    }
                })?;
            let store = Store::open_region(
                session,
                path,
                scope,
                false,
                Capacity::default(),
                true,
                region,
            )?;
            let identity = store.custody_id(session)?;
            let fence = store
                .fence()
                .ok_or_else(|| Error::new("contributor has no observed custody"))?;
            (
                Some(Present {
                    guard: Some(guard),
                    identity,
                    fence,
                }),
                Some(store),
            )
        };
        let contributor = Self {
            path: path.into(),
            scope,
            region,
            claim_dir,
            parents,
            present,
            store,
        };
        contributor.check(session)?;
        Ok(contributor)
    }

    #[cfg(test)]
    pub(crate) fn require_store(&self) -> Result<&Store> {
        self.store
            .as_ref()
            .ok_or_else(|| Error::new("required replica contributor is absent"))
    }

    /// Recheck immediately before local receipt persistence. Empty discovery
    /// reservations may appear, but cannot enroll while control custody is held.
    pub(crate) fn check(&self, session: &Session<'_>) -> Result<()> {
        session.context(self.scope)?;
        if self.path != Self::location(session, self.scope, self.region)? {
            return Err(Error::snapshot_changed());
        }
        self.check_custody()?;
        if let Some(store) = &self.store {
            return store.check(session);
        }
        match &self.present {
            Some(present) if !present.fence.stands(session)? => Err(Error::snapshot_changed()),
            _ => Ok(()),
        }
    }

    /// Guard-only validation is safe inside an already admitted EntryReader;
    /// it never acquires another vault observation or reloads membership.
    pub(crate) fn check_custody(&self) -> Result<()> {
        Self::check_parents(&self.parents)?;
        match &self.present {
            Some(present) => present
                .guard
                .as_ref()
                .ok_or_else(Error::snapshot_changed)?
                .check()
                .map_err(Into::into),
            None if Self::absent(&self.claim_dir, &self.path, self.scope)? => Ok(()),
            None => Err(Error::snapshot_changed()),
        }
    }

    #[cfg(test)]
    pub(crate) fn release_guard_for_test(&mut self) {
        if let Some(present) = &mut self.present {
            present.guard.take();
        }
    }
}

#[cfg(test)]
mod tests;
