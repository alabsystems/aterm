// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Win32 clipboard backend (the Windows twin of `clipboard_x11`):
//! `CF_UNICODETEXT` get/set over direct `unsafe extern "system"` user32 +
//! kernel32 FFI — user32 is the unavoidable clipboard surface (there is no
//! kernel32-only or std alternative), and it is on the approved tiny-FFI list
//! for exactly this use. UTF-8 <-> UTF-16 conversion is std. A separate
//! `CF_HDROP` reader ([`get_paths`], shell32 `DragQueryFileW`) serves ONLY the
//! terminal paste path, so an Explorer file-copy pastes as quoted paths (S9)
//! without ever reaching the text consumers that share [`get`].
//!
//! The Win32 clipboard is a contended GLOBAL: `OpenClipboard` fails spuriously
//! while any other process (a clipboard manager, another copy) holds it, so
//! both entry points retry on a short bounded loop rather than failing the
//! user's copy/paste on first contention. Wired into `control_selection`'s
//! `pbcopy`/`pbpaste`, so the `copy` verb, the GUI shortcuts, copy-on-select,
//! and OSC 52 all ride it. PRIMARY (`primary_set`/`primary_get`) has no
//! Windows analogue and stays a no-op in `control_selection`.

#[link(name = "user32")]
unsafe extern "system" {
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

// The documented reader for a CF_HDROP file list (audit S9). shell32 is on the
// approved tiny-FFI allowlist (see `app_mouse`'s ShellExecuteW); there is no
// user32/kernel32/std alternative — parsing the DROPFILES block by hand would
// re-implement exactly this API against an undocumented-layout temptation.
#[link(name = "shell32")]
unsafe extern "system" {
    fn DragQueryFileW(hdrop: isize, ifile: u32, file: *mut u16, cch: u32) -> u32;
}

/// UTF-16 text with a trailing NUL — the one format aterm reads/writes; the
/// system synthesizes `CF_TEXT`/`CF_OEMTEXT` from it for legacy consumers.
const CF_UNICODETEXT: u32 = 13;
/// An Explorer file-copy: a global DROPFILES block listing the copied paths.
/// Read by [`get_paths`] ONLY — never by [`get`], see its doc for why.
const CF_HDROP: u32 = 15;
/// `DragQueryFileW`'s "give me the file COUNT" index sentinel.
const DRAG_QUERY_COUNT: u32 = 0xFFFF_FFFF;
/// Movable global memory, the documented allocation kind for clipboard data.
const GMEM_MOVEABLE: u32 = 0x0002;

/// Bounded `OpenClipboard` retry: 5 attempts, 10 ms apart (the clipboard is a
/// contended global; a manager snooping a copy holds it for a few ms).
const OPEN_ATTEMPTS: u32 = 5;
const OPEN_RETRY: std::time::Duration = std::time::Duration::from_millis(10);

/// RAII guard for the open clipboard so every early return closes it.
struct OpenGuard;

impl Drop for OpenGuard {
    fn drop(&mut self) {
        // SAFETY: constructed only after OpenClipboard succeeded on this thread.
        unsafe {
            CloseClipboard();
        }
    }
}

/// Open the clipboard with the bounded retry loop; `None` when it stays held.
fn open_clipboard() -> Option<OpenGuard> {
    for attempt in 0..OPEN_ATTEMPTS {
        // SAFETY: 0 owner = associate the open with the current task; the
        // matching CloseClipboard is the guard's Drop.
        if unsafe { OpenClipboard(0) } != 0 {
            return Some(OpenGuard);
        }
        if attempt + 1 < OPEN_ATTEMPTS {
            std::thread::sleep(OPEN_RETRY);
        }
    }
    None
}

/// Read the system CLIPBOARD as text, or `None` when empty/unavailable.
/// UTF-16 up to the first NUL, decoded lossily; `\r\n` is normalized to `\n`
/// (terminal paste convention — the engine's bracketed-paste handling then
/// applies as on every other platform).
pub(crate) fn get() -> Option<String> {
    // Cheap pre-check without opening: nothing to paste, no contention.
    // SAFETY: plain format query, callable anywhere.
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) } == 0 {
        return None;
    }
    let _guard = open_clipboard()?;
    // SAFETY: the clipboard is open (guard live). The returned handle is owned
    // by the clipboard — we only lock/read/unlock it, never free it.
    let text = unsafe {
        let mem = GetClipboardData(CF_UNICODETEXT);
        if mem == 0 {
            return None;
        }
        let ptr = GlobalLock(mem);
        if ptr.is_null() {
            return None;
        }
        // Bound the scan by the allocation size, then stop at the first NUL
        // (the clipboard contract is NUL-terminated text but the allocation
        // may be larger).
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

/// Read the system CLIPBOARD as a FILE LIST (`CF_HDROP` — what Explorer's
/// Ctrl+C on a file puts up), or `None` when the clipboard holds no file list.
/// Returns each file's full path, in clipboard order.
///
/// DELIBERATELY NOT folded into [`get`] (audit S9): `get`/`pbpaste` is shared
/// with the find bar and the rename editor, which must never receive quoted
/// path spam when the user last copied a file in Explorer. The ONE consumer is
/// the terminal paste path (`App::paste_clipboard_into`), which formats the
/// paths through the drag-and-drop contract (`input::paths_paste_insertion`)
/// and only after `CF_UNICODETEXT` came up empty — text always wins, matching
/// how Explorer itself offers "Copy as path" text alongside the HDROP.
///
/// The `GetClipboardData(CF_HDROP)` handle IS the `HDROP` that `DragQueryFileW`
/// takes (an HGLOBAL over a `DROPFILES` block; the API does its own lock), so
/// no `GlobalLock` appears here. Per-file, the first call sizes the buffer
/// (null buffer → required length in UTF-16 units, excluding the NUL) and the
/// second fills it — the documented two-step. A zero count / all-empty list is
/// `None`, mirroring `get`'s empty-text contract.
pub(crate) fn get_paths() -> Option<Vec<String>> {
    // Cheap pre-check without opening: no file list, no contention.
    // SAFETY: plain format query, callable anywhere.
    if unsafe { IsClipboardFormatAvailable(CF_HDROP) } == 0 {
        return None;
    }
    let _guard = open_clipboard()?;
    // SAFETY: the clipboard is open (guard live). The HDROP handle is owned by
    // the clipboard — DragQueryFileW only reads it. Buffers are sized by the
    // API's own length query (+1 for the NUL it always appends) before the fill.
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
            // `copied` excludes the terminating NUL; decode lossily like `get`
            // so an unpaired surrogate in a hostile path cannot drop the paste.
            out.push(String::from_utf16_lossy(&buf[..copied as usize]));
        }
        out
    };
    (!paths.is_empty()).then_some(paths)
}

/// Outbound line-ending normalization: `CF_UNICODETEXT` convention is CRLF,
/// while terminal selections join lines with `\n`. Collapse first so text that
/// already carries `\r\n` is not doubled to `\r\r\n`.
fn to_crlf(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains('\n') {
        std::borrow::Cow::Owned(text.replace("\r\n", "\n").replace('\n', "\r\n"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// Place `text` on the system CLIPBOARD as `CF_UNICODETEXT` (LF normalized to
/// CRLF, the mirror of `get`'s inbound normalization). Returns whether the
/// text was placed. On success the global memory's ownership passes to the
/// system; on failure we free it ourselves.
pub(crate) fn set(text: &str) -> bool {
    let text = to_crlf(text);
    // UTF-16 + trailing NUL, sized before any Win32 call.
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = units.len() * 2;

    let Some(_guard) = open_clipboard() else {
        return false;
    };
    // SAFETY: the clipboard is open (guard live). GlobalAlloc/GlobalLock give a
    // writable buffer of at least `bytes`; the copy stays in bounds. After a
    // successful SetClipboardData the SYSTEM owns `mem` (we must not free it);
    // on any failure we GlobalFree it ourselves.
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
mod tests {
    use super::*;

    /// Round-trip through the REAL system clipboard (serialized in one test —
    /// the clipboard is a process-global; two tests racing it would flake).
    /// Skips cleanly when the clipboard is unavailable (locked session/CI).
    #[test]
    fn set_then_get_round_trips_including_multibyte() {
        let marker = format!("aterm-clip-test-{}-✓ 例", std::process::id());
        if !set(&marker) {
            eprintln!("SKIP: clipboard unavailable (held by another process?)");
            return;
        }
        assert_eq!(get().as_deref(), Some(marker.as_str()));

        // CRLF text is normalized to LF on read (terminal paste convention),
        // and LF-only copies round-trip unchanged despite the CRLF conversion
        // set() applies outbound.
        if set("line1\r\nline2") {
            assert_eq!(get().as_deref(), Some("line1\nline2"));
        }
        if set("line1\nline2") {
            assert_eq!(get().as_deref(), Some("line1\nline2"));
        }

        // A text-only clipboard has NO file list (the caller's text-first
        // precedence never needs get_paths on an ordinary copy)…
        assert_eq!(get_paths(), None);

        // …and a genuine CF_HDROP (a hand-built DROPFILES block, exactly what
        // Explorer's Ctrl+C places) round-trips through DragQueryFileW — count,
        // order, spaces and multibyte intact — while get() correctly reports no
        // TEXT is available. Same serialized test fn, same global-clipboard
        // caveat as above.
        let files = ["C:\\Program Files\\aterm\\例 ✓.txt", "C:\\tmp\\b.txt"];
        if set_hdrop_for_test(&files) {
            assert_eq!(
                get_paths().as_deref(),
                Some(&files.map(String::from)[..]),
                "CF_HDROP paths round-trip in order"
            );
            assert_eq!(get(), None, "a file-list clipboard is not text");
        }

        // Leave TEXT on the clipboard, not a phantom file list whose paths do
        // not exist (pasting that in Explorer raises an error dialog).
        let _ = set(&marker);
    }

    /// Test-only CF_HDROP writer: a DROPFILES header (pFiles=20, wide=1) plus a
    /// double-NUL-terminated UTF-16 path list — the block Explorer itself puts
    /// up. Built as u16 words (the header's five little-endian DWORDs are word
    /// pairs), which is sound on the x86/ARM little-endian targets Windows
    /// ships on. Production code never WRITES CF_HDROP; this exists so the
    /// reader is proven against the real clipboard, not a mock.
    fn set_hdrop_for_test(paths: &[&str]) -> bool {
        // DROPFILES { pFiles: 20, pt: (0,0), fNC: 0, fWide: 1 } as ten u16s.
        let mut block: Vec<u16> = vec![20, 0, 0, 0, 0, 0, 0, 0, 1, 0];
        for p in paths {
            block.extend(p.encode_utf16());
            block.push(0);
        }
        block.push(0); // list terminator (double NUL)
        let bytes = block.len() * 2;
        let Some(_guard) = open_clipboard() else {
            return false;
        };
        // SAFETY: mirrors `set` exactly — open clipboard, movable global,
        // in-bounds copy; ownership passes to the system on success.
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
            std::ptr::copy_nonoverlapping(block.as_ptr(), ptr, block.len());
            GlobalUnlock(mem);
            if SetClipboardData(CF_HDROP, mem) == 0 {
                GlobalFree(mem);
                return false;
            }
        }
        true
    }

    /// Outbound normalization is pure and testable without the clipboard.
    #[test]
    fn to_crlf_converts_lf_without_doubling_crlf() {
        assert_eq!(to_crlf("a\nb"), "a\r\nb");
        assert_eq!(to_crlf("a\r\nb"), "a\r\nb");
        assert_eq!(to_crlf("a\r\nb\nc\n"), "a\r\nb\r\nc\r\n");
        assert_eq!(to_crlf("no newline"), "no newline");
        assert!(matches!(
            to_crlf("no newline"),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}
