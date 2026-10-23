// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/drop_handler.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by Andrew Yates for the aterm project in 2026; see the repository NOTICE.

use std::ffi::{c_void, OsString};
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::core::{GUID, HRESULT};
use windows_sys::Win32::Foundation::{
    DV_E_FORMATETC, E_NOINTERFACE, E_POINTER, HWND, POINTL, S_OK,
};
use windows_sys::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL};
use windows_sys::Win32::System::Ole::{
    ReleaseStgMedium, CF_HDROP, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows_sys::Win32::UI::Shell::{DragQueryFileW, HDROP};

use tracing::debug;

use crate::platform_impl::platform::definitions::{
    IDataObject, IDataObjectVtbl, IDropTarget, IDropTargetVtbl, IUnknown, IUnknownVtbl,
};
use crate::platform_impl::platform::WindowId;

use crate::event::Event;
use crate::window::WindowId as RootWindowId;

const IID_UNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_DROP_TARGET: GUID = GUID::from_u128(0x00000122_0000_0000_c000_000000000046);

/// A successful GetData transfers one medium, including its release owner.
struct StorageMedium(STGMEDIUM);

impl Drop for StorageMedium {
    fn drop(&mut self) {
        // SAFETY: constructed only after GetData succeeds; this is its one release.
        unsafe { ReleaseStgMedium(&mut self.0) };
    }
}

#[repr(C)]
pub struct FileDropHandlerData {
    pub interface: IDropTarget,
    refcount: AtomicUsize,
    window: HWND,
    send_event: Box<dyn Fn(Event<()>)>,
    cursor_effect: u32,
    hovered_is_valid: bool, /* If the currently hovered item is not valid there must not be any
                             * `HoveredFileCancelled` emitted */
}

pub struct FileDropHandler {
    pub data: *mut FileDropHandlerData,
}

#[allow(non_snake_case)]
impl FileDropHandler {
    pub fn new(window: HWND, send_event: Box<dyn Fn(Event<()>)>) -> FileDropHandler {
        let data = Box::new(FileDropHandlerData {
            interface: IDropTarget { lpVtbl: &DROP_TARGET_VTBL as *const IDropTargetVtbl },
            refcount: AtomicUsize::new(1),
            window,
            send_event,
            cursor_effect: DROPEFFECT_NONE,
            hovered_is_valid: false,
        });
        FileDropHandler { data: Box::into_raw(data) }
    }

    pub unsafe extern "system" fn QueryInterface(
        this: *mut IUnknown,
        riid: *const GUID,
        ppvObject: *mut *mut c_void,
    ) -> HRESULT {
        if ppvObject.is_null() {
            return E_POINTER;
        }
        // SAFETY: COM supplies a writable output pointer when non-null.
        unsafe { *ppvObject = ptr::null_mut() };
        if this.is_null() || riid.is_null() {
            return E_POINTER;
        }
        // SAFETY: COM supplies a readable interface ID when non-null.
        let iid = unsafe { &*riid };
        let supported = [IID_UNKNOWN, IID_DROP_TARGET].iter().any(|known| {
            iid.data1 == known.data1
                && iid.data2 == known.data2
                && iid.data3 == known.data3
                && iid.data4 == known.data4
        });
        if !supported {
            return E_NOINTERFACE;
        }
        // SAFETY: both interfaces start at the same repr(C) object's first field; each successful
        // query hands out one owned reference.
        unsafe {
            Self::AddRef(this);
            *ppvObject = this.cast();
        }
        S_OK
    }

    pub unsafe extern "system" fn AddRef(this: *mut IUnknown) -> u32 {
        let drop_handler_data = unsafe { Self::from_interface(this) };
        let count = drop_handler_data.refcount.fetch_add(1, Ordering::Release) + 1;
        count as u32
    }

    pub unsafe extern "system" fn Release(this: *mut IUnknown) -> u32 {
        let drop_handler = unsafe { Self::from_interface(this) };
        let count = drop_handler.refcount.fetch_sub(1, Ordering::Release) - 1;
        if count == 0 {
            drop(unsafe { Box::from_raw(drop_handler as *mut FileDropHandlerData) });
        }
        count as u32
    }

    pub unsafe extern "system" fn DragEnter(
        this: *mut IDropTarget,
        pDataObj: *const IDataObject,
        _grfKeyState: u32,
        _pt: *const POINTL,
        pdwEffect: *mut u32,
    ) -> HRESULT {
        use crate::event::WindowEvent::HoveredFile;
        let drop_handler = unsafe { Self::from_interface(this) };
        let valid = unsafe {
            Self::iterate_filenames(pDataObj, |filename| {
                drop_handler.send_event(Event::WindowEvent {
                    window_id: RootWindowId(WindowId(drop_handler.window)),
                    event: HoveredFile(filename),
                });
            })
        };
        drop_handler.hovered_is_valid = valid;
        drop_handler.cursor_effect =
            if drop_handler.hovered_is_valid { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
        unsafe {
            *pdwEffect = drop_handler.cursor_effect;
        }

        S_OK
    }

    pub unsafe extern "system" fn DragOver(
        this: *mut IDropTarget,
        _grfKeyState: u32,
        _pt: *const POINTL,
        pdwEffect: *mut u32,
    ) -> HRESULT {
        let drop_handler = unsafe { Self::from_interface(this) };
        unsafe {
            *pdwEffect = drop_handler.cursor_effect;
        }

        S_OK
    }

    pub unsafe extern "system" fn DragLeave(this: *mut IDropTarget) -> HRESULT {
        use crate::event::WindowEvent::HoveredFileCancelled;
        let drop_handler = unsafe { Self::from_interface(this) };
        if drop_handler.hovered_is_valid {
            drop_handler.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(drop_handler.window)),
                event: HoveredFileCancelled,
            });
        }

        S_OK
    }

    pub unsafe extern "system" fn Drop(
        this: *mut IDropTarget,
        pDataObj: *const IDataObject,
        _grfKeyState: u32,
        _pt: *const POINTL,
        _pdwEffect: *mut u32,
    ) -> HRESULT {
        use crate::event::WindowEvent::DroppedFile;
        let drop_handler = unsafe { Self::from_interface(this) };
        unsafe {
            Self::iterate_filenames(pDataObj, |filename| {
                drop_handler.send_event(Event::WindowEvent {
                    window_id: RootWindowId(WindowId(drop_handler.window)),
                    event: DroppedFile(filename),
                });
            })
        };

        S_OK
    }

    unsafe fn from_interface<'a, InterfaceT>(this: *mut InterfaceT) -> &'a mut FileDropHandlerData {
        unsafe { &mut *(this as *mut _) }
    }

    unsafe fn iterate_filenames<F>(data_obj: *const IDataObject, callback: F) -> bool
    where
        F: Fn(PathBuf),
    {
        let drop_format = FORMATETC {
            cfFormat: CF_HDROP,
            ptd: ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT,
            lindex: -1,
            tymed: TYMED_HGLOBAL as u32,
        };

        let mut medium = unsafe { std::mem::zeroed() };
        let get_data_fn = unsafe { (*(*data_obj).cast::<IDataObjectVtbl>()).GetData };
        let get_data_result = unsafe { get_data_fn(data_obj as *mut _, &drop_format, &mut medium) };
        if get_data_result >= 0 {
            let medium = StorageMedium(medium);
            if medium.0.tymed != TYMED_HGLOBAL as u32 {
                return false;
            }
            // SAFETY: the advertised medium type selects this union member.
            let hdrop: HDROP = unsafe { medium.0.u.hGlobal };

            let item_count = unsafe { DragQueryFileW(hdrop, 0xffffffff, ptr::null_mut(), 0) };

            for i in 0..item_count {
                let character_count =
                    unsafe { DragQueryFileW(hdrop, i, ptr::null_mut(), 0) as usize };
                let str_len = character_count + 1;

                let mut path_buf = vec![0; str_len];
                // SAFETY: the buffer owns str_len writable UTF-16 units.
                let copied = unsafe {
                    DragQueryFileW(hdrop, i, path_buf.as_mut_ptr(), str_len as u32) as usize
                };
                if copied > 0 {
                    if let Some(path) = path_buf.get(..copied) {
                        callback(OsString::from_wide(path).into());
                    }
                }
            }

            true
        } else if get_data_result == DV_E_FORMATETC {
            debug!("Error occurred while processing dropped/hovered item: item is not a file.");
            false
        } else {
            debug!("Unexpected error occurred while processing dropped/hovered item.");
            false
        }
    }
}

impl FileDropHandlerData {
    fn send_event(&self, event: Event<()>) {
        (self.send_event)(event);
    }
}

impl Drop for FileDropHandler {
    fn drop(&mut self) {
        unsafe {
            FileDropHandler::Release(self.data as *mut IUnknown);
        }
    }
}

static DROP_TARGET_VTBL: IDropTargetVtbl = IDropTargetVtbl {
    parent: IUnknownVtbl {
        QueryInterface: FileDropHandler::QueryInterface,
        AddRef: FileDropHandler::AddRef,
        Release: FileDropHandler::Release,
    },
    DragEnter: FileDropHandler::DragEnter,
    DragOver: FileDropHandler::DragOver,
    DragLeave: FileDropHandler::DragLeave,
    Drop: FileDropHandler::Drop,
};

#[cfg(test)]
mod tests {
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
}
