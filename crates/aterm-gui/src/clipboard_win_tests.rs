// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const CHILD: &str = "ATERM_TEST_CLIPBOARD_PRIVATE_STATION";

#[test]
fn the_real_clipboard_round_trips_on_a_private_window_station() {
    if std::env::var_os(CHILD).is_some() {
        round_trip_on_a_private_station();
        return;
    }
    aterm_uds::host::could_not_run_in_session_zero(
        "the test makes a private window station for its clipboard",
    );
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
