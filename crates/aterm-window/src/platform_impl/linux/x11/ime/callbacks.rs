// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/ime/callbacks.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::collections::HashMap;
use std::os::raw::c_char;
use std::ptr;
use std::sync::Arc;

use super::{ffi, XConnection, XError};

use super::context::{ImeContext, ImeContextCreationError};
use super::inner::{close_im, ImeInner};
use super::input_method::PotentialInputMethods;

pub(crate) unsafe fn xim_set_callback(
    xconn: &Arc<XConnection>,
    xim: ffi::XIM,
    field: *const c_char,
    callback: *mut ffi::XIMCallback,
) -> Result<(), XError> {
    unsafe { (xconn.xlib.XSetIMValues)(xim, field, callback, ptr::null_mut::<()>()) };
    xconn.check_errors()
}

pub(crate) unsafe fn set_instantiate_callback(
    xconn: &Arc<XConnection>,
    client_data: ffi::XPointer,
) -> Result<(), XError> {
    unsafe {
        (xconn.xlib.XRegisterIMInstantiateCallback)(
            xconn.display,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            Some(xim_instantiate_callback),
            client_data,
        )
    };
    xconn.check_errors()
}

pub(crate) unsafe fn unset_instantiate_callback(
    xconn: &Arc<XConnection>,
    client_data: ffi::XPointer,
) -> Result<(), XError> {
    unsafe {
        (xconn.xlib.XUnregisterIMInstantiateCallback)(
            xconn.display,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            Some(xim_instantiate_callback),
            client_data,
        )
    };
    xconn.check_errors()
}

pub(crate) unsafe fn set_destroy_callback(
    xconn: &Arc<XConnection>,
    im: ffi::XIM,
    inner: &ImeInner,
) -> Result<(), XError> {
    unsafe {
        xim_set_callback(
            xconn,
            im,
            ffi::xn::DESTROY_CALLBACK.as_ptr() as *const _,
            &inner.destroy_callback as *const _ as *mut _,
        )
    }
}

#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
enum ReplaceImError {
    MethodOpenFailed(#[allow(dead_code)] Box<PotentialInputMethods>),
    ContextCreationFailed(#[allow(dead_code)] ImeContextCreationError),
    SetDestroyCallbackFailed(#[allow(dead_code)] XError),
}

unsafe fn replace_im(inner: *mut ImeInner) -> Result<(), ReplaceImError> {
    let xconn = unsafe { &(*inner).xconn };

    let (new_im, is_fallback) = {
        let new_im = unsafe { (*inner).potential_input_methods.open_im(xconn, None) };
        let is_fallback = new_im.is_fallback();
        (
            new_im.ok().ok_or_else(|| {
                ReplaceImError::MethodOpenFailed(Box::new(unsafe {
                    (*inner).potential_input_methods.clone()
                }))
            })?,
            is_fallback,
        )
    };

    {
        let result = unsafe { set_destroy_callback(xconn, new_im.im, &*inner) };
        if result.is_err() {
            let _ = unsafe { close_im(xconn, new_im.im) };
        }
        result
    }
    .map_err(ReplaceImError::SetDestroyCallbackFailed)?;

    let mut new_contexts = HashMap::new();
    for (window, old_context) in unsafe { (*inner).contexts.iter() } {
        let spot = unsafe { (*inner).spot_for_new_context(*window) };
        let is_allowed =
            old_context.as_ref().map(|old_context| old_context.is_allowed()).unwrap_or_default();

        let new_context = {
            let result = unsafe {
                ImeContext::new(
                    xconn,
                    &new_im,
                    *window,
                    spot,
                    (*inner).event_sender.clone(),
                    is_allowed,
                )
            };
            if result.is_err() {
                let _ = unsafe { close_im(xconn, new_im.im) };
            }
            result.map_err(ReplaceImError::ContextCreationFailed)?
        };
        new_contexts.insert(*window, Some(new_context));
    }

    unsafe {
        let _ = (*inner).destroy_all_contexts_if_necessary();
        let _ = (*inner).close_im_if_necessary();
        (*inner).im = Some(new_im);
        (*inner).contexts = new_contexts;
        (*inner).is_destroyed = false;
        (*inner).is_fallback = is_fallback;
        let _ = (*inner).refocus();
        (*inner).announce_contexts(false);
    }
    Ok(())
}

/// [`replace_im`], as the input method's instantiate and destroy callbacks run it, for a test.
#[cfg(test)]
pub(crate) unsafe fn replace_im_for_test(inner: *mut ImeInner) -> bool {
    unsafe { replace_im(inner) }.is_ok()
}

pub unsafe extern "C" fn xim_instantiate_callback(
    _display: *mut ffi::Display,
    client_data: ffi::XPointer,
    _call_data: ffi::XPointer,
) {
    let inner: *mut ImeInner = client_data as _;
    if !inner.is_null() {
        let xconn = unsafe { &(*inner).xconn };
        match unsafe { replace_im(inner) } {
            Ok(()) => unsafe {
                let _ = unset_instantiate_callback(xconn, client_data);
                (*inner).is_fallback = false;
            },
            Err(err) => unsafe {
                if (*inner).is_destroyed {
                    panic!("Failed to reopen input method: {err:?}");
                }
            },
        }
    }
}

pub unsafe extern "C" fn xim_destroy_callback(
    _xim: ffi::XIM,
    client_data: ffi::XPointer,
    _call_data: ffi::XPointer,
) {
    let inner: *mut ImeInner = client_data as _;
    if !inner.is_null() {
        unsafe { (*inner).is_destroyed = true };
        let xconn = unsafe { &(*inner).xconn };
        if unsafe { !(*inner).is_fallback } {
            let _ = unsafe { set_instantiate_callback(xconn, client_data) };
            match unsafe { replace_im(inner) } {
                Ok(()) => unsafe { (*inner).is_fallback = true },
                Err(err) => {
                    panic!("Failed to open fallback input method: {err:?}");
                },
            }
        } else {
            unsafe { (*inner).announce_contexts(true) };
        }
    }
}
