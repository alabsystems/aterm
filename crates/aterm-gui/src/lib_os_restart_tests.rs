// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{IMAGE_SUBSYSTEM_WINDOWS_GUI, ImageSubsystem, pe_subsystem, registers_for_os_restart};

#[test]
fn only_a_window_in_the_windowed_image_registers_for_os_restart() {
    use ImageSubsystem::{NotWindowed, Windowed};
    assert!(registers_for_os_restart(false, Windowed));
    assert!(!registers_for_os_restart(true, Windowed), "headless");
    assert!(
        !registers_for_os_restart(false, NotWindowed),
        "the console image"
    );
    assert!(!registers_for_os_restart(true, NotWindowed));
}

fn header(nt: usize, subsystem: u16) -> Vec<u8> {
    let mut head = vec![0u8; nt + 4 + 20 + 70];
    head[..2].copy_from_slice(b"MZ");
    head[0x3C..0x40].copy_from_slice(&u32::try_from(nt).expect("small").to_le_bytes());
    head[nt..nt + 4].copy_from_slice(b"PE\0\0");
    head[nt + 92..nt + 94].copy_from_slice(&subsystem.to_le_bytes());
    head
}

#[test]
fn the_subsystem_is_read_from_the_pe_header_and_nothing_else_is_one() {
    assert_eq!(
        pe_subsystem(&header(0x80, 2)),
        Some(IMAGE_SUBSYSTEM_WINDOWS_GUI)
    );
    assert_eq!(
        pe_subsystem(&header(0x108, 3)),
        Some(3),
        "the console image"
    );
    let mut not_mz = header(0x80, 2);
    not_mz[0] = b'X';
    assert_eq!(pe_subsystem(&not_mz), None, "no MZ");
    let mut not_pe = header(0x80, 2);
    not_pe[0x80] = b'X';
    assert_eq!(pe_subsystem(&not_pe), None, "no PE signature");
    let short = header(0x80, 2);
    assert_eq!(
        pe_subsystem(&short[..0x80 + 93]),
        None,
        "a read that ends inside the field"
    );
    let mut far = header(0x80, 2);
    far[0x3C..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(pe_subsystem(&far), None, "an NT offset past the read");
    assert_eq!(pe_subsystem(b""), None);
    assert_eq!(
        ImageSubsystem::of_head(&header(0x80, 2)),
        ImageSubsystem::Windowed
    );
    assert_eq!(
        ImageSubsystem::of_head(&header(0x108, 3)),
        ImageSubsystem::NotWindowed
    );
    assert_eq!(ImageSubsystem::of_head(&far), ImageSubsystem::NotWindowed);
}

#[cfg(windows)]
#[test]
fn the_windowed_image_is_told_from_the_console_image_by_its_own_header() {
    let system = std::env::var_os("SystemRoot")
        .map_or_else(|| std::path::PathBuf::from(r"C:\Windows"), Into::into);
    let notepad = system.join("System32").join("notepad.exe");
    assert_eq!(
        super::win32::image_subsystem(&notepad),
        ImageSubsystem::Windowed,
        "{}",
        notepad.display()
    );
    let exe = std::env::current_exe().expect("this test binary");
    assert_eq!(
        super::win32::image_subsystem(&exe),
        ImageSubsystem::NotWindowed,
        "{}",
        exe.display()
    );
    assert_eq!(super::win32::running_image(), ImageSubsystem::NotWindowed);
    assert_eq!(
        super::win32::image_subsystem(&exe.with_extension("no-such-image")),
        ImageSubsystem::NotWindowed,
        "a file that cannot be read"
    );
}
