// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kernel exclusion plumbing only. A ready observation plus these locks is NOT
//! a complete census, drained-process proof, credential grant or publication permit.

use super::*;
use crate::harness::{codex_session_slot::Caller, codex_source_registry};

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
#[cfg(test)]
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
    #[test]
    fn borrowed_family_reenters_without_relocking_and_other_grants_remain_live() {
        let home = aterm_tempfile::tempdir().unwrap();
        let a = fixture(home.path(), "grant-a", 1);
        let alias = fixture(home.path(), "grant-a", 9);
        let b = fixture(home.path(), "grant-b", 1);
        let work = FamilyWork::acquire(FamilySource::Fixture(&a)).unwrap();
        assert!(FamilyExclusive::acquire(FamilySource::Fixture(&alias)).is_err());
        drop(work);
        let exclusive = FamilyExclusive::acquire(FamilySource::Fixture(&a)).unwrap();
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
