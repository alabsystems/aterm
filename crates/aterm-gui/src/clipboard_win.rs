// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Win32 clipboard backend (`CF_UNICODETEXT` via user32/kernel32 and `CF_HDROP` via
//! [`get_paths`] for terminal file pastes), retrying `OpenClipboard` on contention.

#[link(name = "user32")]
unsafe extern "system" {
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: isize,
        menu: isize,
        instance: isize,
        param: *mut std::ffi::c_void,
    ) -> isize;
    fn DestroyWindow(window: isize) -> i32;
    fn OpenClipboard(owner: isize) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn GetClipboardData(format: u32) -> isize;
    fn SetClipboardData(format: u32, mem: isize) -> isize;
    fn IsClipboardFormatAvailable(format: u32) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> isize;
    fn GlobalFree(mem: isize) -> isize;
    fn GlobalLock(mem: isize) -> *mut u16;
    fn GlobalUnlock(mem: isize) -> i32;
    fn GlobalSize(mem: isize) -> usize;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn DragQueryFileW(hdrop: isize, ifile: u32, file: *mut u16, cch: u32) -> u32;
}

const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const DRAG_QUERY_COUNT: u32 = 0xFFFF_FFFF;
const GMEM_MOVEABLE: u32 = 0x0002;

const OPEN_ATTEMPTS: u32 = 5;
const OPEN_RETRY: std::time::Duration = std::time::Duration::from_millis(10);

struct OpenGuard;

struct ClipboardOwner(isize);

impl ClipboardOwner {
    fn new() -> Option<Self> {
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        // SAFETY: NUL-terminated system class on HWND_MESSAGE (-3), destroyed on this thread.
        let window = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                -3,
                0,
                0,
                std::ptr::null_mut(),
            )
        };
        (window != 0).then(|| Self(window))
    }
}

impl Drop for ClipboardOwner {
    fn drop(&mut self) {
        // SAFETY: this live window was created on this thread and is not shared. The clipboard's
        // open guard has already closed.
        unsafe { DestroyWindow(self.0) };
    }
}

impl Drop for OpenGuard {
    fn drop(&mut self) {
        // SAFETY: constructed only after OpenClipboard succeeded on this thread.
        unsafe {
            CloseClipboard();
        }
    }
}

fn open_clipboard(owner: isize) -> Option<OpenGuard> {
    for attempt in 0..OPEN_ATTEMPTS {
        // SAFETY: owner is 0 or a live same-thread HWND; OpenGuard's Drop closes it.
        if unsafe { OpenClipboard(owner) } != 0 {
            return Some(OpenGuard);
        }
        if attempt + 1 < OPEN_ATTEMPTS {
            std::thread::sleep(OPEN_RETRY);
        }
    }
    None
}

/// Read the system CLIPBOARD as UTF-16 text (`\r\n` normalized to `\n`), or `None`.
pub(crate) fn get() -> Option<String> {
    // SAFETY: plain format query, callable anywhere.
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) } == 0 {
        return None;
    }
    let _guard = open_clipboard(0)?;
    // SAFETY: the clipboard is open (guard live). The returned handle is owned by the clipboard —
    // we only lock/read/unlock it, never free it, and read only within GlobalSize.
    let text = unsafe {
        let mem = GetClipboardData(CF_UNICODETEXT);
        if mem == 0 {
            return None;
        }
        let ptr = GlobalLock(mem);
        if ptr.is_null() {
            return None;
        }
        let max_units = GlobalSize(mem) / 2;
        let mut len = 0usize;
        while len < max_units && *ptr.add(len) != 0 {
            len += 1;
        }
        let units = std::slice::from_raw_parts(ptr, len);
        let s = String::from_utf16_lossy(units);
        GlobalUnlock(mem);
        s
    };
    if text.is_empty() {
        return None;
    }
    Some(text.replace("\r\n", "\n"))
}

/// Read `CF_HDROP` copied file paths in clipboard order, or `None` (separate from [`get`] so
/// only terminal paste receives Explorer file copies when `CF_UNICODETEXT` is absent).
pub(crate) fn get_paths() -> Option<Vec<String>> {
    // SAFETY: plain format query, callable anywhere.
    if unsafe { IsClipboardFormatAvailable(CF_HDROP) } == 0 {
        return None;
    }
    let _guard = open_clipboard(0)?;
    // SAFETY: the clipboard is open (guard live). The HDROP handle is owned by the clipboard —
    // DragQueryFileW only reads it. Buffers are sized by the API's own length query (+1 for the
    // NUL it always appends) before the fill.
    let paths = unsafe {
        let drop = GetClipboardData(CF_HDROP);
        if drop == 0 {
            return None;
        }
        let count = DragQueryFileW(drop, DRAG_QUERY_COUNT, std::ptr::null_mut(), 0);
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(drop, i, std::ptr::null_mut(), 0);
            if len == 0 {
                continue; // a malformed entry: skip it, keep the rest
            }
            let mut buf = vec![0u16; len as usize + 1];
            let copied = DragQueryFileW(drop, i, buf.as_mut_ptr(), buf.len() as u32);
            if copied == 0 {
                continue;
            }
            out.push(String::from_utf16_lossy(&buf[..copied as usize]));
        }
        out
    };
    (!paths.is_empty()).then_some(paths)
}

fn to_crlf(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains('\n') {
        std::borrow::Cow::Owned(text.replace("\r\n", "\n").replace('\n', "\r\n"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// Place `text` on the system CLIPBOARD as `CF_UNICODETEXT` (CRLF-normalized).
pub(crate) fn set(text: &str) -> bool {
    let text = to_crlf(text);
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = units.len() * 2;

    crate::control::write_with_owner(
        ClipboardOwner::new,
        |owner| open_clipboard(owner.0),
        || set_open(&units, bytes),
    )
}

fn set_open(units: &[u16], bytes: usize) -> bool {
    // SAFETY: the clipboard is open (guard live). GlobalAlloc/GlobalLock give a writable, movable
    // buffer of at least `bytes`; the copy stays in bounds. After a successful SetClipboardData the
    // system owns `mem` (we must not free it); on any failure we GlobalFree it ourselves.
    unsafe {
        if EmptyClipboard() == 0 {
            return false;
        }
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if mem == 0 {
            return false;
        }
        let ptr = GlobalLock(mem);
        if ptr.is_null() {
            GlobalFree(mem);
            return false;
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), ptr, units.len());
        GlobalUnlock(mem);
        if SetClipboardData(CF_UNICODETEXT, mem) == 0 {
            GlobalFree(mem);
            return false;
        }
    }
    true
}

#[cfg(test)]
#[path = "clipboard_win_tests.rs"]
mod tests;
