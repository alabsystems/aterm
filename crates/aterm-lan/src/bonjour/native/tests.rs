// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exercise native ownership through injected entry points, without a LAN operation.

use super::*;
use crate::bonjour::api::{
    DNSServiceBrowseReply, DNSServiceQueryRecordReply, DNSServiceRegisterRecordReply,
    DNSServiceRegisterReply, DNSServiceResolveReply,
};
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex};

macro_rules! refused {
    ($( $(#[$meta:meta])* fn $name:ident($($arg:ident: $ty:ty),*); )*) => {$(
        $(#[$meta])*
        unsafe extern "C" fn $name($($arg: $ty),*) -> i32 { BAD_PARAM }
    )*};
}

refused! {
    fn browse(_r: *mut Reference, _f: u32, _i: u32, _s: *const c_char,
        _d: *const c_char, _reply: DNSServiceBrowseReply, _c: *mut c_void);
    fn resolve(_r: *mut Reference, _f: u32, _i: u32, _n: *const c_char,
        _s: *const c_char, _d: *const c_char, _reply: DNSServiceResolveReply, _c: *mut c_void);
    fn query(_r: *mut Reference, _f: u32, _i: u32, _n: *const c_char, _t: u16,
        _cl: u16, _reply: DNSServiceQueryRecordReply, _c: *mut c_void);
    fn register(_r: *mut Reference, _f: u32, _i: u32, _n: *const c_char,
        _s: *const c_char, _d: *const c_char, _h: *const c_char, _p: u16,
        _l: u16, _t: *const c_void, _reply: DNSServiceRegisterReply, _c: *mut c_void);
    fn create(_r: *mut Reference);
    fn record(_r: Reference, _out: *mut RecordRef, _f: u32, _i: u32,
        _n: *const c_char, _t: u16, _cl: u16, _l: u16, _d: *const c_void,
        _ttl: u32, _reply: DNSServiceRegisterRecordReply, _c: *mut c_void);
}

static FAKE: Api = Api {
    browse,
    resolve,
    query_record: query,
    register,
    create_connection: create,
    register_record: record,
    ref_sock_fd: socket,
    process_result: process,
    ref_deallocate: deallocate,
};

struct FakeReference {
    socket: Option<aterm_uds::CtlStream>,
    context: *mut c_void,
    tag: u8,
    dropped: Arc<Mutex<Vec<u8>>>,
}

unsafe extern "C" fn socket(reference: Reference) -> i32 {
    // SAFETY: these tests pass a boxed FakeReference until deallocate consumes it.
    let reference = unsafe { &*reference.cast::<FakeReference>() };
    reference.socket.as_ref().map_or(-1, AsRawFd::as_raw_fd)
}

unsafe extern "C" fn process(reference: Reference) -> i32 {
    // SAFETY: the operation owns this fake reference and its stable Heard context.
    let context = unsafe { (*reference.cast::<FakeReference>()).context };
    // SAFETY: the callback receives the same live context and valid C strings as the SDK promises.
    unsafe {
        browse_reply(
            reference,
            FLAGS_ADD,
            7,
            0,
            c"unit".as_ptr(),
            c"_aterm-audit._tcp".as_ptr(),
            c"local.".as_ptr(),
            context,
        );
    }
    0
}

unsafe extern "C" fn deallocate(reference: Reference) {
    // SAFETY: each fake reference is a Box transferred to one operation and released once.
    let reference = unsafe { Box::from_raw(reference.cast::<FakeReference>()) };
    reference
        .dropped
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(reference.tag);
}

fn reference(
    socket: Option<aterm_uds::CtlStream>,
    context: *mut c_void,
    tag: u8,
    dropped: &Arc<Mutex<Vec<u8>>>,
) -> Reference {
    Box::into_raw(Box::new(FakeReference {
        socket,
        context,
        tag,
        dropped: dropped.clone(),
    }))
    .cast()
}

#[test]
fn the_operation_uses_its_table_for_callbacks_and_shared_before_main_cleanup() {
    let (read, _write) = aterm_uds::CtlStream::pair().unwrap();
    let dropped = Arc::new(Mutex::new(Vec::new()));
    let mut heard = Box::<Heard>::default();
    let context = std::ptr::from_mut(heard.as_mut()).cast();
    let main = reference(Some(read), context, 0, &dropped);
    let shared = reference(None, context, 1, &dropped);
    let mut operation = Operation::started(&FAKE, main, Some(shared), heard).unwrap();
    // SAFETY: the operation retains this descriptor; F_GETFD takes no extra argument.
    let flags = unsafe { libc::fcntl(operation.fd().as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
    assert_eq!(
        operation.process().unwrap(),
        vec![Event::Added(Instance {
            name: "unit".into(),
            service: "_aterm-audit._tcp".into(),
            domain: "local.".into(),
            interface: 7,
        })]
    );
    assert!(operation.heard.events.is_empty());
    drop(operation);
    assert_eq!(*dropped.lock().unwrap(), [1, 0]);
}

#[test]
fn a_failed_native_descriptor_releases_the_acquired_reference() {
    let dropped = Arc::new(Mutex::new(Vec::new()));
    let main = reference(None, std::ptr::null_mut(), 9, &dropped);
    let error = Operation::started(&FAKE, main, None, Box::default())
        .err()
        .unwrap();
    assert_eq!(error.0, BAD_PARAM);
    assert_eq!(*dropped.lock().unwrap(), [9]);
}

#[test]
fn invalid_requests_do_not_load_and_valid_requests_preserve_loader_failure() {
    for request in [
        Request::Browse {
            service: "bad\0service".into(),
            domain: String::new(),
        },
        Request::Resolve {
            name: "unit.local".into(),
            within_ms: 1,
        },
    ] {
        let error = start_using(&request, || {
            panic!("invalid input must not resolve exports")
        })
        .err()
        .unwrap();
        assert_eq!(error.0, BAD_PARAM);
    }
    let failure = (
        crate::bonjour::UNSUPPORTED,
        "a required export is unavailable".to_owned(),
    );
    let request = Request::Browse {
        service: "_aterm-audit._tcp".into(),
        domain: String::new(),
    };
    let error = start_using(&request, || Err(failure.clone()))
        .err()
        .unwrap();
    assert_eq!(error, failure);
}
