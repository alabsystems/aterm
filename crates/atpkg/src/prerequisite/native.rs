// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The SAME signed prerequisite and native executable/runtime inputs. This
//! neither selects an SDK/seed nor supplies admission, GO or native settlement.
use super::HeldTool;
use crate::private_input::Refused;
use std::{
    io,
    os::{fd::OwnedFd, unix::fs::FileExt as _},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use trust_resource_provenance::program::HeldProgram;

/// Original signed package/lease beside its actual native input pins.
/// Not Clone or recoverable from a path, digest, receipt or public outcome.
#[must_use = "retain the whole signed/native input through actual native settlement"]
pub struct HeldNativeTool {
    original: Box<HeldTool>,
    native: HeldProgram,
    deadline: Instant,
    revoked: AtomicBool,
}

// Capture owns no child. On an explicit error the SAME signed Box is returned;
// unwind/unknown completion conservatively retains that actual lease rather
// than letting temporary native-capture cleanup dispose the original owner.
struct CapturingSigned(Option<Box<HeldTool>>);
impl Drop for CapturingSigned {
    fn drop(&mut self) {
        if let Some(original) = self.0.take() {
            std::mem::forget(original);
        }
    }
}
fn alive(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}
fn checked(original: &HeldTool, deadline: Instant) -> io::Result<()> {
    if original.creator != std::process::id() || original.native_attempt != Some(deadline) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "native prerequisite belongs to another process",
        ));
    }
    alive(deadline)?;
    original.validate().map_err(io::Error::other)?;
    alive(deadline)
}
fn original_sha256(original: &HeldTool, deadline: Instant) -> io::Result<[u8; 32]> {
    checked(original, deadline)?;
    if original.image_identity.len > 512 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native prerequisite image exceeds capture bound",
        ));
    }
    let mut hash = aterm_digest::Sha256::new();
    let mut buffer = [0; 65536];
    let mut offset = 0;
    while offset < original.image_identity.len {
        checked(original, deadline)?;
        let length = buffer
            .len()
            .min((original.image_identity.len - offset) as usize);
        let count = original.image.read_at(&mut buffer[..length], offset)?;
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        hash.update(&buffer[..count]);
        offset += count as u64;
    }
    checked(original, deadline)?;
    Ok(hash.finalize())
}
impl HeldTool {
    /// Bind this actual already authenticated signed prerequisite to its SAME
    /// native program/runtime pins, before independently admitted dispatch.
    /// Hash the original opened image positionally; a caller cannot nominate
    /// different bytes, a namespace, current selection or later endpoint.
    /// Capture is a bounded transaction over newly opened descriptions. No
    /// external count/limit observation is a reservation or success condition:
    /// actual EMFILE/ENFILE/concurrent-open failures refuse and close only the
    /// transaction's newly owned native descriptions. Its original signed
    /// lease/image survive in the returned SAME Box, with native retry revoked.
    /// The signed package lease protects its immutable payload under the store
    /// protocol; these pins add no source, SDK, seed or execution authority.
    ///
    /// # Errors
    /// Foreign creator, expired original endpoint, original lease/image drift,
    /// unsupported native runtime or pin capture failure. The SAME original
    /// signed tool/lease and first endpoint are returned, with no native target
    /// dispatched. A second native attempt on that same owner refuses; unwind
    /// retains the original lease instead of fabricating successful capture.
    pub fn into_native(self, deadline: Instant) -> Result<HeldNativeTool, Refused<Box<Self>>> {
        self.into_native_with(deadline, || Ok(()))
    }

    pub(super) fn into_native_with(
        mut self,
        deadline: Instant,
        before_native: impl FnOnce() -> io::Result<()>,
    ) -> Result<HeldNativeTool, Refused<Box<Self>>> {
        let repeated = self.native_attempt.is_some();
        if !repeated {
            self.native_attempt = Some(deadline);
        }
        let mut retained = CapturingSigned(Some(Box::new(self)));
        let capture = (|| {
            let original = retained.0.as_ref().expect("same signed capture owner");
            if repeated {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "original signed owner already attempted native capture",
                ));
            }
            let digest = original_sha256(original, deadline)?;
            before_native()?;
            let native =
                HeldProgram::open(original.root(), original.physical_path(), &digest, deadline)?;
            native.validate(deadline)?;
            checked(original, deadline)?;
            Ok(native)
        })();
        let original = retained.0.take().expect("same signed capture owner");
        match capture {
            Ok(native) => Ok(HeldNativeTool {
                original,
                native,
                deadline,
                revoked: AtomicBool::new(false),
            }),
            Err(error) => Err(Refused::new(error, original)),
        }
    }
}
impl HeldNativeTool {
    /// Restrictive original endpoint DATA for joining this SAME retained
    /// signed input to another actual owner. This cannot renew the lease,
    /// validate a foreign process, or create dispatch/SDK/Seed authority.
    #[must_use]
    pub const fn original_deadline(&self) -> Instant {
        self.deadline
    }
    /// Original package, native pins and absolute endpoint, with no renewal.
    /// An observed failure permanently revokes THIS native use; actual package
    /// and native descriptions remain held, including on refusal.
    /// # Errors
    /// Original identity, lease, creator or endpoint no longer validates.
    pub fn check_current(&self) -> io::Result<()> {
        if self.revoked.load(Ordering::Acquire) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let current = (|| {
            checked(&self.original, self.deadline)?;
            self.native.validate(self.deadline)?;
            checked(&self.original, self.deadline)
        })();
        if current.is_err() {
            self.revoked.store(true, Ordering::Release);
        }
        current
    }
    /// Borrow the SAME signed package owner; this creates no SDK or seed role.
    /// # Errors
    /// As [`Self::check_current`].
    pub fn original(&self) -> io::Result<&HeldTool> {
        self.check_current()?;
        Ok(&self.original)
    }
    /// Borrow the SAME native pins for the independently owned launch profile.
    /// # Errors
    /// As [`Self::check_current`].
    pub fn program(&self) -> io::Result<&HeldProgram> {
        self.check_current()?;
        Ok(&self.native)
    }
    /// Observe this actual child's original loaded image before GO. The caller
    /// separately retains that child and its real NoFork/containment authority;
    /// a PID observation supplies no launch, signal or retirement rights.
    /// # Errors
    /// Original inputs drifted, endpoint expired or loaded image differs.
    pub fn validate_loaded(
        &self,
        child: trust_resource_provenance::process::Identity,
    ) -> io::Result<()> {
        let current = (|| {
            self.check_current()?;
            self.native.validate_loaded(child, self.deadline)?;
            self.check_current()
        })();
        if current.is_err() {
            self.revoked.store(true, Ordering::Release);
        }
        current
    }
    /// Duplicate ONLY the original close-only signed package GC lease for
    /// fixed native inheritance. Recipients may never unlock/relock or change
    /// shared flags/cursors. The whole original still needs native settlement.
    /// # Errors
    /// Original checks or actual lease duplication fails; no adoption fallback.
    pub fn lease_descriptor(&self) -> io::Result<OwnedFd> {
        let current = (|| {
            self.check_current()?;
            let copy = aterm_uds::kept_out(|| self.original.lease_descriptor())?;
            self.check_current()?;
            Ok(copy)
        })();
        if current.is_err() {
            self.revoked.store(true, Ordering::Release);
        }
        current
    }
}
