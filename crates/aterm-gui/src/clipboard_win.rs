// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Win32 clipboard backend: `CF_UNICODETEXT` get/set over direct user32 + kernel32 FFI
//! (user32 is the only clipboard surface; it is on the approved tiny-FFI list for this use). A
//! separate `CF_HDROP` reader ([`get_paths`]) serves only the terminal paste, so an Explorer
//! file-copy pastes as quoted paths without reaching the text consumers that share [`get`].
//!
//! The Win32 clipboard is a contended global: `OpenClipboard` fails while another process (a
//! clipboard manager, another copy) holds it, so every entry point retries on a short bounded
//! loop. Reached only through `crate::clipboard`; PRIMARY has no Windows analogue.

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

// The documented CF_HDROP reader; shell32 is on the approved tiny-FFI list, and parsing the
// DROPFILES block by hand would re-implement this API.
#[link(name = "shell32")]
unsafe extern "system" {
    fn DragQueryFileW(hdrop: isize, ifile: u32, file: *mut u16, cch: u32) -> u32;
}

/// UTF-16 text with a trailing NUL — the one format aterm reads/writes; the
/// system synthesizes `CF_TEXT`/`CF_OEMTEXT` from it for legacy consumers.
const CF_UNICODETEXT: u32 = 13;
/// An Explorer file-copy: a global DROPFILES block listing the copied paths.
/// Read only by [`get_paths`] (see its doc for why).
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

/// A message-only window on the calling thread. Copy can also run on an
/// OSC 52 worker, where there is no App/winit HWND to borrow. HWND_MESSAGE
/// creates no visible desktop surface and needs no message pump for an
/// eagerly rendered clipboard value.
struct ClipboardOwner(isize);

impl ClipboardOwner {
    fn new() -> Option<Self> {
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        // SAFETY: STATIC is a system class, its name is NUL-terminated, and
        // HWND_MESSAGE (-3) makes an invisible message-only window. The
        // handle is destroyed on this same thread after CloseClipboard.
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
        // SAFETY: this live window was created on this thread and is not
        // shared. The clipboard's open guard has already closed.
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

/// Open the clipboard with the bounded retry loop; `None` when it stays held.
fn open_clipboard(owner: isize) -> Option<OpenGuard> {
    for attempt in 0..OPEN_ATTEMPTS {
        // SAFETY: owner is zero for reads or a live same-thread HWND for
        // writes; the matching CloseClipboard is the guard's Drop.
        if unsafe { OpenClipboard(owner) } != 0 {
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
    let _guard = open_clipboard(0)?;
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

/// Read the clipboard as a file list (`CF_HDROP`, what Explorer's Ctrl+C on a file puts up):
/// each file's full path in clipboard order, or `None` when there is none.
///
/// Deliberately not folded into [`get`]: `get`/`pbpaste` is shared with the find bar and the
/// rename editor, which must never receive quoted paths after an Explorer copy. The one consumer
/// is the terminal paste (`App::paste_clipboard_into`), which formats them through the
/// drag-and-drop contract (`input::paths_paste_insertion`) only when `CF_UNICODETEXT` is empty:
/// text always wins.
///
/// The `GetClipboardData(CF_HDROP)` handle is the `HDROP` that `DragQueryFileW` takes (the API
/// does its own lock). Per file, a first call sizes the buffer (UTF-16 units, excluding the NUL)
/// and a second fills it. An empty list is `None`, like `get`'s empty text.
pub(crate) fn get_paths() -> Option<Vec<String>> {
    // Cheap pre-check without opening: no file list, no contention.
    // SAFETY: plain format query, callable anywhere.
    if unsafe { IsClipboardFormatAvailable(CF_HDROP) } == 0 {
        return None;
    }
    let _guard = open_clipboard(0)?;
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

    crate::control::write_with_owner(
        ClipboardOwner::new,
        |owner| open_clipboard(owner.0),
        || set_open(&units, bytes),
    )
}

/// Place data while a real owner and its OpenGuard are both live. Passing
/// NULL to OpenClipboard before EmptyClipboard makes SetClipboardData fail.
fn set_open(units: &[u16], bytes: usize) -> bool {
    // SAFETY: the clipboard is open (guard live). GlobalAlloc/GlobalLock give a
    // writable buffer of at least `bytes`; the copy stays in bounds. After a
    // successful SetClipboardData the system owns `mem` (we must not free it);
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

    /// Marks the child run of [`round_trip_on_a_private_station`].
    const CHILD: &str = "ATERM_TEST_CLIPBOARD_PRIVATE_STATION";

    /// The real Win32 clipboard, never the person's: this test runs
    /// [`round_trip_on_a_private_station`] in a child of this test binary, which
    /// moves off the interactive window station before it opens a clipboard
    /// ([`station::enter_private`]: the logon session's own non-interactive
    /// station, the one a browser's sandbox runs on, since a standard user may
    /// not name a new one, and a desktop of its own in it). Each window station
    /// has its own clipboard, so the person's (`WinSta0`'s) is never read,
    /// emptied or written: the child makes no clipboard call before it moves,
    /// and it leaves the station's clipboard empty. Before this, the round trip
    /// ran on the developer's own clipboard and left a marker string on it at
    /// every Windows test run.
    #[test]
    fn the_real_clipboard_round_trips_on_a_private_window_station() {
        if std::env::var_os(CHILD).is_some() {
            round_trip_on_a_private_station();
            return;
        }
        let exe = std::env::current_exe().expect("this test binary");
        let out = aterm_uds::spawnfd::command(&exe)
            .args([
                "clipboard_win::tests::the_real_clipboard_round_trips_on_a_private_window_station",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .output()
            .expect("the child test run");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{text}");
        assert!(text.contains("1 passed"), "{text}");
        assert!(text.contains("PRIVATE-STATION-ROUND-TRIP-OK"), "{text}");
    }

    /// The parent's child role: it enters the private station before any clipboard call.
    fn round_trip_on_a_private_station() {
        station::enter_private();
        // The station outlives this process when another holds it (the logon
        // session's station is a sandbox's too), so start and end it empty.
        station::empty();
        assert_eq!(get(), None, "the station's clipboard is empty");
        assert_eq!(get_paths(), None);

        let marker = format!("aterm-clip-test-{}-✓ 例", std::process::id());
        assert!(set(&marker), "the private clipboard takes text");
        assert_eq!(get().as_deref(), Some(marker.as_str()));

        // Outbound LF becomes CF_UNICODETEXT's CRLF (read raw), inbound CRLF
        // becomes LF, and CRLF is never doubled.
        assert!(set("line1\nline2\n"));
        assert_eq!(station::raw_text().as_deref(), Some("line1\r\nline2\r\n"));
        assert_eq!(get().as_deref(), Some("line1\nline2\n"));
        assert!(set("line1\r\nline2"));
        assert_eq!(station::raw_text().as_deref(), Some("line1\r\nline2"));
        assert_eq!(get().as_deref(), Some("line1\nline2"));

        // A text-only clipboard has NO file list (the caller's text-first
        // precedence never needs get_paths on an ordinary copy)…
        assert_eq!(get_paths(), None);

        // …and a hand-built CF_HDROP (what Explorer's Ctrl+C places) round-trips through
        // DragQueryFileW — count, order, spaces and multibyte intact — while get() reports no
        // text.
        let files = ["C:\\Program Files\\aterm\\例 ✓.txt", "C:\\tmp\\b.txt"];
        assert!(set_hdrop_for_test(&files));
        assert_eq!(
            get_paths().as_deref(),
            Some(&files.map(String::from)[..]),
            "CF_HDROP paths round-trip in order"
        );
        assert_eq!(get(), None, "a file-list clipboard is not text");
        station::empty();
        println!("PRIVATE-STATION-ROUND-TRIP-OK");
    }

    /// A window station and desktop of this process's own, for the clipboard
    /// round trip: each window station has its own clipboard.
    mod station {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn CreateWindowStationW(
                name: *const u16,
                flags: u32,
                access: u32,
                security: *const std::ffi::c_void,
            ) -> isize;
            fn SetProcessWindowStation(station: isize) -> i32;
            fn CreateDesktopW(
                name: *const u16,
                device: *const u16,
                mode: *const std::ffi::c_void,
                flags: u32,
                access: u32,
                security: *const std::ffi::c_void,
            ) -> isize;
            fn SetThreadDesktop(desktop: isize) -> i32;
            fn GetProcessWindowStation() -> isize;
            fn GetUserObjectInformationW(
                object: isize,
                index: i32,
                info: *mut std::ffi::c_void,
                len: u32,
                needed: *mut u32,
            ) -> i32;
        }

        const WINSTA_ALL_ACCESS: u32 = 0x37F;
        const GENERIC_ALL: u32 = 0x1000_0000;
        /// `GetUserObjectInformationW`'s name index.
        const UOI_NAME: i32 = 2;

        /// The name of `object` (a window station or desktop).
        fn name_of(object: isize) -> String {
            let mut buf = [0u16; 256];
            let mut needed = 0u32;
            // SAFETY: `buf` is writable for its byte length, which is what is passed.
            let ok = unsafe {
                GetUserObjectInformationW(
                    object,
                    UOI_NAME,
                    buf.as_mut_ptr().cast(),
                    std::mem::size_of_val(&buf) as u32,
                    &mut needed,
                )
            };
            assert_ne!(ok, 0, "GetUserObjectInformationW");
            let end = buf.iter().position(|&unit| unit == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..end])
        }

        /// Move this process onto a new window station, and this thread onto a
        /// new desktop in it, or fail the test. The handles stay open for the
        /// life of the (child) process.
        pub(super) fn enter_private() {
            let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() };
            // SAFETY: a no-argument query of this process's window station.
            let interactive = name_of(unsafe { GetProcessWindowStation() });
            // An unnamed station (a standard user may not name one): the
            // system names it after the logon session, as a sandbox's is.
            // SAFETY: no name; default security.
            let station = unsafe {
                CreateWindowStationW(std::ptr::null(), 0, WINSTA_ALL_ACCESS, std::ptr::null())
            };
            assert_ne!(
                station,
                0,
                "CreateWindowStationW: {}",
                std::io::Error::last_os_error()
            );
            // SAFETY: `station` is the live handle just created.
            assert_ne!(
                unsafe { SetProcessWindowStation(station) },
                0,
                "SetProcessWindowStation"
            );
            let desk = wide("aterm-clip");
            // SAFETY: `desk` is NUL-terminated; no device or mode; default security.
            let desktop = unsafe {
                CreateDesktopW(
                    desk.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    GENERIC_ALL,
                    std::ptr::null(),
                )
            };
            assert_ne!(
                desktop,
                0,
                "CreateDesktopW: {}",
                std::io::Error::last_os_error()
            );
            // SAFETY: this test thread owns no window or hook yet.
            assert_ne!(unsafe { SetThreadDesktop(desktop) }, 0, "SetThreadDesktop");
            // SAFETY: as above.
            let now = name_of(unsafe { GetProcessWindowStation() });
            assert_ne!(now, interactive, "off {interactive}");
            eprintln!("window station {interactive} -> {now}");
        }

        /// Empty this station's clipboard, or fail the test.
        pub(super) fn empty() {
            let owner = super::ClipboardOwner::new().expect("a message-only owner");
            let _guard = super::open_clipboard(owner.0).expect("the station's clipboard");
            // SAFETY: the clipboard is open by `owner`, which outlives the call.
            assert_ne!(unsafe { super::EmptyClipboard() }, 0, "EmptyClipboard");
        }

        /// The clipboard's `CF_UNICODETEXT` exactly as stored (no CRLF
        /// normalization), or `None`.
        pub(super) fn raw_text() -> Option<String> {
            let _guard = super::open_clipboard(0)?;
            // SAFETY: the clipboard is open; the handle is the clipboard's, only
            // locked, read up to its NUL within its size, and unlocked.
            unsafe {
                let mem = super::GetClipboardData(super::CF_UNICODETEXT);
                if mem == 0 {
                    return None;
                }
                let ptr = super::GlobalLock(mem);
                if ptr.is_null() {
                    return None;
                }
                let max = super::GlobalSize(mem) / 2;
                let mut len = 0;
                while len < max && *ptr.add(len) != 0 {
                    len += 1;
                }
                let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
                super::GlobalUnlock(mem);
                Some(text)
            }
        }
    }

    /// Test-only CF_HDROP writer: a DROPFILES header (pFiles=20, wide=1) plus a
    /// double-NUL-terminated UTF-16 path list, built as u16 words (sound on the little-endian
    /// targets Windows ships on). Production code never writes CF_HDROP.
    fn set_hdrop_for_test(paths: &[&str]) -> bool {
        // DROPFILES { pFiles: 20, pt: (0,0), fNC: 0, fWide: 1 } as ten u16s.
        let mut block: Vec<u16> = vec![20, 0, 0, 0, 0, 0, 0, 0, 1, 0];
        for p in paths {
            block.extend(p.encode_utf16());
            block.push(0);
        }
        block.push(0); // list terminator (double NUL)
        let bytes = block.len() * 2;
        let Some(owner) = ClipboardOwner::new() else {
            return false;
        };
        let Some(_guard) = open_clipboard(owner.0) else {
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
