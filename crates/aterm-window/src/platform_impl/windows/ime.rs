// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/ime.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::ffi::{c_void, OsString};
use std::os::windows::prelude::OsStringExt;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::UI::Input::Ime::HIMC;
use windows_sys::Win32::UI::Input::Ime::{
    ImmAssociateContextEx, ImmGetCompositionStringW, ImmGetContext, ImmReleaseContext,
    ImmSetCandidateWindow, ImmSetCompositionWindow, ATTR_TARGET_CONVERTED,
    ATTR_TARGET_NOTCONVERTED, CANDIDATEFORM, CFS_EXCLUDE, CFS_POINT, COMPOSITIONFORM, GCS_COMPATTR,
    GCS_COMPSTR, GCS_CURSORPOS, GCS_RESULTSTR, IACE_CHILDREN, IACE_DEFAULT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_IMMENABLED, WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION,
    WM_IME_STARTCOMPOSITION,
};

use crate::dpi::{Position, Size};
use crate::event::Ime;
use crate::platform_impl::platform::window_state::ImeState;

/// LOCAL PATCH (aterm): window input context reader for live or recorded IME compositions.
pub(crate) trait CompositionSource {
    /// `ImmGetCompositionStringW(gcs_mode)`'s bytes: `None` on an error, empty for nothing.
    fn data(&self, gcs_mode: u32) -> Option<Vec<u8>>;
    /// `ImmGetCompositionStringW(GCS_CURSORPOS)`: the caret in UTF-16 units, negative on error.
    fn cursor(&self) -> i32;
}

/// The composition being typed and the targeted UTF-8 byte offset range.
pub(crate) fn composing_text_and_cursor(
    source: &dyn CompositionSource,
) -> Option<(String, Option<usize>, Option<usize>)> {
    let text = composition_string(source, GCS_COMPSTR)?;
    let attrs = source.data(GCS_COMPATTR).unwrap_or_default();

    let mut first = None;
    let mut last = None;
    let mut boundary_before_char = 0;
    let mut attr_idx = 0;

    for chr in text.chars() {
        let Some(attr) = attrs.get(attr_idx).copied() else {
            break;
        };

        let char_is_targeted =
            attr as u32 == ATTR_TARGET_CONVERTED || attr as u32 == ATTR_TARGET_NOTCONVERTED;

        if first.is_none() && char_is_targeted {
            first = Some(boundary_before_char);
        } else if first.is_some() && last.is_none() && !char_is_targeted {
            last = Some(boundary_before_char);
        }

        boundary_before_char += chr.len_utf8();
        attr_idx += chr.len_utf16();
    }

    if first.is_some() && last.is_none() {
        last = Some(text.len());
    } else if first.is_none() {
        let cursor = source.cursor();
        let cursor =
            (cursor >= 0).then(|| text.chars().take(cursor as _).map(|c| c.len_utf8()).sum());
        first = cursor;
        last = cursor;
    }

    Some((text, first, last))
}

/// The text the IME committed.
pub(crate) fn composed_text(source: &dyn CompositionSource) -> Option<String> {
    composition_string(source, GCS_RESULTSTR)
}

fn composition_string(source: &dyn CompositionSource, gcs_mode: u32) -> Option<String> {
    let data = source.data(gcs_mode)?;
    // SAFETY: reinterpreting initialized bytes as `u16` is sound for every bit pattern;
    // `align_to` hands back any unaligned head and tail, which refuse the string below.
    let (prefix, shorts, suffix) = unsafe { data.align_to::<u16>() };
    if prefix.is_empty() && suffix.is_empty() {
        OsString::from_wide(shorts).into_string().ok()
    } else {
        None
    }
}

/// LOCAL PATCH (aterm): translate an IME window message into `Ime` events and update `state`.
pub(crate) fn ime_events(
    msg: u32,
    lparam: LPARAM,
    allowed: bool,
    state: &mut ImeState,
    source: &dyn CompositionSource,
) -> Vec<Ime> {
    let mut events = Vec::new();
    match msg {
        WM_IME_STARTCOMPOSITION if allowed => {
            *state = ImeState::Enabled;
            events.push(Ime::Enabled);
        },
        WM_IME_COMPOSITION if allowed && *state != ImeState::Disabled => {
            if lparam == 0 {
                events.push(Ime::Preedit(String::new(), None));
            }

            if (lparam as u32 & GCS_RESULTSTR) != 0 {
                if let Some(text) = composed_text(source) {
                    *state = ImeState::Enabled;
                    events.push(Ime::Preedit(String::new(), None));
                    events.push(Ime::Commit(text));
                }
            }

            if (lparam as u32 & GCS_COMPSTR) != 0 {
                if let Some((text, first, last)) = composing_text_and_cursor(source) {
                    *state = ImeState::Preedit;
                    let cursor_range = first.map(|f| (f, last.unwrap_or(f)));
                    events.push(Ime::Preedit(text, cursor_range));
                }
            }
        },
        WM_IME_ENDCOMPOSITION if allowed || *state != ImeState::Disabled => {
            if *state == ImeState::Preedit {
                if let Some(text) = composed_text(source) {
                    events.push(Ime::Preedit(String::new(), None));
                    events.push(Ime::Commit(text));
                }
            }
            *state = ImeState::Disabled;
            events.push(Ime::Disabled);
        },
        _ => {},
    }
    events
}

pub struct ImeContext {
    hwnd: HWND,
    himc: HIMC,
}

/// LOCAL PATCH (aterm): `window`'s input context, acquired lazily via `ImmGetContext`.
pub(crate) struct LazyImeContext {
    window: HWND,
    context: std::cell::OnceCell<ImeContext>,
}

impl LazyImeContext {
    pub(crate) fn new(window: HWND) -> Self {
        Self { window, context: std::cell::OnceCell::new() }
    }

    fn context(&self) -> &ImeContext {
        // SAFETY: `self.window` is the live window whose message is being handled.
        self.context.get_or_init(|| unsafe { ImeContext::current(self.window) })
    }
}

impl CompositionSource for LazyImeContext {
    fn data(&self, gcs_mode: u32) -> Option<Vec<u8>> {
        self.context().data(gcs_mode)
    }

    fn cursor(&self) -> i32 {
        self.context().cursor()
    }
}

impl CompositionSource for ImeContext {
    fn data(&self, gcs_mode: u32) -> Option<Vec<u8>> {
        // SAFETY: `self.himc` is the context `ImmGetContext` returned for `self.hwnd`, held
        // until drop.
        unsafe { self.get_composition_data(gcs_mode) }
    }

    fn cursor(&self) -> i32 {
        // SAFETY: as above; a null buffer of length 0 asks for the caret alone.
        unsafe { ImmGetCompositionStringW(self.himc, GCS_CURSORPOS, null_mut(), 0) }
    }
}

impl ImeContext {
    pub unsafe fn current(hwnd: HWND) -> Self {
        let himc = unsafe { ImmGetContext(hwnd) };
        ImeContext { hwnd, himc }
    }

    unsafe fn get_composition_data(&self, gcs_mode: u32) -> Option<Vec<u8>> {
        let size = match unsafe { ImmGetCompositionStringW(self.himc, gcs_mode, null_mut(), 0) } {
            0 => return Some(Vec::new()),
            size if size < 0 => return None,
            size => size,
        };

        let mut buf = Vec::<u8>::with_capacity(size as _);
        let size = unsafe {
            ImmGetCompositionStringW(
                self.himc,
                gcs_mode,
                buf.as_mut_ptr() as *mut c_void,
                size as _,
            )
        };

        if size < 0 {
            None
        } else {
            unsafe { buf.set_len(size as _) };
            Some(buf)
        }
    }

    pub unsafe fn set_ime_cursor_area(&self, spot: Position, size: Size, scale_factor: f64) {
        if !unsafe { ImeContext::system_has_ime() } {
            return;
        }

        let (x, y) = spot.to_physical::<i32>(scale_factor).into();
        let (width, height): (i32, i32) = size.to_physical::<i32>(scale_factor).into();
        let rc_area = RECT { left: x, top: y, right: x + width, bottom: y + height };
        let candidate_form = CANDIDATEFORM {
            dwIndex: 0,
            dwStyle: CFS_EXCLUDE,
            ptCurrentPos: POINT { x, y },
            rcArea: rc_area,
        };
        let composition_form = COMPOSITIONFORM {
            dwStyle: CFS_POINT,
            ptCurrentPos: POINT { x, y: y + height },
            rcArea: rc_area,
        };

        unsafe {
            ImmSetCompositionWindow(self.himc, &composition_form);
            ImmSetCandidateWindow(self.himc, &candidate_form);
        }
    }

    pub unsafe fn set_ime_allowed(hwnd: HWND, allowed: bool) {
        if !unsafe { ImeContext::system_has_ime() } {
            return;
        }

        if allowed {
            unsafe { ImmAssociateContextEx(hwnd, null_mut(), IACE_DEFAULT) };
        } else {
            unsafe { ImmAssociateContextEx(hwnd, null_mut(), IACE_CHILDREN) };
        }
    }

    unsafe fn system_has_ime() -> bool {
        unsafe { GetSystemMetrics(SM_IMMENABLED) != 0 }
    }
}

impl Drop for ImeContext {
    fn drop(&mut self) {
        unsafe { ImmReleaseContext(self.hwnd, self.himc) };
    }
}
