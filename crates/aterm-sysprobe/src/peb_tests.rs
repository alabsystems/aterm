// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::without_trailing_separator;
use std::path::{Path, PathBuf};

/// The process environment block's structures, as another process holds
/// them, have the offsets and sizes of the SDK's symbols (`ntdll`'s
/// `_PEB`, `_RTL_USER_PROCESS_PARAMETERS` and their WOW64 twins), and
/// `PROCESS_BASIC_INFORMATION` has the SDK's 64-bit layout.
#[cfg(target_pointer_width = "64")]
#[test]
fn the_process_parameters_have_their_sdk_layouts() {
    use crate::win_paths::{
        DriveLetterCurdir32, DriveLetterCurdir64, PebHead32, PebHead64, ProcessBasicInformation,
        ProcessParameters32, ProcessParameters64, RemoteString32, RemoteString64,
    };
    use std::mem::{offset_of, size_of};
    assert_eq!(size_of::<ProcessBasicInformation>(), 48);
    assert_eq!(offset_of!(ProcessBasicInformation, peb_base_address), 8);
    assert_eq!(offset_of!(PebHead64, process_parameters), 0x20);
    assert_eq!(size_of::<PebHead64>(), 0x28);
    assert_eq!(offset_of!(PebHead32, process_parameters), 0x10);
    assert_eq!(size_of::<PebHead32>(), 0x14);
    assert_eq!(size_of::<RemoteString64>(), 16);
    assert_eq!(offset_of!(RemoteString64, buffer), 8);
    assert_eq!(size_of::<RemoteString32>(), 8);
    assert_eq!(offset_of!(RemoteString32, buffer), 4);
    assert_eq!(size_of::<DriveLetterCurdir64>(), 24);
    assert_eq!(size_of::<DriveLetterCurdir32>(), 16);

    assert_eq!(offset_of!(ProcessParameters64, flags), 0x08);
    assert_eq!(offset_of!(ProcessParameters64, standard_input), 0x20);
    assert_eq!(offset_of!(ProcessParameters64, current_directory), 0x38);
    assert_eq!(offset_of!(ProcessParameters64, dll_path), 0x50);
    assert_eq!(offset_of!(ProcessParameters64, image_path_name), 0x60);
    assert_eq!(offset_of!(ProcessParameters64, command_line), 0x70);
    assert_eq!(offset_of!(ProcessParameters64, environment), 0x80);
    assert_eq!(offset_of!(ProcessParameters64, window_title), 0xB0);
    assert_eq!(offset_of!(ProcessParameters64, current_directores), 0xF0);
    assert_eq!(offset_of!(ProcessParameters64, environment_size), 0x3F0);
    assert_eq!(offset_of!(ProcessParameters64, environment_version), 0x3F8);
    assert_eq!(size_of::<ProcessParameters64>(), 0x400);

    assert_eq!(offset_of!(ProcessParameters32, flags), 0x08);
    assert_eq!(offset_of!(ProcessParameters32, current_directory), 0x24);
    assert_eq!(offset_of!(ProcessParameters32, dll_path), 0x30);
    assert_eq!(offset_of!(ProcessParameters32, image_path_name), 0x38);
    assert_eq!(offset_of!(ProcessParameters32, command_line), 0x40);
    assert_eq!(offset_of!(ProcessParameters32, environment), 0x48);
    assert_eq!(offset_of!(ProcessParameters32, window_title), 0x70);
    assert_eq!(offset_of!(ProcessParameters32, current_directores), 0x90);
    assert_eq!(offset_of!(ProcessParameters32, environment_size), 0x290);
    assert_eq!(offset_of!(ProcessParameters32, environment_version), 0x294);
    assert_eq!(size_of::<ProcessParameters32>(), 0x298);
}

#[test]
fn a_working_directory_drops_its_trailing_separator_but_at_a_root() {
    let read = |s: &str| without_trailing_separator(PathBuf::from(s));
    assert_eq!(read(r"C:\x\"), Path::new(r"C:\x"));
    assert_eq!(read(r"C:\x\y z\").as_os_str(), r"C:\x\y z");
    assert_eq!(read(r"C:\").as_os_str(), r"C:\");
    assert_eq!(read(r"\\srv\share\").as_os_str(), r"\\srv\share\");
    assert_eq!(read(r"\\srv\share\x\").as_os_str(), r"\\srv\share\x");
    assert_eq!(read(r"C:\x").as_os_str(), r"C:\x");
}

#[test]
fn a_processs_working_directory_is_read_from_its_environment_block() {
    let own = crate::process_cwd(std::process::id()).expect("our own working directory");
    let want = std::env::current_dir().expect("current dir");
    assert_eq!(own.as_os_str(), want.as_os_str());
    assert_eq!(crate::process_cwd(u32::MAX), None);
    assert_eq!(crate::process_cwd(4), None);
}
