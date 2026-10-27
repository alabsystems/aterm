// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The values UI Automation exchanges with a provider, owned: BSTRs, VARIANTs
//! and SAFEARRAY vectors, made and read as `windows` 0.62.2's conversions made
//! and read them.

use std::mem::ManuallyDrop;
use std::ptr;

use aterm_win32::Win32::Foundation::{
    E_OUTOFMEMORY, ERROR_NO_UNICODE_TRANSLATION, SysAllocStringLen, SysFreeString, SysStringLen,
    VARIANT_FALSE, VARIANT_TRUE,
};
use aterm_win32::Win32::System::Com::SAFEARRAY;
use aterm_win32::Win32::System::Ole::{
    SafeArrayCreateVector, SafeArrayDestroy, SafeArrayPutElement,
};
use aterm_win32::Win32::System::Variant::{
    VARENUM, VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_BSTR, VT_I4, VT_R8,
    VT_UNKNOWN, VariantClear, VariantToBoolean,
};
use aterm_win32::com::Com;
use aterm_win32::core::{BOOL, BSTR, HRESULT, PCWSTR};

use super::com::{Result, into_caller};

/// `HRESULT_FROM_WIN32(ERROR_NO_UNICODE_TRANSLATION)`: what a string that is
/// not UTF-16 converted to.
const NO_UNICODE_TRANSLATION: HRESULT = (0x8007_0000 | ERROR_NO_UNICODE_TRANSLATION).cast_signed();

/// An owned BSTR. The empty string is the null BSTR, which UI Automation reads
/// as empty.
pub(super) struct Bstr(BSTR);

impl Bstr {
    pub(super) fn new(text: &str) -> Result<Self> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        if wide.is_empty() {
            return Ok(Self(ptr::null()));
        }
        let len = u32::try_from(wide.len()).map_err(|_| E_OUTOFMEMORY)?;
        // SAFETY: `wide` holds `len` initialized code units.
        let raw = unsafe { SysAllocStringLen(wide.as_ptr(), len) };
        if raw.is_null() {
            return Err(E_OUTOFMEMORY);
        }
        Ok(Self(raw))
    }

    /// The string, still owned here.
    #[cfg(test)]
    pub(super) const fn as_raw(&self) -> BSTR {
        self.0
    }

    /// Hands the string to the caller, who frees it.
    pub(super) fn into_raw(self) -> BSTR {
        ManuallyDrop::new(self).0
    }
}

impl Drop for Bstr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: allocated by `SysAllocStringLen` and owned here.
            unsafe { SysFreeString(self.0) };
        }
    }
}

/// The text of a BSTR the caller keeps, unpaired surrogates as U+FFFD.
///
/// # Safety
/// A non-null `bstr` is a live BSTR.
pub(super) unsafe fn bstr_text(bstr: BSTR) -> String {
    if bstr.is_null() {
        return String::new();
    }
    // SAFETY: a live BSTR records its length in code units before its text.
    let wide = unsafe { std::slice::from_raw_parts(bstr, SysStringLen(bstr) as usize) };
    char::decode_utf16(wide.iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// The text of a NUL-terminated wide string the caller keeps; one that is not
/// UTF-16 is refused.
///
/// # Safety
/// `text` is non-null and NUL-terminated.
pub(super) unsafe fn wide_text(text: PCWSTR) -> Result<String> {
    let mut len = 0;
    // SAFETY: NUL-terminated, by the caller's contract.
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: the `len` code units before the NUL.
    String::from_utf16(unsafe { std::slice::from_raw_parts(text, len) })
        .map_err(|_| NO_UNICODE_TRANSLATION)
}

/// An owned VARIANT, cleared on drop.
pub(super) struct Variant(VARIANT);

impl Variant {
    /// `VT_EMPTY`.
    pub(super) fn empty() -> Self {
        Self(VARIANT::default())
    }

    fn of(vt: VARENUM, value: VARIANT_0_0_0) -> Self {
        Self(VARIANT {
            Anonymous: VARIANT_0 {
                Anonymous: VARIANT_0_0 {
                    vt,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: value,
                },
            },
        })
    }

    pub(super) fn i32(value: i32) -> Self {
        Self::of(VT_I4, VARIANT_0_0_0 { lVal: value })
    }

    pub(super) fn bool(value: bool) -> Self {
        Self::of(
            VT_BOOL,
            VARIANT_0_0_0 {
                boolVal: if value { VARIANT_TRUE } else { VARIANT_FALSE },
            },
        )
    }

    pub(super) fn text(value: &str) -> Result<Self> {
        Ok(Self::of(
            VT_BSTR,
            VARIANT_0_0_0 {
                bstrVal: Bstr::new(value)?.into_raw(),
            },
        ))
    }

    /// `VT_UNKNOWN`, owning `value`'s reference.
    pub(super) fn unknown(value: Com) -> Self {
        Self::of(
            VT_UNKNOWN,
            VARIANT_0_0_0 {
                punkVal: into_caller(value),
            },
        )
    }

    /// A bitwise copy to pass by value; `self` still owns what it holds.
    pub(super) const fn by_value(&self) -> VARIANT {
        self.0
    }

    /// Hands the value to the caller, who clears it.
    pub(super) fn into_raw(self) -> VARIANT {
        ManuallyDrop::new(self).0
    }
}

impl Drop for Variant {
    fn drop(&mut self) {
        // SAFETY: every `Variant` holds a valid VARIANT whose contents it
        // owns; clearing frees a BSTR and releases an interface.
        unsafe { VariantClear(&mut self.0) };
    }
}

/// `value` as a boolean, by `VariantToBoolean`'s conversions.
pub(super) fn variant_bool(value: &VARIANT) -> Result<bool> {
    let mut out: BOOL = 0;
    // SAFETY: `value` is a valid VARIANT the caller keeps; `out` is writable.
    let hr = unsafe { VariantToBoolean(value, &mut out) };
    if hr < 0 { Err(hr) } else { Ok(out != 0) }
}

/// A one-dimensional SAFEARRAY of `len` `vt` elements from index 0, each
/// stored by `put`; destroyed again when one is refused.
fn array(
    vt: VARENUM,
    len: usize,
    mut put: impl FnMut(*mut SAFEARRAY, i32) -> HRESULT,
) -> Result<*mut SAFEARRAY> {
    let count = i32::try_from(len).map_err(|_| E_OUTOFMEMORY)?;
    // SAFETY: a plain allocation; null is refused below.
    let array = unsafe { SafeArrayCreateVector(vt, 0, count.cast_unsigned()) };
    if array.is_null() {
        return Err(E_OUTOFMEMORY);
    }
    for index in 0..count {
        let hr = put(array, index);
        if hr < 0 {
            // SAFETY: made above and not handed out.
            unsafe { SafeArrayDestroy(array) };
            return Err(hr);
        }
    }
    Ok(array)
}

pub(super) fn array_i32(values: &[i32]) -> Result<*mut SAFEARRAY> {
    array(VT_I4, values.len(), |array, index| {
        // SAFETY: `index` is in bounds of both `array` and `values`; the
        // element is copied.
        unsafe {
            SafeArrayPutElement(
                array,
                &index,
                ptr::from_ref(&values[index.cast_unsigned() as usize]).cast(),
            )
        }
    })
}

pub(super) fn array_f64(values: &[f64]) -> Result<*mut SAFEARRAY> {
    array(VT_R8, values.len(), |array, index| {
        // SAFETY: as in `array_i32`.
        unsafe {
            SafeArrayPutElement(
                array,
                &index,
                ptr::from_ref(&values[index.cast_unsigned() as usize]).cast(),
            )
        }
    })
}

/// A `VT_UNKNOWN` vector; the array takes a reference of its own to each.
pub(super) fn array_com(values: &[Com]) -> Result<*mut SAFEARRAY> {
    array(VT_UNKNOWN, values.len(), |array, index| {
        // SAFETY: `index` is in bounds; a VT_UNKNOWN element is the interface
        // pointer itself, which the array AddRefs.
        unsafe {
            SafeArrayPutElement(
                array,
                &index,
                values[index.cast_unsigned() as usize].as_raw().cast_const(),
            )
        }
    })
}
