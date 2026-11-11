// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Strict physical contributors under a caller's retained control observation.

use super::*;

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
    present: Option<(Option<aterm_journal::ReadGuard>, Store)>,
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
        let present = if Self::absent(&claim_dir, path, scope)? {
            None
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
            Some((Some(guard), store))
        };
        let contributor = Self {
            path: path.into(),
            scope,
            region,
            claim_dir,
            parents,
            present,
        };
        contributor.check(session)?;
        Ok(contributor)
    }

    pub(crate) fn require_store(&self) -> Result<&Store> {
        self.present
            .as_ref()
            .map(|(_, store)| store)
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
        match &self.present {
            Some((_, store)) => store.check(session),
            None => Ok(()),
        }
    }

    /// Guard-only validation is safe inside an already admitted EntryReader;
    /// it never acquires another vault observation or reloads membership.
    pub(crate) fn check_custody(&self) -> Result<()> {
        Self::check_parents(&self.parents)?;
        match &self.present {
            Some((guard, _)) => {
                if let Some(guard) = guard {
                    guard.check()?;
                }
                Ok(())
            }
            None if Self::absent(&self.claim_dir, &self.path, self.scope)? => Ok(()),
            None => Err(Error::snapshot_changed()),
        }
    }

    #[cfg(test)]
    pub(crate) fn release_guard_for_test(&mut self) {
        if let Some((guard, _)) = &mut self.present {
            guard.take();
        }
    }
}

#[cfg(test)]
mod tests;
