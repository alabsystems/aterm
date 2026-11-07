// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Image-specific atomic signalling of an actual owned, unreaped child.
//!
//! This capability is neither serializable nor constructible from process
//! observations. Darwin's audit-token syscall checks PID and image version in
//! the kernel. Symbol availability alone does not promise permission/support.

use crate::spawnfd::Child;
use std::io;

/// The only permitted actions on this captured child image.
#[derive(Clone, Copy, Debug)]
pub enum Action {
    /// Pause this image with SIGSTOP.
    Stop,
    /// Resume this image with SIGCONT.
    Continue,
    /// End this image with SIGKILL.
    Kill,
}

/// A private generational capability captured from an owned unreaped child.
/// It never signals on drop and does not prevent the owner from polling/reaping.
pub struct OwnedChildSignal {
    #[cfg(target_vendor = "apple")]
    original: crate::process::Lineage,
    #[cfg(target_vendor = "apple")]
    token: crate::peer::AuditToken,
    #[cfg(target_vendor = "apple")]
    call: Signal,
}

#[cfg(target_vendor = "apple")]
type Signal = unsafe extern "C" fn(*mut libc::c_void, libc::c_int) -> libc::c_int;

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic owned-child signalling unavailable",
    )
}

#[cfg(target_vendor = "apple")]
fn resolve() -> io::Result<Signal> {
    // SAFETY: Static NUL-terminated symbol and the process's existing image set.
    let symbol =
        unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"proc_signal_with_audittoken".as_ptr()) };
    function(symbol)
}

#[cfg(target_vendor = "apple")]
fn function(symbol: *mut libc::c_void) -> io::Result<Signal> {
    if symbol.is_null() {
        return Err(unsupported());
    }
    // SAFETY: libproc.h declares exactly int(audit_token_t *, int). The token
    // is the existing eight-word AuditToken, passed by pointer, never by value.
    Ok(unsafe { std::mem::transmute::<*mut libc::c_void, Signal>(symbol) })
}

impl OwnedChildSignal {
    /// Capture only an actual child handle, before its exit has been consumed.
    /// No PID, token, environment or persisted observation supplies authority.
    /// # Errors
    /// Unsupported API, lost wait custody, reaped child, or unavailable or
    /// unstable native identity.
    pub fn capture(child: &mut Child) -> io::Result<Self> {
        #[cfg(target_vendor = "apple")]
        {
            Self::capture_with(child, resolve()?)
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            let _ = child;
            Err(unsupported())
        }
    }

    #[cfg(target_vendor = "apple")]
    fn capture_with(child: &mut Child, call: Signal) -> io::Result<Self> {
        Self::capture_with_probe(child, call, crate::process::read_lineage)
    }

    #[cfg(target_vendor = "apple")]
    fn capture_with_probe(
        child: &mut Child,
        call: Signal,
        mut lineage: impl FnMut(i32) -> Option<crate::process::Lineage>,
    ) -> io::Result<Self> {
        let stale = || {
            io::Error::new(
                io::ErrorKind::NotFound,
                "owned child image unavailable or changed",
            )
        };
        // ECHILD forfeits numeric custody without creating a cached positive
        // reap. Never use that stale handle to discover a new process image.
        if !child.owns_unreaped() {
            return Err(stale());
        }
        let original = lineage(child.id() as i32).ok_or_else(stale)?;
        if original.parent != std::process::id() || original.identity.idversion.is_none() {
            return Err(stale());
        }
        let mut token = crate::peer::AuditToken([0; 8]);
        // XNU proc_find_audit_token checks words5/7; all privileges still come
        // from cansignal/MAC policy, never from caller-filled token metadata.
        token.0[5] = child.id();
        token.0[7] = original.identity.idversion.ok_or_else(stale)? as u32;
        let result = Self {
            original,
            token,
            call,
        };
        result.validate()?;
        Ok(result)
    }

    #[cfg(target_vendor = "apple")]
    fn validate(&self) -> io::Result<()> {
        if crate::process::read_lineage(self.original.identity.pid).is_none_or(|now| {
            now.identity != self.original.identity
                || now.uniqueid != self.original.uniqueid
                || now.parent != std::process::id()
        }) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "owned child image unavailable or changed",
            ));
        }
        Ok(())
    }

    /// Signal the originally captured image through the atomic kernel API.
    /// An exec after the fresh check is still rejected by the syscall's token
    /// version check. There is deliberately no PID-based fallback.
    /// # Errors
    /// Stale image, unavailable API or the actual kernel permission/action error.
    pub fn signal(&mut self, action: Action) -> io::Result<()> {
        #[cfg(target_vendor = "apple")]
        {
            self.validate()?;
            let signal = match action {
                Action::Stop => libc::SIGSTOP,
                Action::Continue => libc::SIGCONT,
                Action::Kill => libc::SIGKILL,
            };
            let mut token = self.token;
            // SAFETY: Existing eight-word token has audit_token_t's exact32-byte
            // storage, retained for this call. Kernel validates the image and
            // signal permission atomically; this code never supplies a PID syscall.
            let result = unsafe { (self.call)(token.0.as_mut_ptr().cast(), signal) };
            if result == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            let _ = action;
            Err(unsupported())
        }
    }
}

#[cfg(all(test, target_vendor = "apple"))]
#[path = "owned_signal/tests.rs"]
mod tests;
