// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    ALL_FILES, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
    FOS_NODEREFERENCELINKS, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FilterSpecs, dialog_options,
    image_rows, is_cancelled, path_from_nul_terminated,
};
use std::path::PathBuf;

/// Read one built `COMDLG_FILTERSPEC` row back through its raw pointers, the
/// way the shell will.
fn read_row(specs: &FilterSpecs, index: usize) -> (String, String) {
    // SAFETY: `specs` owns both buffers and is alive for this call; each is NUL-terminated by `wide`.
    unsafe {
        let row = &*specs.as_ptr().add(index);
        let read = |p: *const u16| {
            let mut len = 0usize;
            while *p.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
        };
        (read(row.name), read(row.spec))
    }
}

/// The document type list must still be UNRESTRICTED — the macOS document
/// panel sets no allowed content types, and a filter here would silently
/// hide files the Markdown/editor runtimes accept.
#[test]
fn the_filter_table_is_one_unrestricted_row() {
    let specs = FilterSpecs::new(&ALL_FILES);
    assert_eq!(specs.len(), 1, "one row");
    assert_eq!(
        read_row(&specs, 0),
        ("All Files".to_string(), "*.*".to_string()),
        "the row must match everything"
    );
}

/// Multi-row construction, with a non-ASCII label, survives the move into the struct.
#[test]
fn every_filter_row_round_trips_through_its_pointers() {
    let rows = [
        ("Images (PNG)", "*.png"),
        ("Zeichnungen — Größe", "*.svg;*.pdf"),
    ];
    let specs = FilterSpecs::new(&rows);
    assert_eq!(specs.len(), 2);
    for (i, (name, spec)) in rows.iter().enumerate() {
        assert_eq!(
            read_row(&specs, i),
            ((*name).to_string(), (*spec).to_string()),
            "row {i}"
        );
    }
}

/// The wallpaper picker offers exactly what the wallpaper decoder reads
/// here: PNG and JPEG first-party, and what the imaging component's
/// decoders claim (BMP ships with every Windows).
#[test]
fn the_image_row_offers_what_the_decoders_read() {
    let specs = FilterSpecs::new(&image_rows());
    assert_eq!(specs.len(), 1, "one row");
    let (name, spec) = read_row(&specs, 0);
    assert_eq!(name, "Images");
    for pattern in ["*.png", "*.jpg", "*.jpeg", "*.bmp"] {
        assert!(
            spec.split(';').any(|row| row == pattern),
            "{pattern} in {spec}"
        );
    }
}

/// The three bits the macOS panel turns off come out off even when the defaults had them
/// on: one resolved file, never a folder or a list.
#[test]
fn the_options_mirror_the_macos_panel() {
    let opts = dialog_options(0);
    assert_eq!(
        opts & FOS_FORCEFILESYSTEM,
        FOS_FORCEFILESYSTEM,
        "a real path"
    );
    assert_eq!(opts & FOS_FILEMUSTEXIST, FOS_FILEMUSTEXIST, "an open panel");
    assert_eq!(opts & FOS_PATHMUSTEXIST, FOS_PATHMUSTEXIST, "a real folder");

    let hostile = FOS_PICKFOLDERS | FOS_ALLOWMULTISELECT | FOS_NODEREFERENCELINKS;
    let opts = dialog_options(hostile);
    assert_eq!(opts & FOS_PICKFOLDERS, 0, "canChooseDirectories(false)");
    assert_eq!(
        opts & FOS_ALLOWMULTISELECT,
        0,
        "allowsMultipleSelection(false)"
    );
    assert_eq!(opts & FOS_NODEREFERENCELINKS, 0, "resolvesAliases(true)");
}

/// The dialog's other defaults pass through: we fold, we do not clobber.
#[test]
fn unrelated_shell_defaults_survive() {
    let unrelated = 0x0200_0000 | 0x1000_0000;
    assert_eq!(
        dialog_options(unrelated) & unrelated,
        unrelated,
        "an option we do not speak about must pass through"
    );
}

/// Only `HRESULT_FROM_WIN32(ERROR_CANCELLED)` reads as a cancel; every other failure
/// stays a failure.
#[test]
fn only_error_cancelled_reads_as_a_cancel() {
    assert!(is_cancelled(0x8007_04C7u32 as i32), "ERROR_CANCELLED");
    assert!(!is_cancelled(0), "S_OK");
    assert!(!is_cancelled(1), "S_FALSE");
    assert!(!is_cancelled(0x8000_4005u32 as i32), "E_FAIL");
    assert!(
        !is_cancelled(0x8007_0005u32 as i32),
        "ERROR_ACCESS_DENIED is a fault, not a cancel"
    );
}

/// UNC shares, spaces and non-ASCII names round-trip exactly.
#[test]
fn shell_paths_round_trip_exactly() {
    for original in [
        r"C:\Users\m6-an\pictures\wall.png",
        r"\\fileserver\share\Design Assets\背景 v2.png",
        r"C:\Users\m6-an\My Pictures\naïve — copy (1).jpeg",
        r"\\?\C:\very\long\path\image.png",
    ] {
        let units: Vec<u16> = original.encode_utf16().chain(std::iter::once(0)).collect();
        assert_eq!(
            path_from_nul_terminated(&units),
            PathBuf::from(original),
            "{original}"
        );
    }
}

/// The scan stops at the first NUL: a CoTaskMem buffer may be longer than its string.
#[test]
fn the_path_stops_at_the_terminator() {
    let mut units: Vec<u16> = r"C:\a.png".encode_utf16().collect();
    units.push(0);
    units.extend("GARBAGE".encode_utf16());
    units.push(0);
    assert_eq!(path_from_nul_terminated(&units), PathBuf::from(r"C:\a.png"));
}

/// An unpaired surrogate (a legal NTFS filename) survives byte-exact.
#[test]
fn a_lone_surrogate_survives_instead_of_becoming_a_replacement_char() {
    let mut units: Vec<u16> = r"C:\".encode_utf16().collect();
    units.push(0xD800);
    units.extend(".png".encode_utf16());
    units.push(0);
    let path = path_from_nul_terminated(&units);
    let back: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(path.as_os_str()).collect();
    assert_eq!(back, units[..units.len() - 1], "byte-exact, not lossy");
    assert!(
        !path.to_string_lossy().is_empty(),
        "the lossy VIEW still renders; only the stored units must be exact"
    );
}

/// The shell picker is available on the machine running this test: the probe the
/// palette's File rows and the Wallpaper button gate on.
#[test]
fn the_shell_picker_is_available_on_this_machine() {
    assert!(
        super::available(),
        "CoCreateInstance(CLSID_FileOpenDialog) must answer on a desktop Windows build"
    );
}
