// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kernel exclusion plumbing only. A ready observation plus these locks is NOT
//! a complete census, drained-process proof, credential grant or publication permit.

use super::*;
use crate::harness::{codex_session_slot::Caller, codex_source_registry};
use std::sync::Arc;

pub enum FamilySource<'a> {
    Manager(&'a codex_source_registry::ReadySourceProof),
    Reader(&'a codex_source_registry::ReaderSourceProof),
    #[cfg(test)]
    Fixture(&'a Fixture),
}

#[derive(Clone, PartialEq, Eq)]
struct Identity {
    caller: Caller,
    version: u64,
    family: String,
    gate: SourceGateIdentity,
}

fn family_key(caller: &Caller, binding: &impl serde::Serialize) -> Result<String, &'static str> {
    let bytes = aterm_json::to_vec(&("aterm/codex-actor-family-gate/v1", caller, binding))
        .map_err(|_| UNAVAILABLE)?;
    Ok(aterm_codec::hex::encode_lower(
        &aterm_digest::Sha256::digest(bytes),
    ))
}

impl FamilySource<'_> {
    fn current(&self) -> bool {
        match self {
            Self::Manager(proof) => proof.current(),
            Self::Reader(proof) => proof.current(),
            #[cfg(test)]
            Self::Fixture(proof) => proof.live.load(std::sync::atomic::Ordering::Acquire),
        }
    }
    fn identity(&self) -> Result<Identity, &'static str> {
        if !self.current() {
            return Err(UNAVAILABLE);
        }
        #[cfg(test)]
        if let Self::Fixture(proof) = self {
            return Ok(proof.identity.clone());
        }
        let (caller, version, source, gate) = match self {
            Self::Manager(proof) => proof.gate_source(),
            Self::Reader(proof) => proof.gate_source(),
            #[cfg(test)]
            Self::Fixture(_) => unreachable!(),
        };
        Ok(Identity {
            caller: caller.clone(),
            version,
            family: family_key(caller, source.binding())?,
            gate: gate.clone(),
        })
    }
}

#[cfg(unix)]
fn family_gate(identity: &Identity) -> Result<Held, &'static str> {
    let root = identity
        .gate
        .path
        .parent()
        .ok_or(UNAVAILABLE)?
        .join("families");
    private_directory(&root)?;
    let family = root.join(&identity.family);
    private_directory(&family)?;
    open_gate(&family)
}

#[cfg(not(unix))]
fn family_gate(_: &Identity) -> Result<Held, &'static str> {
    Err("native credential custody is unsupported on this platform")
}

pub struct FamilyWork<'source> {
    source: FamilySource<'source>,
    identity: Identity,
    global: SourceWorkLease,
    held: Held,
}
impl<'source> FamilyWork<'source> {
    pub fn acquire(source: FamilySource<'source>) -> Result<Self, &'static str> {
        let identity = source.identity()?;
        // Every path uses global-before-family ordering. Other families remain live.
        let global = identity.gate.shared()?;
        let held = family_gate(&identity)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "this actor-family lease owns its actual shared kernel lock"
        )]
        held.file.try_lock_shared().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => BUSY,
            std::fs::TryLockError::Error(_) => UNAVAILABLE,
        })?;
        let work = Self {
            source,
            identity,
            global,
            held,
        };
        work.current()?;
        Ok(work)
    }
    pub fn current(&self) -> Result<(), &'static str> {
        if self.source.identity()? != self.identity {
            return Err(UNAVAILABLE);
        }
        self.global.current()?;
        self.held.current()
    }
}

pub struct FamilyExclusive<'source> {
    source: FamilySource<'source>,
    identity: Identity,
    global: SourceWorkLease,
    held: Held,
}
impl<'source> FamilyExclusive<'source> {
    pub fn acquire(source: FamilySource<'source>) -> Result<Self, &'static str> {
        let identity = source.identity()?;
        let global = identity.gate.shared()?;
        let held = family_gate(&identity)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "this actor-family guard owns its actual exclusive kernel lock"
        )]
        held.file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => BUSY,
            std::fs::TryLockError::Error(_) => UNAVAILABLE,
        })?;
        let guard = Self {
            source,
            identity,
            global,
            held,
        };
        guard.current()?;
        Ok(guard)
    }
    pub fn current(&self) -> Result<(), &'static str> {
        if self.source.identity()? != self.identity {
            return Err(UNAVAILABLE);
        }
        self.global.current()?;
        self.held.current()
    }
    pub fn borrow<'guard>(&'guard self) -> Result<SourceBorrow<'guard, 'source>, &'static str> {
        self.current()?;
        Ok(SourceBorrow { guard: self })
    }
}

/// Borrows only this actual held family guard. No global-exclusive or credential
/// authority is implied, and no second shared family lock is acquired.
pub struct SourceBorrow<'guard, 'source> {
    guard: &'guard FamilyExclusive<'source>,
}
impl SourceBorrow<'_, '_> {
    pub fn current(&self) -> Result<(), &'static str> {
        self.guard.current()
    }
    pub fn caller(&self) -> &Caller {
        &self.guard.identity.caller
    }
    pub fn version(&self) -> u64 {
        self.guard.identity.version
    }
    pub fn matches(&self, source: FamilySource<'_>) -> Result<(), &'static str> {
        self.current()?;
        if source.identity()? != self.guard.identity {
            return Err(UNAVAILABLE);
        }
        Ok(())
    }
}

/// Owned only by a registry phase. Its source proof survives legitimate member
/// retirement without claiming an old Ready observation is still current.
const FAMILY_EXCLUSIVE: u8 = 1;
const FAMILY_SHARED: u8 = 2;
const FAMILY_RETIRING: u8 = 3;
const FAMILY_RELEASED: u8 = 4;
struct OwnedFamily {
    held: Held,
    mode: std::sync::atomic::AtomicU8,
}
impl OwnedFamily {
    fn exclusive(&self) -> Result<(), &'static str> {
        if self.mode.load(std::sync::atomic::Ordering::Acquire) != FAMILY_EXCLUSIVE {
            return Err(UNAVAILABLE);
        }
        self.held.current()
    }
    // Private kernel transition; the public target path checks its actual
    // durable Applied receipt before entering here.
    fn share_after_admission(&self) -> Result<(), &'static str> {
        if matches!(
            self.mode.load(std::sync::atomic::Ordering::Acquire),
            FAMILY_RETIRING | FAMILY_RELEASED
        ) {
            return Err(UNAVAILABLE);
        }
        if self.mode.load(std::sync::atomic::Ordering::Acquire) == FAMILY_SHARED {
            return self.held.current();
        }
        self.mode.store(0, std::sync::atomic::Ordering::Release);
        #[expect(
            clippy::disallowed_methods,
            reason = "actual Applied receipt authorizes retained same-family lock handoff"
        )]
        self.held
            .file
            .try_lock_shared()
            .map_err(|error| match error {
                std::fs::TryLockError::WouldBlock => BUSY,
                std::fs::TryLockError::Error(_) => UNAVAILABLE,
            })?;
        self.held.current()?;
        self.mode
            .store(FAMILY_SHARED, std::sync::atomic::Ordering::Release);
        Ok(())
    }
    fn exclusive_after_census(&self) -> Result<(), &'static str> {
        match self.mode.load(std::sync::atomic::Ordering::Acquire) {
            FAMILY_EXCLUSIVE => return self.held.current(),
            FAMILY_RETIRING | FAMILY_RELEASED => return Err(UNAVAILABLE),
            _ => (),
        }
        // Conversion may release SH before failing. Mode zero owns uncertainty
        // and must not reopen a borrower until a real lock acknowledgement.
        self.mode.store(0, std::sync::atomic::Ordering::Release);
        #[expect(
            clippy::disallowed_methods,
            reason = "actual sealed census transfers the existing family FD"
        )]
        self.held.file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => BUSY,
            std::fs::TryLockError::Error(_) => UNAVAILABLE,
        })?;
        self.held.current()?;
        self.mode
            .store(FAMILY_EXCLUSIVE, std::sync::atomic::Ordering::Release);
        Ok(())
    }
    fn release_after_effect(&self) -> Result<(), &'static str> {
        if self.mode.load(std::sync::atomic::Ordering::Acquire) == FAMILY_RELEASED {
            return Ok(());
        }
        self.mode
            .store(FAMILY_RETIRING, std::sync::atomic::Ordering::Release);
        self.held.file.unlock().map_err(|_| UNAVAILABLE)?;
        self.mode
            .store(FAMILY_RELEASED, std::sync::atomic::Ordering::Release);
        Ok(())
    }
    fn borrowed(&self) -> Result<(), &'static str> {
        if !matches!(
            self.mode.load(std::sync::atomic::Ordering::Acquire),
            FAMILY_EXCLUSIVE | FAMILY_SHARED
        ) {
            return Err(UNAVAILABLE);
        }
        self.held.current()
    }
}

pub(in crate::harness) struct ReservationFamilyExclusive {
    source: codex_source_registry::SealingSource,
    identity: Identity,
    global: Arc<SourceWorkLease>,
    held: Arc<OwnedFamily>,
    _scope: crate::harness::account_work::Scope,
}
impl ReservationFamilyExclusive {
    pub(in crate::harness) fn acquire(
        source: codex_source_registry::SealingSource,
    ) -> Result<Self, &'static str> {
        if !source.current() {
            return Err(UNAVAILABLE);
        }
        let scope = source.retained_scope().map_err(|_| UNAVAILABLE)?;
        let identity = Identity {
            caller: source.caller().clone(),
            version: 0, // phase identity is retained by source, never a Ready version
            family: family_key(source.caller(), source.source().binding())?,
            gate: source.gate().clone(),
        };
        let global = Arc::new(identity.gate.shared()?);
        let held = family_gate(&identity)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "registry phase retains this actual exclusive family lock"
        )]
        held.file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => BUSY,
            std::fs::TryLockError::Error(_) => UNAVAILABLE,
        })?;
        let guard = Self {
            source,
            identity,
            global,
            held: Arc::new(OwnedFamily {
                held,
                mode: std::sync::atomic::AtomicU8::new(FAMILY_EXCLUSIVE),
            }),
            _scope: scope,
        };
        guard.current()?;
        Ok(guard)
    }
    /// Reuse only the actual normalized source claim selected by the native
    /// census phase. No new global share is acquired behind a queued writer.
    pub(in crate::harness) fn reclaim(
        source: codex_source_registry::SealingSource,
        retained: &TargetFamilyLease,
    ) -> Result<Self, &'static str> {
        if !source.current()
            || source.caller() != &retained.identity.caller
            || source.gate() != &retained.identity.gate
            || family_key(source.caller(), source.source().binding())? != retained.identity.family
        {
            return Err(UNAVAILABLE);
        }
        let scope = source.retained_scope().map_err(|_| UNAVAILABLE)?;
        retained.global.current()?;
        if retained.global.identity()? != retained.identity.gate {
            return Err(UNAVAILABLE);
        }
        let held = retained.held.owned().clone();
        held.exclusive_after_census()?;
        let guard = Self {
            source,
            identity: retained.identity.clone(),
            global: retained.global.clone(),
            held,
            _scope: scope,
        };
        guard.current()?;
        Ok(guard)
    }

    pub(in crate::harness) fn release_source_after_applied(
        &self,
        receipt: &crate::harness::codex_assignment::target_ack::AppliedReceipt,
        commit: &crate::harness::codex_assignment::AssignmentCommit,
    ) -> Result<(), &'static str> {
        if !receipt.matches(commit) || !self.source.matches_applied(commit) {
            return Err(UNAVAILABLE);
        }
        self.global.current()?;
        if self.global.identity()? != self.identity.gate {
            return Err(UNAVAILABLE);
        }
        if family_key(commit.caller(), commit.binding())? == self.identity.family {
            self.held.share_after_admission()
        } else {
            self.held.release_after_effect()
        }
    }

    /// Acquire only physical target-family custody under the SAME actual global
    /// shared lease. This is not a credential, installation or factory grant.
    pub(in crate::harness) fn target_resource(
        &self,
        target: &crate::harness::codex_assignment::prepare::TargetIntent,
    ) -> Result<TargetFamilyLease, &'static str> {
        self.current()?;
        if !self.source.matches_target(target) || target.caller() != &self.identity.caller {
            return Err(UNAVAILABLE);
        }
        let binding = target.target_binding().ok_or(UNAVAILABLE)?;
        let identity = Identity {
            caller: target.caller().clone(),
            version: 0,
            family: family_key(target.caller(), binding)?,
            gate: self.identity.gate.clone(),
        };
        let held = if identity.family == self.identity.family {
            // The actual held same-family exclusion is borrowed, not relocked.
            TargetHeld::Same(self.held.clone())
        } else {
            let held = family_gate(&identity)?;
            #[expect(
                clippy::disallowed_methods,
                reason = "target resource owns its actual shared family lock"
            )]
            held.file.try_lock_shared().map_err(|error| match error {
                std::fs::TryLockError::WouldBlock => BUSY,
                std::fs::TryLockError::Error(_) => UNAVAILABLE,
            })?;
            TargetHeld::Shared(Arc::new(OwnedFamily {
                held,
                mode: std::sync::atomic::AtomicU8::new(FAMILY_SHARED),
            }))
        };
        let resource = TargetFamilyLease {
            identity,
            global: self.global.clone(),
            held,
        };
        resource.current()?;
        self.current()?;
        Ok(resource)
    }
    pub(in crate::harness) fn current(&self) -> Result<(), &'static str> {
        if !self.source.current() || self.source.gate() != &self.identity.gate {
            return Err(UNAVAILABLE);
        }
        self.global.current()?;
        self.held.exclusive()
    }
}
/// Separately retained physical global share + B family. It contains no A
/// exclusive guard or source/target admission authority. New B work must also
/// prove its actual installed assignment and counted member/use admission.
enum TargetHeld {
    Shared(Arc<OwnedFamily>),
    Same(Arc<OwnedFamily>),
}
impl TargetHeld {
    fn owned(&self) -> &Arc<OwnedFamily> {
        match self {
            Self::Shared(held) | Self::Same(held) => held,
        }
    }
    fn current(&self) -> Result<(), &'static str> {
        self.owned().borrowed()
    }
}
pub(in crate::harness) struct TargetFamilyLease {
    identity: Identity,
    global: Arc<SourceWorkLease>,
    held: TargetHeld,
}
impl TargetFamilyLease {
    /// Retain the actual claim without probing under a registry mutex. The
    /// caller must validate current() outside that mutex before using it.
    pub(in crate::harness) fn retained_source_lease(&self) -> Arc<SourceWorkLease> {
        self.global.clone()
    }
    pub(in crate::harness) fn current(&self) -> Result<(), &'static str> {
        self.global.current()?;
        if self.global.identity()? != self.identity.gate {
            return Err(UNAVAILABLE);
        }
        self.held.current()
    }
    pub(in crate::harness) fn restore_after_cancel(
        &self,
        permit: &crate::harness::codex_assignment::prepare::cancel::CancelledSource<'_>,
        source: &crate::harness::codex_session_slot::SourceHandle,
    ) -> Result<(), &'static str> {
        if permit.caller() != &self.identity.caller
            || !permit.matches_source(source)
            || family_key(&self.identity.caller, source.binding())? != self.identity.family
        {
            return Err(UNAVAILABLE);
        }
        self.global.current()?;
        if self.global.identity()? != self.identity.gate {
            return Err(UNAVAILABLE);
        }
        self.held.owned().share_after_admission()
    }

    /// Only the actual durable ACK receipt may turn same-family exclusion into
    /// an ordinary B resource. Conversion is not assumed atomic: mode zero
    /// denies every borrower until the shared kernel claim is acknowledged.
    pub(in crate::harness) fn normalize_after_applied(
        &self,
        receipt: &crate::harness::codex_assignment::target_ack::AppliedReceipt,
        assignment: &crate::harness::codex_assignment::AssignmentCommit,
    ) -> Result<(), &'static str> {
        if !receipt.matches(assignment)
            || self.identity.caller != *assignment.caller()
            || family_key(assignment.caller(), assignment.binding())? != self.identity.family
        {
            return Err(UNAVAILABLE);
        }
        self.global.current()?;
        if self.global.identity()? != self.identity.gate {
            return Err(UNAVAILABLE);
        }
        match &self.held {
            TargetHeld::Shared(held) => held.borrowed(),
            TargetHeld::Same(held) => held.share_after_admission(),
        }
    }
    pub(in crate::harness) fn matches_target(
        &self,
        target: &crate::harness::codex_assignment::prepare::TargetIntent,
    ) -> bool {
        target.caller() == &self.identity.caller
            && target.target_binding().is_some_and(|binding| {
                family_key(target.caller(), binding).is_ok_and(|key| key == self.identity.family)
            })
    }
    pub(in crate::harness) fn gate_identity(&self) -> &SourceGateIdentity {
        &self.identity.gate
    }
}
impl std::fmt::Debug for TargetFamilyLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TargetFamilyLease(redacted)")
    }
}

impl std::fmt::Debug for ReservationFamilyExclusive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReservationFamilyExclusive(redacted)")
    }
}

macro_rules! redacted {
    ($($type:ident),*) => {$ (
        impl std::fmt::Debug for $type<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($type), "(redacted)"))
            }
        }
    )*};
}
redacted!(FamilyWork, FamilyExclusive);
impl std::fmt::Debug for SourceBorrow<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceBorrow(redacted)")
    }
}

#[cfg(test)]
pub struct Fixture {
    identity: Identity,
    live: std::sync::atomic::AtomicBool,
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn fixture(home: &Path, grant: &str, version: u64) -> Fixture {
        // Kernel-lock controls only; never a qualified peer, native grant or census.
        let gate = SourceWorkLease::for_home(home).unwrap().identity().unwrap();
        let caller = Caller {
            sid: "gate-fixture".into(),
            pid: std::process::id(),
            birth: 1,
        };
        Fixture {
            identity: Identity {
                family: family_key(&caller, &grant).unwrap(),
                caller,
                version,
                gate,
            },
            live: true.into(),
        }
    }

    fn released_family(source: &Fixture) -> FamilyExclusive<'_> {
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the family lock after its last descriptor closes",
            || match FamilyExclusive::acquire(FamilySource::Fixture(source)) {
                Ok(guard) => Some(guard),
                Err(BUSY) => None,
                Err(error) => panic!("family lock unavailable after release: {error}"),
            },
        )
    }

    #[test]
    fn borrowed_family_reenters_without_relocking_and_other_grants_remain_live() {
        let home = aterm_tempfile::tempdir().unwrap();
        let a = fixture(home.path(), "grant-a", 1);
        let alias = fixture(home.path(), "grant-a", 9);
        let b = fixture(home.path(), "grant-b", 1);
        let work = FamilyWork::acquire(FamilySource::Fixture(&a)).unwrap();
        assert!(FamilyExclusive::acquire(FamilySource::Fixture(&alias)).is_err());
        let retained = work.held.file.try_clone().unwrap();
        drop(work);
        assert_eq!(
            FamilyExclusive::acquire(FamilySource::Fixture(&a)).err(),
            Some(BUSY),
            "a second descriptor retains the shared lock after its guard drops"
        );
        drop(retained);
        let exclusive = released_family(&a);
        assert!(FamilyWork::acquire(FamilySource::Fixture(&alias)).is_err());
        let first = exclusive.borrow().unwrap();
        let second = exclusive.borrow().unwrap();
        first.current().unwrap();
        second.current().unwrap();
        assert_eq!(first.version(), 1);
        assert!(first.matches(FamilySource::Fixture(&alias)).is_err());
        assert!(first.matches(FamilySource::Fixture(&b)).is_err());
        let independent = FamilyWork::acquire(FamilySource::Fixture(&b)).unwrap();
        independent.current().unwrap();
        assert!(PublicationLock::for_home(home.path()).is_err());
    }
    #[test]
    fn same_family_handoff_retains_shared_kernel_custody() {
        let home = aterm_tempfile::tempdir().unwrap();
        let source = fixture(home.path(), "grant-a", 1);
        let exclusive = FamilyExclusive::acquire(FamilySource::Fixture(&source)).unwrap();
        let FamilyExclusive {
            global: _global,
            held,
            ..
        } = exclusive;
        let held = OwnedFamily {
            held,
            mode: FAMILY_EXCLUSIVE.into(),
        };
        assert!(FamilyWork::acquire(FamilySource::Fixture(&source)).is_err());
        held.share_after_admission().unwrap();
        held.share_after_admission().unwrap();
        assert!(held.exclusive().is_err());
        held.borrowed().unwrap();
        let work = FamilyWork::acquire(FamilySource::Fixture(&source)).unwrap();
        assert!(FamilyExclusive::acquire(FamilySource::Fixture(&source)).is_err());
        drop(work);
        assert!(FamilyExclusive::acquire(FamilySource::Fixture(&source)).is_err());
        drop(held);
        let _exclusive = released_family(&source);
    }

    #[test]
    fn replaced_family_gate_during_handoff_never_reenables_borrowers() {
        let home = aterm_tempfile::tempdir().unwrap();
        let source = fixture(home.path(), "grant-a", 1);
        let exclusive = FamilyExclusive::acquire(FamilySource::Fixture(&source)).unwrap();
        let FamilyExclusive {
            global: _global,
            held,
            ..
        } = exclusive;
        let held = OwnedFamily {
            held,
            mode: FAMILY_EXCLUSIVE.into(),
        };
        std::fs::rename(&held.held.path, held.held.path.with_extension("old")).unwrap();
        private_file::write_private_new(
            held.held.path.parent().unwrap(),
            "gate",
            b"replacement",
            |_| Ok(()),
        )
        .unwrap();
        assert!(held.share_after_admission().is_err());
        assert!(held.borrowed().is_err());
        assert!(held.exclusive().is_err());
        assert_eq!(held.mode.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn replaced_source_family_cannot_restore_after_failed_upgrade() {
        let home = aterm_tempfile::tempdir().unwrap();
        let source = fixture(home.path(), "grant-b", 1);
        let original = FamilyExclusive::acquire(FamilySource::Fixture(&source)).unwrap();
        let FamilyExclusive {
            global: _global,
            held,
            ..
        } = original;
        let held = OwnedFamily {
            held,
            mode: FAMILY_EXCLUSIVE.into(),
        };
        held.share_after_admission().unwrap();
        std::fs::rename(&held.held.path, held.held.path.with_extension("old")).unwrap();
        private_file::write_private_new(
            held.held.path.parent().unwrap(),
            "gate",
            b"replacement",
            |_| Ok(()),
        )
        .unwrap();
        assert!(held.exclusive_after_census().is_err());
        assert_eq!(held.mode.load(std::sync::atomic::Ordering::Acquire), 0);
        assert!(held.share_after_admission().is_err());
        assert!(held.borrowed().is_err());
        assert_eq!(held.mode.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn competing_fd_blocks_upgrade_and_only_real_shared_reacquisition_restores_borrow() {
        // Physical lock-state controls only; no fabricated census/cancel/ACK.
        let home = aterm_tempfile::tempdir().unwrap();
        let source = fixture(home.path(), "grant-b", 1);
        let exclusive = FamilyExclusive::acquire(FamilySource::Fixture(&source)).unwrap();
        let FamilyExclusive {
            global: _global,
            held,
            ..
        } = exclusive;
        let held = OwnedFamily {
            held,
            mode: FAMILY_EXCLUSIVE.into(),
        };
        held.share_after_admission().unwrap();
        let competing = FamilyWork::acquire(FamilySource::Fixture(&source)).unwrap();
        assert_eq!(held.exclusive_after_census(), Err(BUSY));
        assert_eq!(held.mode.load(std::sync::atomic::Ordering::Acquire), 0);
        assert!(held.borrowed().is_err());
        // An OS failure is never interpreted as still owning the old SH.
        held.share_after_admission().unwrap();
        held.borrowed().unwrap();
        assert!(FamilyExclusive::acquire(FamilySource::Fixture(&source)).is_err());
        // Parallel process controls can briefly inherit this open description
        // between fork and exec. Acknowledge actual competitor retirement,
        // rather than assuming the parent's last FD drop released every copy.
        competing.held.file.unlock().unwrap();
        drop(competing);
        held.exclusive_after_census().unwrap();
        assert!(FamilyWork::acquire(FamilySource::Fixture(&source)).is_err());
    }

    #[test]
    fn repeated_family_epochs_do_not_keep_old_exclusive_or_revive_old_handles() {
        let home = aterm_tempfile::tempdir().unwrap();
        let b = fixture(home.path(), "grant-b", 1);
        let c = fixture(home.path(), "grant-c", 1);
        let first = FamilyExclusive::acquire(FamilySource::Fixture(&b)).unwrap();
        let FamilyExclusive {
            global: _global_b,
            held,
            ..
        } = first;
        let old_b = OwnedFamily {
            held,
            mode: FAMILY_EXCLUSIVE.into(),
        };
        old_b.share_after_admission().unwrap();
        old_b.exclusive_after_census().unwrap();
        let next = FamilyExclusive::acquire(FamilySource::Fixture(&c)).unwrap();
        let FamilyExclusive {
            global: _global_c,
            held,
            ..
        } = next;
        let old_c = OwnedFamily {
            held,
            mode: FAMILY_EXCLUSIVE.into(),
        };
        old_c.share_after_admission().unwrap();
        old_b.release_after_effect().unwrap();
        assert!(old_b.borrowed().is_err());
        assert!(old_b.share_after_admission().is_err());
        assert!(old_b.exclusive_after_census().is_err());
        old_c.exclusive_after_census().unwrap();
        let final_b = FamilyExclusive::acquire(FamilySource::Fixture(&b)).unwrap();
        let FamilyExclusive {
            global: _global_b2,
            held,
            ..
        } = final_b;
        let current_b = OwnedFamily {
            held,
            mode: FAMILY_EXCLUSIVE.into(),
        };
        current_b.share_after_admission().unwrap();
        old_c.release_after_effect().unwrap();
        // Exact historical release cannot unlock the new epoch's distinct FD.
        old_b.release_after_effect().unwrap();
        assert!(FamilyExclusive::acquire(FamilySource::Fixture(&b)).is_err());
        current_b.borrowed().unwrap();
        drop(current_b);
        let _exclusive = released_family(&b);
    }

    #[test]
    fn root_version_liveness_and_replaced_gate_cannot_inherit_a_borrow() {
        let home = aterm_tempfile::tempdir().unwrap();
        let other = aterm_tempfile::tempdir().unwrap();
        let a = fixture(home.path(), "grant-a", 1);
        let foreign = fixture(other.path(), "grant-a", 1);
        let exclusive = FamilyExclusive::acquire(FamilySource::Fixture(&a)).unwrap();
        let independent = FamilyExclusive::acquire(FamilySource::Fixture(&foreign)).unwrap();
        independent.current().unwrap();
        let borrowed = exclusive.borrow().unwrap();
        assert!(borrowed.matches(FamilySource::Fixture(&foreign)).is_err());
        a.live.store(false, std::sync::atomic::Ordering::Release);
        assert!(borrowed.current().is_err());
        a.live.store(true, std::sync::atomic::Ordering::Release);
        let old = exclusive.held.path.with_extension("old");
        std::fs::rename(&exclusive.held.path, old).unwrap();
        private_file::write_private_new(
            exclusive.held.path.parent().unwrap(),
            "gate",
            b"replacement",
            |_| Ok(()),
        )
        .unwrap();
        assert!(borrowed.current().is_err());
    }
}
