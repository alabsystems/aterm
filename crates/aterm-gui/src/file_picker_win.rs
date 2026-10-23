// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Windows local-file picker: the `IFileOpenDialog` behind Settings ▸ Wallpaper's "Choose Image…" and the File ▸ Open Markdown / Open File rows.
#![cfg(windows)]

use std::ffi::c_void;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::win_ffi::{Apartment, Com, Guid, StepError, check, co_create, wide};

/// `CLSID_FileOpenDialog` {DC1C5A9C-E88A-4DDE-A5A1-60F82A20AEF7}.
const CLSID_FILE_OPEN_DIALOG: Guid = Guid {
    data1: 0xDC1C_5A9C,
    data2: 0xE88A,
    data3: 0x4DDE,
    data4: [0xA5, 0xA1, 0x60, 0xF8, 0x2A, 0x20, 0xAE, 0xF7],
};
/// `IID_IFileOpenDialog` {D57C7288-D4AD-4768-BE02-9D969532D960}.
const IID_IFILE_OPEN_DIALOG: Guid = Guid {
    data1: 0xD57C_7288,
    data2: 0xD4AD,
    data3: 0x4768,
    data4: [0xBE, 0x02, 0x9D, 0x96, 0x95, 0x32, 0xD9, 0x60],
};

/// `COMDLG_FILTERSPEC` — one row of the dialog's file-type combo.
#[repr(C)]
struct FilterSpec {
    name: *const u16,
    spec: *const u16,
}

/// `IFileOpenDialog` (IUnknown → IModalWindow → IFileDialog → IFileOpenDialog).
#[repr(C)]
struct IFileOpenDialogVtbl {
    query_interface:
        unsafe extern "system" fn(*mut IFileOpenDialog, *const Guid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut IFileOpenDialog) -> u32,
    release: unsafe extern "system" fn(*mut IFileOpenDialog) -> u32,
    show: unsafe extern "system" fn(*mut IFileOpenDialog, isize) -> i32,
    set_file_types: unsafe extern "system" fn(*mut IFileOpenDialog, u32, *const FilterSpec) -> i32,
    set_file_type_index: unsafe extern "system" fn(*mut IFileOpenDialog, u32) -> i32,
    get_file_type_index: unsafe extern "system" fn(*mut IFileOpenDialog, *mut u32) -> i32,
    advise: unsafe extern "system" fn(*mut IFileOpenDialog, *mut c_void, *mut u32) -> i32,
    unadvise: unsafe extern "system" fn(*mut IFileOpenDialog, u32) -> i32,
    set_options: unsafe extern "system" fn(*mut IFileOpenDialog, u32) -> i32,
    get_options: unsafe extern "system" fn(*mut IFileOpenDialog, *mut u32) -> i32,
    set_default_folder: unsafe extern "system" fn(*mut IFileOpenDialog, *mut c_void) -> i32,
    set_folder: unsafe extern "system" fn(*mut IFileOpenDialog, *mut c_void) -> i32,
    get_folder: unsafe extern "system" fn(*mut IFileOpenDialog, *mut *mut c_void) -> i32,
    get_current_selection: unsafe extern "system" fn(*mut IFileOpenDialog, *mut *mut c_void) -> i32,
    set_file_name: unsafe extern "system" fn(*mut IFileOpenDialog, *const u16) -> i32,
    get_file_name: unsafe extern "system" fn(*mut IFileOpenDialog, *mut *mut u16) -> i32,
    set_title: unsafe extern "system" fn(*mut IFileOpenDialog, *const u16) -> i32,
    set_ok_button_label: unsafe extern "system" fn(*mut IFileOpenDialog, *const u16) -> i32,
    set_file_name_label: unsafe extern "system" fn(*mut IFileOpenDialog, *const u16) -> i32,
    get_result: unsafe extern "system" fn(*mut IFileOpenDialog, *mut *mut c_void) -> i32,
}

#[repr(C)]
struct IFileOpenDialog {
    vtbl: *const IFileOpenDialogVtbl,
}

/// `IShellItem` (shobjidl_core.h).
#[repr(C)]
struct IShellItemVtbl {
    query_interface:
        unsafe extern "system" fn(*mut IShellItem, *const Guid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut IShellItem) -> u32,
    release: unsafe extern "system" fn(*mut IShellItem) -> u32,
    bind_to_handler: unsafe extern "system" fn(
        *mut IShellItem,
        *mut c_void,
        *const Guid,
        *const Guid,
        *mut *mut c_void,
    ) -> i32,
    get_parent: unsafe extern "system" fn(*mut IShellItem, *mut *mut c_void) -> i32,
    get_display_name: unsafe extern "system" fn(*mut IShellItem, u32, *mut *mut u16) -> i32,
}

#[repr(C)]
struct IShellItem {
    vtbl: *const IShellItemVtbl,
}

/// `HRESULT_FROM_WIN32(ERROR_CANCELLED)`: what `IModalWindow::Show` returns when the user
/// dismisses the dialog.
const HRESULT_CANCELLED: i32 = 0x8007_04C7u32 as i32;
/// `SIGDN_FILESYSPATH`: the display-name form that is a filesystem path, which succeeds for
/// every item under [`FOS_FORCEFILESYSTEM`].
const SIGDN_FILESYSPATH: u32 = 0x8005_8000;

/// Directories, not files, are the selectable items.
const FOS_PICKFOLDERS: u32 = 0x0000_0020;
/// Refuse virtual items (a scanner page) that have no path. Set: the callers open a path.
const FOS_FORCEFILESYSTEM: u32 = 0x0000_0040;
/// More than one item may be returned. Cleared: `setAllowsMultipleSelection(false)`.
const FOS_ALLOWMULTISELECT: u32 = 0x0000_0200;
/// The typed folder must exist.
const FOS_PATHMUSTEXIST: u32 = 0x0000_0800;
/// The typed file must exist: this is an open panel, not a save panel.
const FOS_FILEMUSTEXIST: u32 = 0x0000_1000;
/// Return the shortcut itself rather than its target. Cleared: `setResolvesAliases(true)`.
const FOS_NODEREFERENCELINKS: u32 = 0x0010_0000;

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoTaskMemFree(block: *mut c_void);
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetActiveWindow() -> isize;
}

/// The document picker's file-type list.
const ALL_FILES: [(&str, &str); 1] = [("All Files", "*.*")];

/// The wallpaper picker's list: the images the wallpaper decoder reads here
/// (`app_config::decode_wallpaper_bytes`: PNG and JPEG first-party, and every
/// format the imaging component's installed decoders claim), from the
/// decoders' own tables, so the picker and the decoder cannot drift apart.
fn image_rows() -> [(String, String); 1] {
    [crate::app_config::wallpaper_image_filter(
        crate::image_decode_win::decodable_extensions(),
    )]
}

/// Owned `COMDLG_FILTERSPEC` rows: the wide buffers and the array of pointers into them, kept
/// together so the array cannot outlive its strings.
struct FilterSpecs {
    /// Alive for the pointers in `specs`.
    _text: Vec<Vec<u16>>,
    specs: Vec<FilterSpec>,
}

impl FilterSpecs {
    fn new<N: AsRef<str>, S: AsRef<str>>(rows: &[(N, S)]) -> Self {
        let mut text: Vec<Vec<u16>> = Vec::with_capacity(rows.len() * 2);
        for (name, spec) in rows {
            text.push(wide(name.as_ref()));
            text.push(wide(spec.as_ref()));
        }
        let specs = (0..rows.len())
            .map(|i| FilterSpec {
                name: text[i * 2].as_ptr(),
                spec: text[i * 2 + 1].as_ptr(),
            })
            .collect();
        Self { _text: text, specs }
    }

    fn len(&self) -> u32 {
        // `rows` is a table of one or two rows; the cast cannot truncate.
        self.specs.len() as u32
    }

    fn as_ptr(&self) -> *const FilterSpec {
        self.specs.as_ptr()
    }
}

/// The `FILEOPENDIALOGOPTIONS` this picker runs with, folded over the dialog's own defaults
/// (`GetOptions`) so the ones we have no opinion about survive.
fn dialog_options(defaults: u32) -> u32 {
    (defaults | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST)
        & !(FOS_PICKFOLDERS | FOS_ALLOWMULTISELECT | FOS_NODEREFERENCELINKS)
}

/// Whether an `IModalWindow::Show` HRESULT is the user cancelling (Cancel, Esc, ✕) rather
/// than a fault.
fn is_cancelled(hr: i32) -> bool {
    hr == HRESULT_CANCELLED
}

/// A `PathBuf` from a NUL-terminated UTF-16 buffer, the shape `GetDisplayName` returns.
fn path_from_nul_terminated(units: &[u16]) -> PathBuf {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    PathBuf::from(std::ffi::OsString::from_wide(&units[..end]))
}

/// Whether [`choose`] can produce a path on this machine: the predicate behind the palette's File rows and Settings ▸ Wallpaper's button.
pub(crate) fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let Ok(_apartment) = Apartment::enter() else {
            return false;
        };
        co_create(&CLSID_FILE_OPEN_DIALOG, &IID_IFILE_OPEN_DIALOG, "probe").is_ok()
    })
}

/// Present the shell open dialog and return exactly the one local path the user approved, or `None` for a cancel.
pub(crate) fn choose(title: &str, prompt: &str) -> Option<PathBuf> {
    choose_from(title, prompt, &ALL_FILES)
}

/// [`choose`] offering only the images the wallpaper decoder reads here; the Windows arm of `menu::choose_local_image`.
pub(crate) fn choose_image(title: &str, prompt: &str) -> Option<PathBuf> {
    choose_from(title, prompt, &image_rows())
}

fn choose_from<N: AsRef<str>, S: AsRef<str>>(
    title: &str,
    prompt: &str,
    types: &[(N, S)],
) -> Option<PathBuf> {
    // SAFETY: `show_dialog` is one linear pass of documented shell COM calls; every pointer it hands the shell outlives the call that reads it, every returned interface is RAII-released, and the apartment is balanced by `Apartment`'s Drop.
    match unsafe { show_dialog(title, prompt, types, owner_window()) } {
        Ok(picked) => picked,
        Err((step, hr)) => {
            aterm_log::warn!("file picker skipped ({step}: hr={hr:#010x})");
            None
        }
    }
}

/// The window the dialog is modal to: `GetActiveWindow()`, as `lib.rs`'s `TaskDialogIndirect`
/// confirm resolves its owner.
fn owner_window() -> isize {
    // SAFETY: a plain query of this thread's active window (may be 0).
    unsafe { GetActiveWindow() }
}

/// `Ok(None)` is a cancel, the user's answer; `Err` is a failure the caller turns into one
/// warn line.
unsafe fn show_dialog<N: AsRef<str>, S: AsRef<str>>(
    title: &str,
    prompt: &str,
    types: &[(N, S)],
    owner: isize,
) -> Result<Option<PathBuf>, StepError> {
    let _apartment = Apartment::enter()?;

    let dialog = co_create(
        &CLSID_FILE_OPEN_DIALOG,
        &IID_IFILE_OPEN_DIALOG,
        "create FileOpenDialog",
    )?;
    let ptr = dialog.as_raw().cast::<IFileOpenDialog>();
    let vt = unsafe { &*(*ptr).vtbl };

    let mut defaults = 0u32;
    check(
        unsafe { (vt.get_options)(ptr, &mut defaults) },
        "GetOptions",
    )?;
    check(
        unsafe { (vt.set_options)(ptr, dialog_options(defaults)) },
        "SetOptions",
    )?;

    let filters = FilterSpecs::new(types);
    check(
        unsafe { (vt.set_file_types)(ptr, filters.len(), filters.as_ptr()) },
        "SetFileTypes",
    )?;

    let title_w = wide(title);
    check(unsafe { (vt.set_title)(ptr, title_w.as_ptr()) }, "SetTitle")?;
    let prompt_w = wide(prompt);
    check(
        unsafe { (vt.set_ok_button_label)(ptr, prompt_w.as_ptr()) },
        "SetOkButtonLabel",
    )?;

    let hr = unsafe { (vt.show)(ptr, owner) };
    if is_cancelled(hr) {
        return Ok(None);
    }
    check(hr, "Show")?;

    let mut item: *mut c_void = std::ptr::null_mut();
    check(unsafe { (vt.get_result)(ptr, &mut item) }, "GetResult")?;
    // SAFETY: a successful GetResult hands over one reference to the item.
    let Some(item) = (unsafe { Com::from_raw(item) }) else {
        return Err(("GetResult", 0));
    };
    let item_ptr = item.as_raw().cast::<IShellItem>();

    let mut name: *mut u16 = std::ptr::null_mut();
    check(
        unsafe { ((*(*item_ptr).vtbl).get_display_name)(item_ptr, SIGDN_FILESYSPATH, &mut name) },
        "GetDisplayName",
    )?;
    if name.is_null() {
        return Err(("GetDisplayName", 0));
    }
    // SAFETY: `name` is a live, NUL-terminated, CoTaskMem-allocated UTF-16 string owned by us from here; the length scan stops at that NUL, and the buffer is freed with the allocator that produced it as soon as it has been copied.
    let path = unsafe {
        let mut len = 0usize;
        while *name.add(len) != 0 {
            len += 1;
        }
        let path = path_from_nul_terminated(std::slice::from_raw_parts(name, len + 1));
        CoTaskMemFree(name.cast());
        path
    };
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::{
        ALL_FILES, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
        FOS_NODEREFERENCELINKS, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FilterSpecs,
        IFileOpenDialogVtbl, IShellItemVtbl, dialog_options, image_rows, is_cancelled,
        path_from_nul_terminated,
    };
    use std::path::PathBuf;

    /// Pins the hand-rolled vtables' shape, as `task_dialog_config_layout_matches_the_sdk`
    /// does for TASKDIALOGCONFIG: a dropped or reordered uncalled slot compiles and turns a
    /// call into a wild indirect call.
    #[test]
    fn com_vtable_layout_matches_the_documented_slot_order() {
        use std::mem::{offset_of, size_of};

        assert_eq!(
            size_of::<IFileOpenDialogVtbl>(),
            21 * size_of::<usize>(),
            "IFileOpenDialog vtable is IUnknown(3) + IModalWindow(1) + IFileDialog(17)"
        );
        assert_eq!(
            size_of::<IShellItemVtbl>(),
            6 * size_of::<usize>(),
            "IShellItem vtable is IUnknown(3) + BindToHandler/GetParent/GetDisplayName"
        );

        let slot = |bytes: usize| bytes / size_of::<usize>();
        assert_eq!(
            slot(offset_of!(IFileOpenDialogVtbl, release)),
            2,
            "IUnknown::Release"
        );
        assert_eq!(
            slot(offset_of!(IFileOpenDialogVtbl, show)),
            3,
            "IModalWindow::Show"
        );
        assert_eq!(
            slot(offset_of!(IFileOpenDialogVtbl, set_file_types)),
            4,
            "IFileDialog::SetFileTypes"
        );
        assert_eq!(
            slot(offset_of!(IFileOpenDialogVtbl, set_options)),
            9,
            "SetOptions"
        );
        assert_eq!(
            slot(offset_of!(IFileOpenDialogVtbl, get_options)),
            10,
            "GetOptions"
        );
        assert_eq!(
            slot(offset_of!(IFileOpenDialogVtbl, set_title)),
            17,
            "SetTitle"
        );
        assert_eq!(
            slot(offset_of!(IFileOpenDialogVtbl, set_ok_button_label)),
            18,
            "SetOkButtonLabel"
        );
        assert_eq!(
            slot(offset_of!(IFileOpenDialogVtbl, get_result)),
            20,
            "GetResult is LAST"
        );
        assert_eq!(
            slot(offset_of!(IShellItemVtbl, release)),
            2,
            "IUnknown::Release"
        );
        assert_eq!(
            slot(offset_of!(IShellItemVtbl, get_display_name)),
            5,
            "IShellItem::GetDisplayName is LAST"
        );
    }

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
        let back: Vec<u16> =
            std::os::windows::ffi::OsStrExt::encode_wide(path.as_os_str()).collect();
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
}
