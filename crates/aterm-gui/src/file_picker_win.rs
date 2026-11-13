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

/// `IShellItem` (shobjidl_core.h). Only `GetDisplayName` is called; the five
/// slots before it keep the documented method order so it lands on slot 5.
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

const _: () = {
    use core::mem::{offset_of, size_of};
    const W: usize = size_of::<usize>();
    assert!(size_of::<IFileOpenDialogVtbl>() == 21 * W);
    assert!(size_of::<IShellItemVtbl>() == 6 * W);
    assert!(offset_of!(IFileOpenDialogVtbl, release) == 2 * W);
    assert!(offset_of!(IFileOpenDialogVtbl, show) == 3 * W);
    assert!(offset_of!(IFileOpenDialogVtbl, set_file_types) == 4 * W);
    assert!(offset_of!(IFileOpenDialogVtbl, set_options) == 9 * W);
    assert!(offset_of!(IFileOpenDialogVtbl, get_options) == 10 * W);
    assert!(offset_of!(IFileOpenDialogVtbl, set_title) == 17 * W);
    assert!(offset_of!(IFileOpenDialogVtbl, set_ok_button_label) == 18 * W);
    assert!(offset_of!(IFileOpenDialogVtbl, get_result) == 20 * W);
    assert!(offset_of!(IShellItemVtbl, release) == 2 * W);
    assert!(offset_of!(IShellItemVtbl, get_display_name) == 5 * W);
};

/// `HRESULT_FROM_WIN32(ERROR_CANCELLED)`: what `IModalWindow::Show` returns when the user
/// dismisses the dialog.
const HRESULT_CANCELLED: i32 = 0x8007_04C7u32 as i32;
/// `SIGDN_FILESYSPATH`: the display-name form that is a filesystem path, which succeeds for
/// every item under [`FOS_FORCEFILESYSTEM`].
const SIGDN_FILESYSPATH: u32 = 0x8005_8000;

/// Directories, not files, are the selectable items. Cleared:
/// `setCanChooseDirectories(false)`.
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

/// The document picker's file-type list. ONE unrestricted row, because the
/// macOS document panel restricts nothing either: the Markdown/editor document
/// host validates what it was handed, and a type filter here would only be a
/// second, drifting copy of that rule that silently hides files it would have
/// accepted.
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
    /// Alive for the pointers in `specs`. Moving the outer `Vec` does not move
    /// these heap buffers, so the pointers stay valid across the move into `Self`.
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
#[path = "file_picker_win_tests.rs"]
mod tests;
