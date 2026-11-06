// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright 2026 Andrew Yates

//! Restoration of original secret-backed ownership, never a new admission.
use super::*;
use crate::recovery_material::{RecoveryAuthority, RecoveryUnit, commitment};

/// Original settlement ownership restored from its private secret capability.
/// This is not evidence that its work ended and cannot issue or launch work.
pub struct RecoveredReservation {
    authority: RecoveryAuthority,
    leases: Vec<RetainedLease>,
}
impl RecoveredReservation {
    /// Host context originally committed before grant.
    pub fn context(&self) -> [u8; 32] {
        self.authority.context()
    }
    /// Original declared bundle width, not a newly granted width.
    pub fn width(&self) -> NonZeroU32 {
        self.authority.width()
    }
    /// Number of original records still present. A partial publication or
    /// previously settled subset can make this less than the declared width.
    pub fn retained_units(&self) -> usize {
        self.leases.len()
    }
    /// Observe only the exact currently owned durable debt generations.
    pub fn identities(&self) -> io::Result<Vec<RetainedLeaseIdentity>> {
        self.leases.iter().map(RetainedLease::identity).collect()
    }
    /// Exercise the restored ORIGINAL settlement authority once.
    /// The custodian MUST first establish authenticated original memory-cohort
    /// closure or the applicable same-host later-boot closure. Possession of
    /// this capability, a PID, an exit code or these observations is not closure.
    /// A write error may have consumed a unit; never retry a consumed unit.
    pub fn settle(mut self) -> io::Result<()> {
        while let Some(lease) = self.leases.pop() {
            lease.settle()?;
        }
        Ok(())
    }
}
impl RetainedPool {
    pub(crate) fn manifest_identity(&self) -> io::Result<(u64, u64)> {
        self.validate()?;
        Ok((self.0.manifest.dev as u64, self.0.manifest.ino as u64))
    }
    pub(crate) fn try_acquire_recoverable(
        &self,
        owner: Owner,
        width: NonZeroU32,
        domain: &[u8; 32],
        context: &[u8; 32],
        unit: &RecoveryUnit,
    ) -> io::Result<Option<RetainedLease>> {
        self.acquire_with(owner, unit.nonce, |slot| {
            Some(commitment(domain, owner, width, context, unit, slot))
        })
    }
    pub(crate) fn restore_original(
        &self,
        authority: RecoveryAuthority,
    ) -> io::Result<RecoveredReservation> {
        self.validate()?;
        let units = authority
            .units()
            .iter()
            .enumerate()
            .map(|(index, unit)| (unit.nonce, index))
            .collect::<std::collections::BTreeMap<_, _>>();
        if units.len() != authority.units().len() {
            return Err(invalid("duplicate recovery generation"));
        }
        let mut leases = Vec::new();
        for slot in 0..self.0.slots.get() {
            let mut candidate = Candidate::lock(self.open_slot(slot, true)?)?;
            match read_record(candidate.file()?, slot)? {
                State::Retained { owner, nonce, recovery: Some(observed) }
                    if units.contains_key(&nonce) =>
                {
                    let unit = &authority.units()[units[&nonce]];
                    let expected = commitment(
                        &authority.domain(),
                        authority.owner(),
                        authority.width(),
                        &authority.context(),
                        unit,
                        slot,
                    );
                    if owner != authority.owner() || expected != observed {
                        return Err(invalid("original recovery commitment differs"));
                    }
                    leases.push(RetainedLease {
                        pool: self.clone(),
                        slot,
                        owner,
                        nonce,
                        recovery: Some(observed),
                        file: candidate.take()?,
                    });
                }
                State::Retained { nonce, recovery: None, .. } if units.contains_key(&nonce) => {
                    return Err(invalid("public nonce cannot restore settlement authority"));
                }
                _ => {}
            }
        }
        self.validate()?;
        if leases.len() > authority.width().get() as usize {
            return Err(invalid("recovery bundle exceeds original width"));
        }
        Ok(RecoveredReservation { authority, leases })
    }
}

// Unmatched/failed probes unlock before close, even if a concurrent fork
// temporarily copied the descriptor. Only a validated original capability
// takes ownership without this unlock.
struct Candidate(Option<File>);
impl Candidate {
    fn lock(file: File) -> io::Result<Self> {
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
            std::fs::TryLockError::Error(error) => error,
        })?;
        Ok(Self(Some(file)))
    }
    fn file(&mut self) -> io::Result<&mut File> {
        self.0.as_mut().ok_or_else(|| invalid("missing recovery probe"))
    }
    fn take(&mut self) -> io::Result<File> {
        self.0.take().ok_or_else(|| invalid("missing original recovery file"))
    }
}
impl Drop for Candidate {
    fn drop(&mut self) {
        if let Some(file) = &self.0 {
            let _ = file.unlock();
        }
    }
}
