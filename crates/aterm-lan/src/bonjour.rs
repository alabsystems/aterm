// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The broker's DNS-SD calls, behind a small trait so its dispatch is tested without
//! mDNSResponder: a request starts a [`Live`] operation, whose descriptor the connection's thread
//! polls beside the client's socket. What it hears goes to the client as events; a client that
//! hangs up ends it, and dropping it ends the operation (a registration is withdrawn).

use std::os::fd::{AsFd as _, BorrowedFd};

use aterm_uds::CtlStream;
use aterm_uds::lan::wire::{self, Event, Frame, Request};

use crate::server::Failure;

/// `kDNSServiceErr_Unsupported`.
#[cfg(not(target_os = "macos"))]
pub(crate) const UNSUPPORTED: i32 = -65544;
/// `kDNSServiceErr_BadParam`.
#[cfg(target_os = "macos")]
pub(crate) const BAD_PARAM: i32 = -65540;

/// Where Bonjour requests start.
pub(crate) trait Bonjour: Send + Sync {
    /// Start `request` (a browse, a service resolve, a query or a register).
    ///
    /// # Errors
    /// The DNS-SD error code and the words.
    fn start(&self, request: &Request) -> Result<Box<dyn Live>, Failure>;
}

/// One operation in flight, driven on its connection's thread alone.
pub(crate) trait Live {
    /// The descriptor that is readable when [`Live::process`] has something to read.
    fn fd(&self) -> BorrowedFd<'_>;
    /// Read what is waiting and answer what it said.
    ///
    /// # Errors
    /// The operation's DNS-SD error: it is over.
    fn process(&mut self) -> Result<Vec<Event>, Failure>;
}

/// Drive `live` for the client on `stream` until the client hangs up (or sends anything, which a
/// connection past its request may not), a send to it fails, or the operation fails.
///
/// # Errors
/// The operation's failure, for the client to hear.
pub(crate) fn drive(stream: &CtlStream, live: &mut dyn Live) -> Result<(), Failure> {
    loop {
        let ready = aterm_uds::ready::either(live.fd(), stream.as_fd())
            .map_err(|error| (libc::EIO, error.to_string()))?;
        if ready.second {
            return Ok(());
        }
        if ready.first {
            for event in live.process()? {
                if wire::send(stream, &Frame::Event(event)).is_err() {
                    return Ok(());
                }
            }
        }
    }
}

/// This machine's DNS-SD: mDNSResponder on macOS; elsewhere none.
pub(crate) fn system() -> Box<dyn Bonjour> {
    #[cfg(target_os = "macos")]
    {
        Box::new(native::System)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(Unsupported)
    }
}

/// No DNS-SD: every request refused.
#[cfg(not(target_os = "macos"))]
struct Unsupported;

#[cfg(not(target_os = "macos"))]
impl Bonjour for Unsupported {
    fn start(&self, _request: &Request) -> Result<Box<dyn Live>, Failure> {
        Err((
            UNSUPPORTED,
            "the LAN broker makes DNS-SD calls on macOS only".to_owned(),
        ))
    }
}

/// A TXT record's strings, as DNS-SD's wire form has them (each behind its length byte).
#[cfg(target_os = "macos")]
pub(crate) fn txt_strings(record: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut rest = record;
    while let Some((&len, tail)) = rest.split_first() {
        let len = usize::from(len).min(tail.len());
        let (item, after) = tail.split_at(len);
        if out.len() < wire::MAX_ITEMS {
            out.push(item.to_vec());
        }
        rest = after;
    }
    out
}

/// `strings` as a TXT record's wire form.
///
/// # Errors
/// A string over 255 bytes, or a record over 65535.
#[cfg(target_os = "macos")]
pub(crate) fn txt_record(strings: &[Vec<u8>]) -> Result<Vec<u8>, Failure> {
    let total: usize = strings.iter().map(|item| 1 + item.len()).sum();
    if total > usize::from(u16::MAX) {
        return Err((BAD_PARAM, "a TXT record over 65535 bytes".to_owned()));
    }
    let mut out = Vec::with_capacity(total);
    for item in strings {
        let len = u8::try_from(item.len())
            .map_err(|_| (BAD_PARAM, "a TXT string over 255 bytes".to_owned()))?;
        out.push(len);
        out.extend_from_slice(item);
    }
    Ok(out)
}

#[cfg(target_os = "macos")]
mod native {
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::net::IpAddr;
    use std::os::fd::BorrowedFd;

    use aterm_uds::lan::wire::{Advert, Event, Instance, Request};

    use super::{BAD_PARAM, Bonjour, Live, txt_record, txt_strings};
    use crate::server::Failure;

    type Reference = *mut c_void;
    type RecordRef = *mut c_void;

    const FLAGS_ADD: u32 = 0x2;
    const FLAGS_UNIQUE: u32 = 0x20;
    const FLAGS_SHARE_CONNECTION: u32 = 0x4000;
    const CLASS_IN: u16 = 1;
    const TYPE_A: u16 = 1;
    const TYPE_AAAA: u16 = 28;
    const HOST_TTL: u32 = 240;

    type BrowseReply = unsafe extern "C" fn(
        Reference,
        u32,
        u32,
        i32,
        *const c_char,
        *const c_char,
        *const c_char,
        *mut c_void,
    );
    type ResolveReply = unsafe extern "C" fn(
        Reference,
        u32,
        u32,
        i32,
        *const c_char,
        *const c_char,
        u16,
        u16,
        *const u8,
        *mut c_void,
    );
    type QueryReply = unsafe extern "C" fn(
        Reference,
        u32,
        u32,
        i32,
        *const c_char,
        u16,
        u16,
        u16,
        *const c_void,
        u32,
        *mut c_void,
    );
    type RegisterReply = unsafe extern "C" fn(
        Reference,
        u32,
        i32,
        *const c_char,
        *const c_char,
        *const c_char,
        *mut c_void,
    );
    type RecordReply = unsafe extern "C" fn(Reference, RecordRef, u32, i32, *mut c_void);

    // The macOS SDK re-exports libsystem_dnssd through libSystem.
    #[link(name = "System")]
    unsafe extern "C" {
        fn DNSServiceBrowse(
            reference: *mut Reference,
            flags: u32,
            interface: u32,
            service: *const c_char,
            domain: *const c_char,
            reply: BrowseReply,
            context: *mut c_void,
        ) -> i32;
        fn DNSServiceResolve(
            reference: *mut Reference,
            flags: u32,
            interface: u32,
            name: *const c_char,
            service: *const c_char,
            domain: *const c_char,
            reply: ResolveReply,
            context: *mut c_void,
        ) -> i32;
        fn DNSServiceQueryRecord(
            reference: *mut Reference,
            flags: u32,
            interface: u32,
            name: *const c_char,
            rrtype: u16,
            rrclass: u16,
            reply: QueryReply,
            context: *mut c_void,
        ) -> i32;
        fn DNSServiceRegister(
            reference: *mut Reference,
            flags: u32,
            interface: u32,
            name: *const c_char,
            service: *const c_char,
            domain: *const c_char,
            host: *const c_char,
            port: u16,
            txt_len: u16,
            txt: *const c_void,
            reply: RegisterReply,
            context: *mut c_void,
        ) -> i32;
        fn DNSServiceCreateConnection(reference: *mut Reference) -> i32;
        fn DNSServiceRegisterRecord(
            reference: Reference,
            record: *mut RecordRef,
            flags: u32,
            interface: u32,
            name: *const c_char,
            rrtype: u16,
            rrclass: u16,
            rdlen: u16,
            rdata: *const c_void,
            ttl: u32,
            reply: RecordReply,
            context: *mut c_void,
        ) -> i32;
        fn DNSServiceRefSockFD(reference: Reference) -> i32;
        fn DNSServiceProcessResult(reference: Reference) -> i32;
        fn DNSServiceRefDeallocate(reference: Reference);
    }

    pub(super) struct System;

    /// What the callbacks heard since the last [`Live::process`].
    #[derive(Default)]
    struct Heard {
        events: Vec<Event>,
        error: Option<i32>,
    }

    /// One DNS-SD operation: its connection (`main`), a registration shared on it (`shared`),
    /// and the boxed context its callbacks write, at a fixed address until both are deallocated.
    struct Operation {
        main: Reference,
        shared: Option<Reference>,
        fd: i32,
        heard: Box<Heard>,
    }

    impl Drop for Operation {
        fn drop(&mut self) {
            // SAFETY: each reference is live and owned here alone, no callback is running (they
            // run only inside `process`, on this thread), and each is deallocated once: a shared
            // one before the connection it shares.
            unsafe {
                if let Some(shared) = self.shared.take() {
                    DNSServiceRefDeallocate(shared);
                }
                DNSServiceRefDeallocate(self.main);
            }
        }
    }

    fn c(text: &str) -> Result<CString, Failure> {
        CString::new(text).map_err(|_| (BAD_PARAM, format!("{text:?} holds a NUL")))
    }

    /// `text`, or NULL for empty (DNS-SD's "the default").
    fn c_or_null(text: &CString) -> *const c_char {
        if text.as_bytes().is_empty() {
            std::ptr::null()
        } else {
            text.as_ptr()
        }
    }

    fn failed(code: i32, what: &str) -> Failure {
        (code, format!("{what} failed (DNS-SD error {code})"))
    }

    /// # Safety
    /// `ptr` is NULL or a NUL-terminated string live for the call.
    unsafe fn text(ptr: *const c_char) -> String {
        if ptr.is_null() {
            String::new()
        } else {
            // SAFETY: the caller's contract.
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned()
        }
    }

    /// # Safety
    /// `context` is the live `Heard` an [`Operation`] installed, on its own thread.
    unsafe fn heard<'a>(context: *mut c_void) -> &'a mut Heard {
        // SAFETY: the caller's contract.
        unsafe { &mut *context.cast::<Heard>() }
    }

    unsafe extern "C" fn browse_reply(
        _reference: Reference,
        flags: u32,
        interface: u32,
        error: i32,
        name: *const c_char,
        service: *const c_char,
        domain: *const c_char,
        context: *mut c_void,
    ) {
        // SAFETY: DNS-SD passes back the context `start` installed.
        let heard = unsafe { heard(context) };
        if error != 0 {
            heard.error = Some(error);
            return;
        }
        // SAFETY: a successful reply's strings are live for the callback.
        let instance = unsafe {
            Instance {
                name: text(name),
                service: text(service),
                domain: text(domain),
                interface,
            }
        };
        heard.events.push(if flags & FLAGS_ADD != 0 {
            Event::Added(instance)
        } else {
            Event::Removed(instance)
        });
    }

    unsafe extern "C" fn resolve_reply(
        _reference: Reference,
        _flags: u32,
        interface: u32,
        error: i32,
        _full_name: *const c_char,
        host: *const c_char,
        port: u16,
        txt_len: u16,
        txt: *const u8,
        context: *mut c_void,
    ) {
        // SAFETY: as `browse_reply`.
        let heard = unsafe { heard(context) };
        if error != 0 {
            heard.error = Some(error);
            return;
        }
        let record = if txt.is_null() {
            &[][..]
        } else {
            // SAFETY: a successful reply's TXT record is `txt_len` bytes live for the callback.
            unsafe { std::slice::from_raw_parts(txt, usize::from(txt_len)) }
        };
        heard.events.push(Event::Resolved {
            // SAFETY: as `browse_reply`.
            host: unsafe { text(host) },
            port: u16::from_be(port),
            txt: txt_strings(record),
            interface,
        });
    }

    unsafe extern "C" fn query_reply(
        _reference: Reference,
        flags: u32,
        interface: u32,
        error: i32,
        _full_name: *const c_char,
        rrtype: u16,
        _rrclass: u16,
        rdlen: u16,
        rdata: *const c_void,
        ttl: u32,
        context: *mut c_void,
    ) {
        // SAFETY: as `browse_reply`.
        let heard = unsafe { heard(context) };
        if error != 0 {
            heard.error = Some(error);
            return;
        }
        let rdata = if rdata.is_null() {
            Vec::new()
        } else {
            // SAFETY: a successful reply's rdata is `rdlen` bytes live for the callback.
            unsafe { std::slice::from_raw_parts(rdata.cast::<u8>(), usize::from(rdlen)) }.to_vec()
        };
        heard.events.push(Event::Record {
            rrtype,
            rdata,
            ttl,
            interface,
            added: flags & FLAGS_ADD != 0,
        });
    }

    unsafe extern "C" fn register_reply(
        _reference: Reference,
        flags: u32,
        error: i32,
        name: *const c_char,
        service: *const c_char,
        domain: *const c_char,
        context: *mut c_void,
    ) {
        // SAFETY: as `browse_reply`.
        let heard = unsafe { heard(context) };
        if error != 0 {
            heard.error = Some(error);
            return;
        }
        if flags & FLAGS_ADD == 0 {
            return;
        }
        // SAFETY: as `browse_reply`.
        heard.events.push(unsafe {
            Event::Registered {
                name: text(name),
                service: text(service),
                domain: text(domain),
            }
        });
    }

    unsafe extern "C" fn record_reply(
        _reference: Reference,
        _record: RecordRef,
        _flags: u32,
        error: i32,
        context: *mut c_void,
    ) {
        // SAFETY: as `browse_reply`.
        let heard = unsafe { heard(context) };
        if error != 0 {
            heard.error = Some(error);
        }
    }

    impl Operation {
        /// An operation whose `main` reference was just started on `heard`.
        fn started(
            main: Reference,
            shared: Option<Reference>,
            heard: Box<Heard>,
        ) -> Result<Self, Failure> {
            let mut operation = Self {
                main,
                shared,
                fd: -1,
                heard,
            };
            // SAFETY: `main` is live and owned by `operation`.
            let fd = unsafe { DNSServiceRefSockFD(operation.main) };
            if fd < 0 {
                return Err((
                    BAD_PARAM,
                    "a DNS-SD operation with no connection".to_owned(),
                ));
            }
            operation.fd = fd;
            // SAFETY: the reference owns this descriptor for as long as the operation lives.
            let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
            aterm_uds::keep_out_of_children(&borrowed)
                .map_err(|error| (BAD_PARAM, error.to_string()))?;
            Ok(operation)
        }
    }

    fn register(advert: &Advert) -> Result<Operation, Failure> {
        let name = c(&advert.name)?;
        let service = c(&advert.service)?;
        let domain = c(&advert.domain)?;
        let host = advert.host.as_deref().map(c).transpose()?;
        let txt = txt_record(&advert.txt)?;
        let txt_len = u16::try_from(txt.len()).unwrap_or(u16::MAX);
        let txt_ptr = if txt.is_empty() {
            std::ptr::null()
        } else {
            txt.as_ptr().cast::<c_void>()
        };
        let interface = advert.interface.unwrap_or(0);
        let mut heard = Box::<Heard>::default();
        let context = std::ptr::from_mut(heard.as_mut()).cast::<c_void>();
        let proxy = host.as_ref().filter(|_| !advert.addresses.is_empty());
        let Some(host) = proxy else {
            let mut reference: Reference = std::ptr::null_mut();
            // SAFETY: every string and the TXT bytes live through the call; `context` stays at
            // its boxed address until the operation is deallocated.
            let rc = unsafe {
                DNSServiceRegister(
                    &raw mut reference,
                    0,
                    interface,
                    c_or_null(&name),
                    service.as_ptr(),
                    c_or_null(&domain),
                    host.as_ref().map_or(std::ptr::null(), |h| h.as_ptr()),
                    advert.port.to_be(),
                    txt_len,
                    txt_ptr,
                    register_reply,
                    context,
                )
            };
            if rc != 0 {
                return Err(failed(rc, "DNSServiceRegister"));
            }
            return Operation::started(reference, None, heard);
        };
        let mut connection: Reference = std::ptr::null_mut();
        // SAFETY: an out-parameter for one new connection.
        let rc = unsafe { DNSServiceCreateConnection(&raw mut connection) };
        if rc != 0 {
            return Err(failed(rc, "DNSServiceCreateConnection"));
        }
        let mut operation = Operation::started(connection, None, heard)?;
        for address in &advert.addresses {
            let v4;
            let v6;
            let (rrtype, rdata): (u16, &[u8]) = match address {
                IpAddr::V4(addr) => {
                    v4 = addr.octets();
                    (TYPE_A, &v4)
                }
                IpAddr::V6(addr) => {
                    v6 = addr.octets();
                    (TYPE_AAAA, &v6)
                }
            };
            let mut record: RecordRef = std::ptr::null_mut();
            // SAFETY: the connection is live and owned by `operation`; the name and rdata live
            // through the call; the record is freed with the connection.
            let rc = unsafe {
                DNSServiceRegisterRecord(
                    operation.main,
                    &raw mut record,
                    FLAGS_UNIQUE,
                    interface,
                    host.as_ptr(),
                    rrtype,
                    CLASS_IN,
                    u16::try_from(rdata.len()).unwrap_or(0),
                    rdata.as_ptr().cast(),
                    HOST_TTL,
                    record_reply,
                    context,
                )
            };
            if rc != 0 {
                return Err(failed(rc, "DNSServiceRegisterRecord"));
            }
        }
        let mut shared = operation.main;
        // SAFETY: as the plain register; `shared` starts as the live connection, as
        // `kDNSServiceFlagsShareConnection` requires, and comes back the registration's own
        // reference on it.
        let rc = unsafe {
            DNSServiceRegister(
                &raw mut shared,
                FLAGS_SHARE_CONNECTION,
                interface,
                c_or_null(&name),
                service.as_ptr(),
                c_or_null(&domain),
                host.as_ptr(),
                advert.port.to_be(),
                txt_len,
                txt_ptr,
                register_reply,
                context,
            )
        };
        if rc != 0 {
            return Err(failed(rc, "DNSServiceRegister"));
        }
        operation.shared = Some(shared);
        Ok(operation)
    }

    impl Bonjour for System {
        fn start(&self, request: &Request) -> Result<Box<dyn Live>, Failure> {
            if let Request::Register(advert) = request {
                return Ok(Box::new(register(advert)?));
            }
            let mut heard = Box::<Heard>::default();
            let context = std::ptr::from_mut(heard.as_mut()).cast::<c_void>();
            let mut reference: Reference = std::ptr::null_mut();
            let (rc, what) = match request {
                Request::Browse { service, domain } => {
                    let (service, domain) = (c(service)?, c(domain)?);
                    // SAFETY: the strings live through the call, the out-parameter is
                    // writable, and `context` stays at its boxed address until the reference is
                    // deallocated.
                    let rc = unsafe {
                        DNSServiceBrowse(
                            &raw mut reference,
                            0,
                            0,
                            service.as_ptr(),
                            c_or_null(&domain),
                            browse_reply,
                            context,
                        )
                    };
                    (rc, "DNSServiceBrowse")
                }
                Request::ResolveService {
                    name,
                    service,
                    domain,
                } => {
                    let (name, service, domain) = (c(name)?, c(service)?, c(domain)?);
                    // SAFETY: the strings live through the call, the out-parameter is
                    // writable, and `context` stays at its boxed address until the reference is
                    // deallocated.
                    let rc = unsafe {
                        DNSServiceResolve(
                            &raw mut reference,
                            0,
                            0,
                            name.as_ptr(),
                            service.as_ptr(),
                            domain.as_ptr(),
                            resolve_reply,
                            context,
                        )
                    };
                    (rc, "DNSServiceResolve")
                }
                Request::Query { name, rrtype } => {
                    let name = c(name)?;
                    // SAFETY: the strings live through the call, the out-parameter is
                    // writable, and `context` stays at its boxed address until the reference is
                    // deallocated.
                    let rc = unsafe {
                        DNSServiceQueryRecord(
                            &raw mut reference,
                            0,
                            0,
                            name.as_ptr(),
                            *rrtype,
                            CLASS_IN,
                            query_reply,
                            context,
                        )
                    };
                    (rc, "DNSServiceQueryRecord")
                }
                other => {
                    return Err((BAD_PARAM, format!("{other:?} is no Bonjour request")));
                }
            };
            if rc != 0 {
                return Err(failed(rc, what));
            }
            Ok(Box::new(Operation::started(reference, None, heard)?))
        }
    }

    impl Live for Operation {
        fn fd(&self) -> BorrowedFd<'_> {
            // SAFETY: the reference owns this descriptor for as long as `self` lives.
            unsafe { BorrowedFd::borrow_raw(self.fd) }
        }

        fn process(&mut self) -> Result<Vec<Event>, Failure> {
            // SAFETY: the reference and its context are live and driven on this thread alone;
            // the callbacks run here, synchronously.
            let rc = unsafe { DNSServiceProcessResult(self.main) };
            if let Some(error) = self.heard.error.take() {
                return Err(failed(error, "the DNS-SD operation"));
            }
            if rc != 0 {
                return Err(failed(rc, "DNSServiceProcessResult"));
            }
            Ok(std::mem::take(&mut self.heard.events))
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn txt_records_round_trip_and_bound_their_strings() {
        let strings = vec![b"v=1".to_vec(), Vec::new(), b"k".to_vec()];
        let record = txt_record(&strings).unwrap();
        assert_eq!(record, b"\x03v=1\x00\x01k");
        assert_eq!(txt_strings(&record), strings);
        assert_eq!(
            txt_strings(b"\x09ab"),
            vec![b"ab".to_vec()],
            "a short tail is cut"
        );
        assert!(txt_record(&[vec![0; 256]]).is_err());
    }
}
