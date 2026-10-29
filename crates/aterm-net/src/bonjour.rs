// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Bonjour browse owned by this process, without a helper process.
//! The blocking reader and its callbacks stay on the calling thread. A callback
//! can end the browse; every exit releases its connection to mDNSResponder.

use std::ffi::{CStr, c_char, c_void};
use std::io;
use std::ops::ControlFlow;

type Reference = *mut c_void;
type Reply = unsafe extern "C" fn(
    Reference,
    u32,
    u32,
    i32,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_void,
);

const ADD: u32 = 2;
const BAD_PARAMETER: i32 = -65540;

/// One service instance added or removed on an interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event<'a> {
    pub added: bool,
    pub interface: u32,
    /// The unescaped instance name, borrowed only for the callback.
    pub name: &'a CStr,
}

/// Browse `service` in `domain` until the callback returns `Break`, or an error.
/// This blocks while there is no reply; call it on a background reader thread.
/// Unit-test builds have an inert entry point, with no live network I/O.
///
/// # Errors
/// The service cannot start, the daemon or a callback reports an error, or the
/// connection cannot be kept out of child processes, or the debug loopback
/// guard refuses network browsing before the native call.
///
/// # Panics
/// A callback's panic resumes after the native callback returns, releasing the
/// browse connection during unwinding.
pub fn browse(
    service: &CStr,
    domain: &CStr,
    heard: impl FnMut(Event<'_>) -> ControlFlow<()>,
) -> io::Result<()> {
    crate::loopback::check(crate::loopback::Reach::Browse(&service.to_string_lossy()))?;
    #[cfg(not(test))]
    {
        browse_with(&native::System, service, domain, heard)
    }
    #[cfg(test)]
    {
        let _ = (service, domain, heard);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "live Bonjour browsing is disabled in a unit test",
        ))
    }
}

/// The native calls, injected so lifecycle tests never contact the daemon.
/// Replies run synchronously only inside `process`, on its calling thread.
trait Api {
    /// # Safety
    /// `context` remains live for `reply` until the returned reference is closed.
    unsafe fn start(
        &self,
        service: &CStr,
        domain: &CStr,
        reply: Reply,
        context: *mut c_void,
    ) -> Result<Reference, i32>;
    /// # Safety
    /// `reference` is live and exclusively driven by the calling thread.
    unsafe fn keep_private(&self, reference: Reference) -> io::Result<()>;
    /// # Safety
    /// `reference` and its callback context are live, on their owning thread.
    unsafe fn process(&self, reference: Reference) -> i32;
    /// # Safety
    /// `reference` is live, no callback is in progress, and it is closed once.
    unsafe fn close(&self, reference: Reference);
}

struct Held<'a, A: Api> {
    reference: Reference,
    api: &'a A,
}

impl<A: Api> Drop for Held<'_, A> {
    fn drop(&mut self) {
        // SAFETY: this guard owns the successfully started reference. It drops
        // after process returns, before the stack callback context is dropped.
        unsafe { self.api.close(self.reference) };
    }
}

struct Context<F> {
    heard: F,
    stopped: bool,
    error: Option<i32>,
    panicked: Option<Box<dyn std::any::Any + Send>>,
}

fn failure(code: i32) -> io::Error {
    io::Error::other(format!("Bonjour browse failed ({code})"))
}

fn browse_with<A: Api, F: FnMut(Event<'_>) -> ControlFlow<()>>(
    api: &A,
    service: &CStr,
    domain: &CStr,
    heard: F,
) -> io::Result<()> {
    let mut context = Context {
        heard,
        stopped: false,
        error: None,
        panicked: None,
    };
    // SAFETY: context stays at this stack address until the browse is closed.
    // Only process invokes reply, synchronously on this thread; no native
    // dispatch queue or concurrent driver is installed.
    let reference = unsafe { api.start(service, domain, reply::<F>, (&raw mut context).cast()) }
        .map_err(failure)?;
    if reference.is_null() {
        return Err(failure(BAD_PARAMETER));
    }
    let held = Held { reference, api };
    // SAFETY: held exclusively owns this live reference on the current thread.
    unsafe { api.keep_private(held.reference) }?;
    loop {
        // SAFETY: held and context remain live and are driven on this thread.
        let result = unsafe { api.process(held.reference) };
        if let Some(panicked) = context.panicked.take() {
            std::panic::resume_unwind(panicked);
        }
        if let Some(error) = context.error.take() {
            return Err(failure(error));
        }
        if context.stopped {
            return Ok(());
        }
        if result != 0 {
            return Err(failure(result));
        }
    }
}

/// # Safety
/// The daemon supplies this callback's arguments. Context is a live Context<F>
/// exclusively driven on its owning thread; successful replies have a readable
/// NUL-terminated name for the duration of the call. Error replies' other
/// parameters are undefined and are not read.
unsafe extern "C" fn reply<F: FnMut(Event<'_>) -> ControlFlow<()>>(
    _reference: Reference,
    flags: u32,
    interface: u32,
    error: i32,
    name: *const c_char,
    _service: *const c_char,
    _domain: *const c_char,
    context: *mut c_void,
) {
    // SAFETY: start installed this exclusive stack context, still live here.
    let context = unsafe { &mut *context.cast::<Context<F>>() };
    if context.stopped || context.error.is_some() || context.panicked.is_some() {
        return;
    }
    if error != 0 {
        context.error = Some(error);
        return;
    }
    if name.is_null() {
        context.error = Some(BAD_PARAMETER);
        return;
    }
    // SAFETY: a successful reply supplies this NUL-terminated name, live until
    // the callback returns. Event's borrow cannot escape the callback.
    let name = unsafe { CStr::from_ptr(name) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (context.heard)(Event {
            added: flags & ADD != 0,
            interface,
            name,
        })
    }));
    match result {
        Ok(ControlFlow::Continue(())) => {}
        Ok(ControlFlow::Break(())) => context.stopped = true,
        Err(panicked) => context.panicked = Some(panicked),
    }
}

#[cfg(not(test))]
mod native {
    use super::*;
    use std::os::fd::BorrowedFd;

    pub(super) struct System;

    // The macOS SDK re-exports libsystem_dnssd through libSystem.
    #[link(name = "System")]
    unsafe extern "C" {
        fn DNSServiceBrowse(
            reference: *mut Reference,
            flags: u32,
            interface: u32,
            service: *const c_char,
            domain: *const c_char,
            reply: Reply,
            context: *mut c_void,
        ) -> i32;
        fn DNSServiceRefSockFD(reference: Reference) -> i32;
        fn DNSServiceProcessResult(reference: Reference) -> i32;
        fn DNSServiceRefDeallocate(reference: Reference);
    }

    impl Api for System {
        unsafe fn start(
            &self,
            service: &CStr,
            domain: &CStr,
            reply: Reply,
            context: *mut c_void,
        ) -> Result<Reference, i32> {
            let mut reference = std::ptr::null_mut();
            // SAFETY: the output slot is writable, strings live through this
            // call, and the caller keeps context live until deallocation.
            let result = unsafe {
                DNSServiceBrowse(
                    &raw mut reference,
                    0,
                    0,
                    service.as_ptr(),
                    domain.as_ptr(),
                    reply,
                    context,
                )
            };
            if result == 0 {
                Ok(reference)
            } else {
                Err(result)
            }
        }

        unsafe fn keep_private(&self, reference: Reference) -> io::Result<()> {
            // SAFETY: caller supplies the live, exclusively held reference.
            let fd = unsafe { DNSServiceRefSockFD(reference) };
            if fd < 0 {
                return Err(io::Error::other("Bonjour browse has no connection"));
            }
            // SAFETY: the native reference owns this descriptor throughout the
            // borrow; no driver or callback can close it concurrently.
            let fd = unsafe { BorrowedFd::borrow_raw(fd) };
            aterm_uds::keep_out_of_children(&fd)
        }

        unsafe fn process(&self, reference: Reference) -> i32 {
            // SAFETY: caller holds the reference and callback context live on
            // this thread. DNS-SD blocks for a reply and invokes callbacks here.
            unsafe { DNSServiceProcessResult(reference) }
        }

        unsafe fn close(&self, reference: Reference) {
            // SAFETY: the sole owner closes it after processing has returned.
            unsafe { DNSServiceRefDeallocate(reference) };
        }
    }
}

#[cfg(test)]
mod tests;
