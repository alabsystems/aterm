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
mod tests {
    use super::*;

    const CHILD: &str = "ATERM_TEST_CLIPBOARD_PRIVATE_STATION";

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

    fn round_trip_on_a_private_station() {
        station::enter_private();
        station::empty();
        assert_eq!(get(), None, "the station's clipboard is empty");
        assert_eq!(get_paths(), None);

        let marker = format!("aterm-clip-test-{}-✓ 例", std::process::id());
        assert!(set(&marker), "the private clipboard takes text");
        assert_eq!(get().as_deref(), Some(marker.as_str()));

        assert!(set("line1\nline2\n"));
        assert_eq!(station::raw_text().as_deref(), Some("line1\r\nline2\r\n"));
        assert_eq!(get().as_deref(), Some("line1\nline2\n"));
        assert!(set("line1\r\nline2"));
        assert_eq!(station::raw_text().as_deref(), Some("line1\r\nline2"));
        assert_eq!(get().as_deref(), Some("line1\nline2"));

        assert_eq!(get_paths(), None);

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
        const UOI_NAME: i32 = 2;

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

        /// Move this process onto a private window station and thread desktop.
        pub(super) fn enter_private() {
            let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() };
            // SAFETY: a no-argument query of this process's window station.
            let interactive = name_of(unsafe { GetProcessWindowStation() });
            // SAFETY: unnamed station with default security.
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

        /// Read `CF_UNICODETEXT` without CRLF normalization.
        pub(super) fn raw_text() -> Option<String> {
            let _guard = super::open_clipboard(0)?;
            // SAFETY: the clipboard is open; the handle is the clipboard's, only locked, read up to
            // its NUL within its size, and unlocked.
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

    fn set_hdrop_for_test(paths: &[&str]) -> bool {
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
        // SAFETY: open clipboard, movable global sized for `block`; system owns on success.
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
