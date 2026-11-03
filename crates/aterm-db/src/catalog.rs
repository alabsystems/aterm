// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Default, secrets-backed table discovery. The catalog is an ordinary signed
//! astream table with a deterministic scope. Descriptors are inert data; only
//! local code chooses paths, validators and whether credentials may be used.

use crate::{
    Error, Result,
    table::{Scope, Session, store::Store},
};
use astream_wire::sync::{Actor, Digest};
use aterm_secrets::{client::Paths, commons::journal::Trust};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub(crate) mod command;
mod descriptor;
pub(crate) mod policy;
pub mod release;
pub(crate) mod ring;
pub use descriptor::Descriptor;
use policy::Policy;

/// The catalog has its own slot; applications cannot displace it.
pub(crate) const MAX_TABLES: usize = 63;
/// The slots of [`MAX_TABLES`] kept for the compiled reserved tables ([`reserved`]), which take
/// them before any declaration in causal order (docs/DESIGN-alab-lab-model-2026-11-01.md
/// §3.10): the registry and the generation a roll restates it into, which both hold storage
/// until the storage release frees the sealed one ([`release`]).
pub(crate) const RESERVED_SLOTS: usize = 2;
/// The slots of [`MAX_TABLES`] the catalog's declared tables share: the rest stay for the rings
/// (`ring::RING_SLOTS`) and the reserved tables ([`RESERVED_SLOTS`]), whose storage never
/// counts against these.
pub(crate) const CATALOG_SLOTS: usize = MAX_TABLES - ring::RING_SLOTS - RESERVED_SLOTS;

/// The compiled tables that take the reserved slots: the lab registry's generations
/// (`alab/registry` and `alab/registry.2`, each at version 1). Catalog slots otherwise go to
/// declarations in causal order, so a new Mac that meets more subscribable declarations ahead of
/// the registry than there are slots could never hold it, and the owner keeps adding Macs; a
/// reserved table is subscribed whatever its causal order and whoever declared it, while there
/// is storage room at all. Its descriptor is matched exactly: any other declaration at its
/// identity conflicts as for every table.
pub(crate) fn reserved(trust: Trust) -> [Descriptor; RESERVED_SLOTS] {
    let generation = |table: &crate::typed::Table| {
        Descriptor::application(
            trust,
            table.application,
            table.role,
            crate::alab::registry::VERSION,
        )
        .expect("the registry's declaration is valid")
    };
    crate::alab::registry::GENERATIONS.map(generation)
}
pub(crate) const PER_CREATOR: usize = 8;
pub(crate) const RECORDS_PER_CREATOR: usize = 64;
pub(crate) const MAX_PAYLOAD: usize = 4096;
pub(crate) fn scope(trust: Trust) -> Scope {
    descriptor::catalog_scope(trust)
}

/// Signed provenance of a declaration. Time is an author claim, never priority.
#[derive(Clone, Debug)]
pub struct Declaration {
    pub descriptor: Descriptor,
    pub operation: Digest,
    pub actor: Actor,
    pub authored_ms: u64,
}
/// Why a known table is, or is not, eligible for automatic repair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Subscribed,
    Conflict,
    Excluded,
    CreatorLimit,
    Capacity,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub scope: Scope,
    pub declarations: Vec<Declaration>,
    pub state: State,
}
#[derive(Debug, Default)]
pub struct Snapshot {
    /// Declarations were read from separate recovery storage, not repaired writer custody.
    pub recovery: bool,
    pub entries: BTreeMap<Digest, Entry>,
    /// Authenticated operations with unsupported/invalid catalog payloads.
    pub inert: usize,
}

/// Deterministic storage locations inside this secrets installation. No path
/// component, code or grant is accepted from an announced table descriptor.
#[derive(Clone)]
pub struct Directory {
    root: PathBuf,
    trust: Trust,
    region: crate::table::store::Region,
}
impl Directory {
    pub fn new(paths: &Paths, trust: Trust) -> Self {
        Self {
            root: paths.dir.join("commons/tables"),
            trust,
            region: crate::table::store::Region::Primary,
        }
    }

    pub(crate) fn recovery(&self) -> Self {
        if self.is_recovery() {
            return self.clone();
        }
        Self {
            root: self
                .root
                .parent()
                .expect("table root has a Commons parent")
                .join("recovery/tables"),
            trust: self.trust,
            region: crate::table::store::Region::Recovery,
        }
    }

    pub(crate) fn is_recovery(&self) -> bool {
        self.region == crate::table::store::Region::Recovery
    }

    pub(crate) fn primary(&self) -> Self {
        if !self.is_recovery() {
            return self.clone();
        }
        Self {
            root: self
                .root
                .parent()
                .and_then(Path::parent)
                .expect("recovery tables have a Commons grandparent")
                .join("tables"),
            trust: self.trust,
            region: crate::table::store::Region::Primary,
        }
    }

    pub(crate) fn open_store(&self, session: &Session<'_>, scope: Scope) -> Result<Store> {
        let path = self.path(scope)?;
        match self.region {
            crate::table::store::Region::Primary => Store::open(session, &path, scope),
            crate::table::store::Region::Recovery => Store::open_recovery(session, &path, scope),
        }
    }

    pub(crate) fn observe_store(&self, session: &Session<'_>, scope: Scope) -> Result<Store> {
        let path = self.path(scope)?;
        match self.region {
            crate::table::store::Region::Primary => Store::observe(session, &path, scope),
            crate::table::store::Region::Recovery => Store::observe_recovery(session, &path, scope),
        }
    }

    /// Observe existing authenticated data. An application's recovery observation
    /// includes healthy primary data, rechecking both custody snapshots. This never
    /// creates storage or changes writer custody. Inspect [`Store::is_recovery`]
    /// before treating its contents as the original replica.
    ///
    /// # Errors
    /// A required custody snapshot is unavailable or changes, membership is
    /// stale, the scope is foreign, or combined evidence disagrees or exceeds
    /// the table's limits.
    pub fn observe_table(&self, session: &Session<'_>, scope: Scope) -> Result<Store> {
        match self.observe_table_if_exists(session, scope)? {
            Some(store) => Ok(store),
            None => self.observe_store(session, scope),
        }
    }

    pub(crate) fn observe_store_if_exists(
        &self,
        session: &Session<'_>,
        scope: Scope,
    ) -> Result<Option<Store>> {
        Store::observe_if_exists_region(session, &self.path(scope)?, scope, self.region)
    }

    /// Observe existing primary or recovery storage without creating it. A missing
    /// table or incomplete first enrollment returns `None`; a valid recovery
    /// observation includes healthy primary application data and remains
    /// receive-only. Sharing policy is the caller's decision.
    ///
    /// # Errors
    /// A required established store is damaged or changes, the scope belongs
    /// to another network, membership is stale, or combined evidence disagrees
    /// or exceeds the table's limits.
    pub fn observe_table_if_exists(
        &self,
        session: &Session<'_>,
        scope: Scope,
    ) -> Result<Option<Store>> {
        match self.observe_store_if_exists(session, scope) {
            Ok(Some(store)) if self.is_recovery() && scope != self.catalog_scope() => {
                match self.primary().observe_store_if_exists(session, scope) {
                    Ok(Some(primary)) => store.with_observed_primary(session, primary).map(Some),
                    Err(error) if error.is_busy() => Err(error),
                    _ => Ok(Some(store)),
                }
            }
            Ok(Some(store)) if !self.is_recovery() && scope != self.catalog_scope() => {
                match self.recovery().observe_store_if_exists(session, scope)? {
                    Some(recovery) => recovery.with_observed_primary(session, store).map(Some),
                    None => Ok(Some(store)),
                }
            }
            Ok(Some(store)) => Ok(Some(store)),
            Ok(None) if !self.is_recovery() => {
                self.recovery().observe_store_if_exists(session, scope)
            }
            Ok(None) => Ok(None),
            Err(error) if self.is_recovery() || error.is_busy() => Err(error),
            Err(original) => match self.recovery().observe_store_if_exists(session, scope) {
                Ok(Some(store)) => Ok(Some(store)),
                _ => Err(original),
            },
        }
    }

    fn join_store(&self, session: &Session<'_>, path: &Path, scope: Scope) -> Result<Store> {
        match self.region {
            crate::table::store::Region::Primary => Store::join(session, path, scope),
            crate::table::store::Region::Recovery => Store::join_recovery(session, path, scope),
        }
    }
    pub fn catalog_scope(&self) -> Scope {
        scope(self.trust)
    }
    /// The identities of this lab's reserved tables ([`reserved`]).
    fn reserved_ids(&self) -> BTreeSet<Digest> {
        reserved(self.trust)
            .iter()
            .map(|descriptor| descriptor.scope.digest())
            .collect()
    }
    /// Whether `descriptor` is one of this lab's reserved tables, exactly.
    fn is_reserved(&self, descriptor: &Descriptor) -> bool {
        reserved(self.trust).contains(descriptor)
    }
    /// The lab this directory's tables belong to.
    pub(crate) const fn trust(&self) -> Trust {
        self.trust
    }
    pub fn path(&self, scope: Scope) -> Result<PathBuf> {
        if scope.trust() != self.trust {
            return Err(Error::new("foreign table network"));
        }
        Ok(self
            .root
            .join(aterm_codec::hex::encode_lower(&scope.digest())))
    }
    fn catalog(&self, session: &Session<'_>) -> Result<Store> {
        let scope = self.catalog_scope();
        let path = self.path(scope)?;
        if let Ok(store) = self.open_store(session, scope) {
            return Ok(store);
        }
        if !self.is_recovery() {
            aterm_journal::create_private_dir_durable(&self.root)?;
        }
        self.join_store(session, &path, scope)
    }
    /// The storage directories counted against the catalog's slots ([`CATALOG_SLOTS`], and the
    /// catalog's own): every retained one but the ring tables this Mac holds
    /// (`ring_storage`) and the reserved tables' ([`RESERVED_SLOTS`]), out of `retained`'s
    /// `count`.
    fn catalog_count(
        &self,
        session: &Session<'_>,
        retained: &BTreeSet<Digest>,
        count: usize,
    ) -> usize {
        let rings = self.ring_storage(session);
        let reserved = self.reserved_ids();
        count.saturating_sub(
            retained
                .iter()
                .filter(|id| rings.contains(*id) || reserved.contains(*id))
                .count(),
        )
    }
    fn retained(&self) -> Result<(BTreeSet<Digest>, usize)> {
        let mut retained = BTreeSet::new();
        let mut count = 0;
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            count += 1;
            if count > MAX_TABLES + 1 {
                return Err(Error::new("retained table storage exceeds its bound"));
            }
            if let Some(name) = entry.file_name().to_str()
                && let Ok(id) = crate::database::decode_hex::<32>(name)
            {
                retained.insert(id);
            }
        }
        if self.is_recovery() && self.primary().root.try_exists()? {
            // Catalog recovery does not retire healthy primary application
            // stores. Their declarations and occupied slots still matter.
            let (primary, primary_count) = self.primary().retained()?;
            let duplicate = retained.intersection(&primary).count();
            count = count.saturating_add(primary_count.saturating_sub(duplicate));
            if count > MAX_TABLES + 1 {
                return Err(Error::new(
                    "combined retained table storage exceeds its bound",
                ));
            }
            retained.extend(primary);
        }
        Ok((retained, count))
    }
    /// Whether `scope`'s storage exists or one more directory fits beside every retained one
    /// (all of them, where the storage root is not made yet).
    fn has_room(&self, scope: Scope) -> Result<bool> {
        let path = self.path(scope)?;
        if !path.try_exists()? && self.root.try_exists()? {
            // Count every retained directory, not merely the current selection.
            // Repeated catalog churn cannot accumulate unbounded old stores.
            for (count, entry) in std::fs::read_dir(&self.root)?.enumerate() {
                entry?;
                if count >= MAX_TABLES {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
    fn room(&self, scope: Scope) -> Result<PathBuf> {
        if !self.has_room(scope)? {
            return Err(Error::new("retained table storage limit reached"));
        }
        self.path(scope)
    }
    pub(crate) fn enroll(&self, session: &Session<'_>, scope: Scope) -> Result<Store> {
        let path = self.room(scope)?;
        if let Ok(store) = self.open_store(session, scope) {
            return Ok(store);
        }
        self.join_store(session, &path, scope)
    }
    /// Durable offline creation and publication. Reopening the same descriptor
    /// coalesces an existing declaration, without adding another signed record.
    pub fn publish(&self, session: &Session<'_>, descriptor: &Descriptor) -> Result<Scope> {
        session.writer()?;
        let payload = descriptor.encode()?;
        if descriptor.scope.trust() != self.trust || descriptor.scope == self.catalog_scope() {
            return Err(Error::new(
                "invalid declaration network or catalog identity",
            ));
        }
        let mut catalog = self.catalog(session)?;
        let snapshot = self.project(session, catalog.catalog(session)?, &Policy::default())?;
        let id = descriptor.scope.digest();
        // A reserved table takes its own slot: neither this creator's budget of tables nor the
        // shared slots others' declarations hold can keep it out.
        let reserved = self.is_reserved(descriptor);
        if let Some(entry) = snapshot.entries.get(&id) {
            if entry.state == State::Conflict
                || entry
                    .declarations
                    .iter()
                    .any(|d| d.descriptor != *descriptor)
            {
                return Err(Error::new("table has incompatible declarations"));
            }
            if entry.state == State::Subscribed {
                self.enroll(session, descriptor.scope)?;
                return Ok(descriptor.scope);
            }
            if entry
                .declarations
                .iter()
                .any(|d| d.actor == session.actor())
            {
                return Err(Error::new(
                    "existing declaration exceeds subscription capacity",
                ));
            }
        }
        let count = snapshot
            .entries
            .values()
            .filter(|entry| {
                entry
                    .declarations
                    .iter()
                    .any(|d| d.actor == session.actor())
            })
            .count();
        if count >= PER_CREATOR && !reserved {
            return Err(Error::new("table creator subscription limit reached"));
        }
        if catalog.retained_by(session, session.actor())? >= RECORDS_PER_CREATOR {
            return Err(Error::new("catalog author history limit reached"));
        }
        let (retained, count) = self.retained()?;
        let count = self.catalog_count(session, &retained, count);
        let reserved_ids = self.reserved_ids();
        let awaiting = snapshot
            .entries
            .iter()
            .filter(|(id, entry)| {
                entry.state == State::Subscribed
                    && !retained.contains(*id)
                    && !reserved_ids.contains(*id)
            })
            .count();
        if count + awaiting > CATALOG_SLOTS && !reserved {
            return Err(Error::new("subscription capacity is already assigned"));
        }
        self.room(descriptor.scope)?;
        catalog.commit_catalog(session, &payload)?;
        self.enroll(session, descriptor.scope)?;
        Ok(descriptor.scope)
    }

    /// Add this Mac's own declaration of a reserved table another Mac declared, as evidence
    /// that this Mac runs a build that compiles it (a registry roll seals only once every Writer
    /// shows that, `alab::roll`): `publish` coalesces onto an existing declaration, so it says
    /// nothing of the Mac that runs it. Only a reserved table, declared alike by everyone, still
    /// subscribed and not yet declared by this Mac, gets one; it takes no creator slot. Returns
    /// whether a declaration was written.
    ///
    /// # Errors
    /// No Commons writer, the catalog cannot be read or written, or this Mac's catalog history
    /// is spent.
    pub(crate) fn endorse(&self, session: &Session<'_>, descriptor: &Descriptor) -> Result<bool> {
        session.writer()?;
        if !self.is_reserved(descriptor) || descriptor.scope.trust() != self.trust {
            return Ok(false);
        }
        let mut catalog = self.catalog(session)?;
        let snapshot = self.project(session, catalog.catalog(session)?, &Policy::default())?;
        let Some(entry) = snapshot.entries.get(&descriptor.scope.digest()) else {
            return Ok(false);
        };
        if entry.state != State::Subscribed
            || entry
                .declarations
                .iter()
                .any(|d| d.descriptor != *descriptor)
            || entry
                .declarations
                .iter()
                .any(|d| d.actor == session.actor())
        {
            return Ok(false);
        }
        if catalog.retained_by(session, session.actor())? >= RECORDS_PER_CREATOR {
            return Err(Error::new("catalog author history limit reached"));
        }
        catalog.commit_catalog(session, &descriptor.encode()?)?;
        Ok(true)
    }

    /// The current Writers (`Session::writers`) with no declaration of `descriptor` in
    /// `snapshot`, by Commons machine: the Macs that have not shown a build that compiles it
    /// ([`Self::endorse`]).
    pub(crate) fn undeclared(
        session: &Session<'_>,
        snapshot: &Snapshot,
        descriptor: &Descriptor,
    ) -> Vec<[u8; 16]> {
        let declared = snapshot
            .entries
            .get(&descriptor.scope.digest())
            .map(|entry| entry.declarations.as_slice())
            .unwrap_or_default();
        session
            .writers()
            .into_iter()
            .filter(|binding| {
                !declared
                    .iter()
                    .any(|d| d.descriptor == *descriptor && d.actor == binding.actor())
            })
            .map(|binding| *binding.machine())
            .collect()
    }
    /// Opens work immediately on one offline machine, with the same identity
    /// when another machine independently opens this application role.
    pub fn open_application(
        &self,
        session: &Session<'_>,
        application: &str,
        role: &str,
        version: u32,
    ) -> Result<Scope> {
        self.publish(
            session,
            &Descriptor::application(self.trust, application, role, version)?,
        )
    }
    /// Current catalog view, revalidated against the locked control observation.
    pub fn snapshot(&self, session: &Session<'_>) -> Result<Snapshot> {
        self.project(
            session,
            self.discovery_records(session)?,
            &Policy::default(),
        )
    }
    /// Local view with the same policy used by automatic network repair.
    pub fn snapshot_with_config(&self, session: &Session<'_>, config: &Path) -> Result<Snapshot> {
        self.project(
            session,
            self.discovery_records(session)?,
            &Policy::read(config)?,
        )
    }

    /// Read locally retained declarations without enrolling or repairing storage.
    /// A genuinely absent catalog is empty. Damaged primary custody may use an
    /// already verified recovery replica; neither store is initialized here.
    pub fn observe_with_config(&self, session: &Session<'_>, config: &Path) -> Result<Snapshot> {
        self.observe_with_policy(session, &Policy::read(config)?)
    }

    /// Read retained declarations through one policy snapshot, including recovery fallback.
    pub(crate) fn observe_with_policy(
        &self,
        session: &Session<'_>,
        policy: &Policy,
    ) -> Result<Snapshot> {
        match self.observe_primary_with_policy(session, policy) {
            Ok(snapshot) => Ok(snapshot),
            Err(error) if self.is_recovery() || error.is_busy() => Err(error),
            Err(original) => {
                let recovery = self.recovery();
                if !matches!(
                    recovery.observe_store_if_exists(session, self.catalog_scope()),
                    Ok(Some(_))
                ) {
                    return Err(original);
                }
                recovery
                    .observe_primary_with_policy(session, policy)
                    .map_err(|_| original)
            }
        }
    }

    pub(crate) fn observe_primary_with_config(
        &self,
        session: &Session<'_>,
        config: &Path,
    ) -> Result<Snapshot> {
        self.observe_primary_with_policy(session, &Policy::read(config)?)
    }

    fn observe_primary_with_policy(
        &self,
        session: &Session<'_>,
        policy: &Policy,
    ) -> Result<Snapshot> {
        let root_exists = match aterm_uds::private_file::check_private_dir(&self.root) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        let scope = self.catalog_scope();
        let Some(catalog) = self.observe_store_if_exists(session, scope)? else {
            if root_exists && self.retained()?.1 != 0 {
                return Err(Error::new(
                    "catalog is absent beside retained table storage",
                ));
            }
            return Ok(Snapshot::default());
        };
        self.project(
            session,
            self.discovery_records_from(session, &catalog)?,
            policy,
        )
    }

    /// Read the catalog this Mac holds with default local projection policy, without enrolling
    /// it: an absent catalog reads as empty.
    pub(crate) fn snapshot_if_exists(&self, session: &Session<'_>) -> Result<Snapshot> {
        let scope = self.catalog_scope();
        match self.observe_store_if_exists(session, scope)? {
            None => Ok(Snapshot::default()),
            Some(catalog) => self.project(
                session,
                self.discovery_records_from(session, &catalog)?,
                &Policy::default(),
            ),
        }
    }

    /// Retire `retired`, a table an earlier build declares, by declaring its next version once:
    /// its declarations then conflict, so a build that knows only `retired` neither reads nor
    /// writes it again. Only where this Mac's catalog declares `retired` and nothing else at its
    /// identity; the creator budgets hold as for [`Self::publish`], and nothing is enrolled.
    /// Whether a declaration was written.
    ///
    /// # Errors
    /// Not a current writer, a foreign or catalog identity, unreadable catalog storage, or this
    /// Mac's creator budget is spent.
    pub(crate) fn seal(&self, session: &Session<'_>, retired: &Descriptor) -> Result<bool> {
        session.writer()?;
        if retired.scope.trust() != self.trust || retired.scope == self.catalog_scope() {
            return Err(Error::new(
                "invalid declaration network or catalog identity",
            ));
        }
        let twin = Descriptor {
            version: retired
                .version
                .checked_add(1)
                .ok_or_else(|| Error::new("table schema version is exhausted"))?,
            ..retired.clone()
        };
        let payload = twin.encode()?;
        let scope = self.catalog_scope();
        let claim =
            crate::table::store::claim_dir(session)?.join(crate::table::store::claim_name(scope));
        if !claim.try_exists()? {
            return Ok(false);
        }
        let mut catalog = self.open_store(session, scope)?;
        let snapshot = self.project(session, catalog.catalog(session)?, &Policy::default())?;
        let Some(entry) = snapshot.entries.get(&retired.scope.digest()) else {
            return Ok(false);
        };
        if entry.declarations.iter().any(|d| d.descriptor != *retired) {
            return Ok(false);
        }
        let mine = |entry: &Entry| {
            entry
                .declarations
                .iter()
                .any(|d| d.actor == session.actor())
        };
        if !mine(entry) && snapshot.entries.values().filter(|e| mine(e)).count() >= PER_CREATOR {
            return Err(Error::new("table creator subscription limit reached"));
        }
        if catalog.retained_by(session, session.actor())? >= RECORDS_PER_CREATOR {
            return Err(Error::new("catalog author history limit reached"));
        }
        catalog.commit_catalog(session, &payload)?;
        Ok(true)
    }

    /// Read an existing catalog with default local projection policy. Missing
    /// or damaged established storage is never replaced by an empty enrollment.
    pub fn snapshot_existing(&self, session: &Session<'_>) -> Result<Snapshot> {
        let catalog = self.observe_table(session, self.catalog_scope())?;
        let directory = if catalog.is_recovery() {
            self.recovery()
        } else {
            self.clone()
        };
        directory.project(
            session,
            directory.discovery_records_from(session, &catalog)?,
            &Policy::default(),
        )
    }
    /// Observe an already enrolled catalog without falling back to enrollment
    /// when its claim or journal is absent or incomplete.
    ///
    /// # Errors
    ///
    /// Returns an error when existing catalog custody or storage cannot be
    /// validated, retained storage exceeds its bound, or the sharing
    /// configuration is unavailable or invalid.
    pub fn snapshot_existing_with_config(
        &self,
        session: &Session<'_>,
        config: &Path,
    ) -> Result<Snapshot> {
        let catalog = self.observe_table(session, self.catalog_scope())?;
        let directory = if catalog.is_recovery() {
            self.recovery()
        } else {
            self.clone()
        };
        directory.project(
            session,
            directory.discovery_records_from(session, &catalog)?,
            &Policy::read(config)?,
        )
    }
    /// Whether `id` may be repaired now (`now`, seconds since the Unix epoch, places the ring
    /// windows): the catalog, a ring table of the current window, or a subscribed declaration.
    pub(crate) fn permits(
        &self,
        session: &Session<'_>,
        policy: &Policy,
        id: Digest,
        now: u64,
    ) -> Result<bool> {
        if !policy.includes(self.catalog_scope().digest(), id) {
            return Ok(false);
        }
        if id == self.catalog_scope().digest() {
            return Ok(true);
        }
        if self.ring_permits(session, id, now)? {
            return Ok(true);
        }
        Ok(self
            .project(session, self.discovery_records(session)?, policy)?
            .entries
            .get(&id)
            .is_some_and(|entry| entry.state == State::Subscribed))
    }
    /// Observe settled subscriptions without enrolling or reserving a slot.
    /// None asks the caller to acquire a writer for first-time setup.
    pub(crate) fn existing_subscriptions(
        &self,
        session: &Session<'_>,
        policy: &Policy,
    ) -> Result<Option<BTreeMap<Digest, (Scope, PathBuf)>>> {
        let scope = self.catalog_scope();
        if !policy.enabled || policy.excluded.contains(&scope.digest()) {
            return Ok(Some(BTreeMap::new()));
        }
        let Some(catalog) = self.observe_store_if_exists(session, scope)? else {
            return Ok(None);
        };
        let snapshot = self.project(
            session,
            self.discovery_records_from(session, &catalog)?,
            policy,
        )?;
        let mut tables = BTreeMap::from([(scope.digest(), (scope, self.path(scope)?))]);
        for entry in snapshot
            .entries
            .values()
            .filter(|entry| entry.state == State::Subscribed)
        {
            let path = self.path(entry.scope)?;
            if !path.try_exists()? {
                return Ok(None);
            }
            tables.insert(entry.scope.digest(), (entry.scope, path));
        }
        // Also seal a reservation whose earlier parent barrier failed. A
        // barrier creates no slot and needs no control-journal writer.
        aterm_durable::durable_dir(&self.root)?;
        Ok(Some(tables))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCatalogSubscription",
            action = "Discover",
            project = "aterm_db::catalog::tests::subscription_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCatalogSubscription",
            action = "Conflict",
            project = "aterm_db::catalog::tests::subscription_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCatalogSubscription",
            action = "Exclude",
            project = "aterm_db::catalog::tests::subscription_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCatalogSubscription",
            action = "Include",
            project = "aterm_db::catalog::tests::subscription_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCatalogSubscription",
            action = "Disable",
            project = "aterm_db::catalog::tests::subscription_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCatalogSubscription",
            action = "Enable",
            project = "aterm_db::catalog::tests::subscription_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCatalogSubscription",
            action = "Quota",
            project = "aterm_db::catalog::tests::subscription_transition"
        )
    )]
    pub(crate) fn subscriptions(
        &self,
        session: &Session<'_>,
        policy: &Policy,
    ) -> Result<BTreeMap<Digest, (Scope, PathBuf)>> {
        let scope = self.catalog_scope();
        if !policy.enabled || policy.excluded.contains(&scope.digest()) {
            return Ok(BTreeMap::new());
        }
        let snapshot = self.project(session, self.discovery_records(session)?, policy)?;
        let mut tables = BTreeMap::from([(scope.digest(), (scope, self.path(scope)?))]);
        for entry in snapshot
            .entries
            .values()
            .filter(|entry| entry.state == State::Subscribed)
        {
            let scope = entry.scope;
            let path = self.path(scope)?;
            if !path.try_exists()? {
                // Reserve the slot before advertising it, without opening or
                // replaying the application journal. One parent barrier seals
                // the entire batch of new private directory names.
                aterm_uds::private_file::create_private_dir(&path)?;
            }
            tables.insert(scope.digest(), (scope, path));
        }
        // Also complete an earlier create whose parent barrier failed.
        aterm_durable::durable_dir(&self.root)?;
        Ok(tables)
    }
    /// Withdrawing a creator cannot erase discovery of already-retained tables:
    /// they may carry outstanding liabilities or root-signed stops. Historical
    /// declarations only retain existing slots, never admit a new table. Local
    /// exclusion and descriptor-conflict checks still run in `project`.
    fn discovery_records(&self, session: &Session<'_>) -> Result<Vec<crate::table::Record>> {
        let catalog = if session.has_control_writer() {
            self.catalog(session)?
        } else {
            self.observe_store(session, self.catalog_scope())?
        };
        self.discovery_records_from(session, &catalog)
    }
    fn discovery_records_from(
        &self,
        session: &Session<'_>,
        catalog: &Store,
    ) -> Result<Vec<crate::table::Record>> {
        let mut records = catalog.catalog(session)?;
        let mut seen: BTreeSet<_> = records.iter().map(|r| r.header().id).collect();
        let (retained, _) = self.retained()?;
        for evidence in catalog.metadata_evidence(session)? {
            let Some(record) = evidence.record else {
                continue;
            };
            let Ok(descriptor) = Descriptor::decode(self.trust, record.payload()) else {
                continue;
            };
            if retained.contains(&descriptor.scope.digest()) && seen.insert(record.header().id) {
                records.push(record);
            }
        }
        // The eligible prefix keeps its original causal order and new-table
        // quota choices. The appended history only describes retained slots.
        Ok(records)
    }

    fn project(
        &self,
        session: &Session<'_>,
        records: Vec<crate::table::Record>,
        policy: &Policy,
    ) -> Result<Snapshot> {
        let mut snapshot = Snapshot {
            recovery: self.is_recovery(),
            ..Snapshot::default()
        };
        let mut created: BTreeMap<Actor, BTreeSet<Digest>> = BTreeMap::new();
        let mut order = Vec::new();
        for record in records {
            let Ok(descriptor) = Descriptor::decode(self.trust, record.payload()) else {
                snapshot.inert += 1;
                continue;
            };
            if descriptor.scope == self.catalog_scope() {
                snapshot.inert += 1;
                continue;
            }
            let id = descriptor.scope.digest();
            let actor = record.header().actor;
            let nominated = created.entry(actor).or_default();
            if nominated.len() < PER_CREATOR {
                nominated.insert(id);
            }
            if !snapshot.entries.contains_key(&id) {
                order.push(id);
            }
            snapshot
                .entries
                .entry(id)
                .or_insert_with(|| Entry {
                    scope: descriptor.scope,
                    declarations: Vec::new(),
                    state: State::Subscribed,
                })
                .declarations
                .push(Declaration {
                    descriptor,
                    actor,
                    operation: record.header().id,
                    authored_ms: record.authored_ms(),
                });
        }
        // Already enrolled tables retain their storage slots. A later low
        // digest cannot evict one, nor can exclusion silently reclaim custody.
        // The rings' and the reserved tables' slots stay theirs: their storage counts against
        // none of these.
        let (retained, mut total) = self.retained()?;
        let count = self.catalog_count(session, &retained, total);
        let mut free = (CATALOG_SLOTS + 1).saturating_sub(count);
        let compiled = reserved(self.trust);
        let mut reserved = BTreeSet::new();
        for (id, entry) in &mut snapshot.entries {
            let first = &entry.declarations[0].descriptor;
            if compiled.contains(first) {
                reserved.insert(*id);
            }
            entry.state = if entry.declarations.iter().any(|d| &d.descriptor != first) {
                State::Conflict
            } else if !policy.includes(self.catalog_scope().digest(), *id) {
                State::Excluded
            } else if !retained.contains(id)
                && !reserved.contains(id)
                && !entry
                    .declarations
                    .iter()
                    .any(|d| created[&d.actor].contains(id))
            {
                State::CreatorLimit
            } else {
                State::Subscribed
            };
        }
        // The reserved tables first, whatever their causal order: each takes its own slot while
        // storage has room for one more directory at all, which it lacks only on a Mac whose
        // retained tables already fill every slot (`room`).
        for id in &order {
            let entry = snapshot
                .entries
                .get_mut(id)
                .expect("ordered declaration exists");
            if reserved.contains(id) && entry.state == State::Subscribed && !retained.contains(id) {
                if total > MAX_TABLES {
                    entry.state = State::Capacity;
                } else {
                    total += 1;
                }
            }
        }
        for id in order {
            let entry = snapshot
                .entries
                .get_mut(&id)
                .expect("ordered declaration exists");
            if !reserved.contains(&id)
                && entry.state == State::Subscribed
                && !retained.contains(&id)
            {
                if free == 0 {
                    entry.state = State::Capacity;
                } else {
                    free -= 1;
                }
            }
        }
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests;
