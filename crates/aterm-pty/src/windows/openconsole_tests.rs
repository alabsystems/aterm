// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_host_command_line_quotes_the_path() {
    let line = host_command_line(
        Path::new(r"C:\Program Files\aterm\OpenConsole.exe"),
        ffi::COORD { x: 120, y: 30 },
        0x2c4,
        0x1a8,
    );
    assert_eq!(line.last(), Some(&0), "NUL-terminated");
    assert_eq!(
        String::from_utf16_lossy(&line[..line.len() - 1]),
        "\"C:\\Program Files\\aterm\\OpenConsole.exe\" --headless --width 120 --height 30 \
         --signal 0x2c4 --server 0x1a8"
    );
}

#[test]
fn the_pseudoconsole_slots_are_three_pointers() {
    let slots: [HANDLE; 3] = [1, 2, 3];
    assert_eq!(std::mem::size_of_val(&slots), 24);
    let base = slots.as_ptr() as usize;
    assert_eq!(std::ptr::addr_of!(slots[1]) as usize - base, 8);
    assert_eq!(std::ptr::addr_of!(slots[2]) as usize - base, 16);
}

/// An event standing in for a process handle: signalled is exited.
fn process_standin(exited: bool) -> Handle {
    // SAFETY: NULL attributes and name; manual-reset, so a signal stays.
    let event =
        unsafe { ffi::CreateEventW(std::ptr::null_mut(), 1, i32::from(exited), std::ptr::null()) };
    assert_ne!(event, 0);
    Handle::new(event)
}

/// Pipes whose output end holds `bytes`, as a host's first write left them.
fn output_holding(bytes: &[u8]) -> super::super::Pipes {
    let pipes = super::super::Pipes::new().expect("pipes");
    if !bytes.is_empty() {
        let mut wrote = 0u32;
        // SAFETY: our own live pipe end and a buffer of the stated length.
        let ok = unsafe {
            ffi::WriteFile(
                pipes.out_write.raw(),
                bytes.as_ptr(),
                bytes.len() as u32,
                &mut wrote,
                std::ptr::null_mut(),
            )
        };
        assert!(ok != 0 && wrote as usize == bytes.len());
    }
    pipes
}

#[test]
fn the_health_check_blames_the_host_only_for_what_the_host_did() {
    let window = Duration::from_millis(30);
    let check = |host_exited: bool, client_exited: bool, out: &[u8]| {
        let (host, client) = (process_standin(host_exited), process_standin(client_exited));
        let pipes = output_holding(out);
        await_first_output(host.raw(), client.raw(), pipes.out_read.raw(), window)
    };
    assert_eq!(check(false, false, b"\x1b[1t"), Health::Started);
    assert_eq!(
        check(false, true, b"\x1b[1t"),
        Health::Started,
        "it answered before the client went"
    );
    assert_eq!(check(false, true, b""), Health::NoClient);
    assert!(
        matches!(check(false, false, b""), Health::Failed { ref why, sticky: false } if why == "it wrote nothing in 30 ms")
    );
    assert!(matches!(
        check(true, false, b""),
        Health::Failed { sticky: true, .. }
    ));
    assert!(matches!(
        check(true, true, b""),
        Health::Failed { sticky: true, .. }
    ));
    assert!(
        matches!(check(false, false, b"usage"), Health::Failed { ref why, sticky: true } if why.contains("0x75"))
    );
}

#[test]
fn a_gui_image_is_told_from_a_console_one() {
    let root = std::path::PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
    assert!(!is_gui_image(&root.join("System32").join("cmd.exe")));
    assert!(is_gui_image(&root.join("explorer.exe")));
    assert!(!is_gui_image(&root.join("no-such-program.exe")));
    assert!(!is_gui_image(
        &root
            .join("System32")
            .join("drivers")
            .join("etc")
            .join("hosts")
    ));
}

#[test]
fn a_system_command_names_its_program_by_full_path() {
    let line = system_command("cmd.exe", "/d /c exit");
    let text = String::from_utf16_lossy(&line[..line.len() - 1]);
    assert!(text.starts_with('"'), "{text}");
    assert!(
        text.to_ascii_lowercase()
            .ends_with("\\system32\\cmd.exe\" /d /c exit"),
        "{text}"
    );
}
