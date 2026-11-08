// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DNS-SD entry points, acquired together before an operation owns native references.

use std::ffi::{c_char, c_void};
use std::sync::OnceLock;

macro_rules! opaque {
    ($($name:ident),* $(,)?) => {$(
        #[repr(C)]
        pub struct $name { _opaque: [u8; 0] }
    )*};
}

opaque! { _DNSServiceRef_t, _DNSRecordRef_t }

pub type DNSServiceRef = *mut _DNSServiceRef_t;
pub type DNSRecordRef = *mut _DNSRecordRef_t;
pub type DNSServiceBrowseReply = unsafe extern "C" fn(
    DNSServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_void,
);
pub type DNSServiceResolveReply = unsafe extern "C" fn(
    DNSServiceRef,
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
pub type DNSServiceQueryRecordReply = unsafe extern "C" fn(
    DNSServiceRef,
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
pub type DNSServiceRegisterReply = unsafe extern "C" fn(
    DNSServiceRef,
    u32,
    i32,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_void,
);
pub type DNSServiceRegisterRecordReply =
    unsafe extern "C" fn(DNSServiceRef, DNSRecordRef, u32, i32, *mut c_void);

aterm_dl::table! {
    /// A complete DNS-SD capability. An operation retains the table through deallocation.
    pub struct Api {
        browse = c"DNSServiceBrowse": unsafe extern "C" fn(
            *mut DNSServiceRef, u32, u32, *const c_char, *const c_char,
            DNSServiceBrowseReply, *mut c_void,
        ) -> i32;
        resolve = c"DNSServiceResolve": unsafe extern "C" fn(
            *mut DNSServiceRef, u32, u32, *const c_char, *const c_char, *const c_char,
            DNSServiceResolveReply, *mut c_void,
        ) -> i32;
        query_record = c"DNSServiceQueryRecord": unsafe extern "C" fn(
            *mut DNSServiceRef, u32, u32, *const c_char, u16, u16,
            DNSServiceQueryRecordReply, *mut c_void,
        ) -> i32;
        register = c"DNSServiceRegister": unsafe extern "C" fn(
            *mut DNSServiceRef, u32, u32, *const c_char, *const c_char, *const c_char,
            *const c_char, u16, u16, *const c_void, DNSServiceRegisterReply, *mut c_void,
        ) -> i32;
        create_connection = c"DNSServiceCreateConnection": unsafe extern "C" fn(*mut DNSServiceRef) -> i32;
        register_record = c"DNSServiceRegisterRecord": unsafe extern "C" fn(
            DNSServiceRef, *mut DNSRecordRef, u32, u32, *const c_char, u16, u16, u16,
            *const c_void, u32, DNSServiceRegisterRecordReply, *mut c_void,
        ) -> i32;
        ref_sock_fd = c"DNSServiceRefSockFD": unsafe extern "C" fn(DNSServiceRef) -> i32;
        process_result = c"DNSServiceProcessResult": unsafe extern "C" fn(DNSServiceRef) -> i32;
        ref_deallocate = c"DNSServiceRefDeallocate": unsafe extern "C" fn(DNSServiceRef);
    }
}

/// Resolve the SDK's public libSystem exports only when a valid DNS-SD request needs them.
pub(super) fn system() -> Result<&'static Api, crate::server::Failure> {
    static API: OnceLock<Result<Api, aterm_dl::Error>> = OnceLock::new();
    API.get_or_init(|| {
        let library = aterm_dl::Library::open(&[c"/usr/lib/libSystem.B.dylib"])?;
        // SAFETY: the table declares the dns_sd.h prototypes. The loader retains the
        // public system library for the process lifetime, including after this scope.
        unsafe { Api::load(&library) }
    })
    .as_ref()
    .map_err(|why| (super::UNSUPPORTED, format!("DNS-SD entry points: {why}")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn concurrent_first_use_resolves_one_complete_real_table_without_a_dns_operation() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    super::system().expect("macOS supplies DNS-SD") as *const super::Api as usize
                })
            })
            .collect();
        let addresses: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert!(addresses.windows(2).all(|pair| pair[0] == pair[1]));
    }
}
