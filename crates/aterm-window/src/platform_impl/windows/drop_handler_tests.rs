// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/drop_handler.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::*;

#[test]
fn queries_preserve_interface_identity_and_own_each_successful_reference() {
    let handler = FileDropHandler::new(ptr::null_mut(), Box::new(|_| {}));
    let identity = handler.data.cast::<IUnknown>();
    for iid in [IID_UNKNOWN, IID_DROP_TARGET] {
        let mut interface = ptr::null_mut();
        // SAFETY: the handler, IID and output slot remain live throughout the call.
        unsafe {
            assert_eq!(FileDropHandler::QueryInterface(identity, &iid, &mut interface), S_OK);
            assert_eq!(interface, identity.cast());
            assert_eq!((*handler.data).refcount.load(Ordering::Relaxed), 2);
            assert_eq!(FileDropHandler::Release(interface.cast()), 1);
        }
    }
    let mut interface = identity.cast();
    let unknown = GUID::from_u128(1);
    // SAFETY: valid object and slots; null arguments exercise their explicit refusal.
    unsafe {
        assert_eq!(
            FileDropHandler::QueryInterface(identity, &unknown, &mut interface),
            E_NOINTERFACE,
        );
        assert!(interface.is_null());
        assert_eq!((*handler.data).refcount.load(Ordering::Relaxed), 1);
        assert_eq!(
            FileDropHandler::QueryInterface(identity, &IID_UNKNOWN, ptr::null_mut()),
            E_POINTER,
        );
        interface = identity.cast();
        assert_eq!(
            FileDropHandler::QueryInterface(identity, ptr::null(), &mut interface),
            E_POINTER,
        );
        assert!(interface.is_null());
    }
}

#[test]
fn storage_medium_releases_its_declared_owner_once() {
    let handler = FileDropHandler::new(ptr::null_mut(), Box::new(|_| {}));
    let identity = handler.data.cast::<IUnknown>();
    // SAFETY: the handler stays live, and the medium owns exactly this added reference.
    unsafe { FileDropHandler::AddRef(identity) };
    let medium = StorageMedium(STGMEDIUM {
        tymed: TYMED_HGLOBAL as u32,
        u: windows_sys::Win32::System::Com::STGMEDIUM_0 { hGlobal: ptr::null_mut() },
        pUnkForRelease: identity.cast(),
    });
    drop(medium);
    // SAFETY: the handler's original reference remains owned until the end of this scope.
    assert_eq!(unsafe { (*handler.data).refcount.load(Ordering::Relaxed) }, 1);
}
