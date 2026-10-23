// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Windows taskbar jump list: [`task`] and [`crate::jumplist_recent`] rows targeting
//! [`front_door_exe`], gated by [`recents_tracked`] and [`owns_the_jump_list`].
#![cfg(windows)]

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;

use crate::win_ffi::{
    COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize, Com, Guid, HKEY_CURRENT_USER,
    StepError, check, co_create, wide,
};

/// `CLSID_DestinationList` {77F10CF0-3DB5-4966-B520-B7C54FD35ED6}.
const CLSID_DESTINATION_LIST: Guid = Guid {
    data1: 0x77F1_0CF0,
    data2: 0x3DB5,
    data3: 0x4966,
    data4: [0xB5, 0x20, 0xB7, 0xC5, 0x4F, 0xD3, 0x5E, 0xD6],
};
/// `IID_ICustomDestinationList` {6332DEBF-87B5-4670-90C0-5E57B408A49E}.
const IID_ICUSTOM_DESTINATION_LIST: Guid = Guid {
    data1: 0x6332_DEBF,
    data2: 0x87B5,
    data3: 0x4670,
    data4: [0x90, 0xC0, 0x5E, 0x57, 0xB4, 0x08, 0xA4, 0x9E],
};
/// `CLSID_EnumerableObjectCollection` {2D3468C1-36A7-43B6-AC24-D3F02FD9607A}.
const CLSID_ENUMERABLE_OBJECT_COLLECTION: Guid = Guid {
    data1: 0x2D34_68C1,
    data2: 0x36A7,
    data3: 0x43B6,
    data4: [0xAC, 0x24, 0xD3, 0xF0, 0x2F, 0xD9, 0x60, 0x7A],
};
/// `IID_IObjectCollection` {5632B1A4-E38A-400A-928A-D4CD63230295}.
const IID_IOBJECT_COLLECTION: Guid = Guid {
    data1: 0x5632_B1A4,
    data2: 0xE38A,
    data3: 0x400A,
    data4: [0x92, 0x8A, 0xD4, 0xCD, 0x63, 0x23, 0x02, 0x95],
};
/// `IID_IObjectArray` {92CA9DCD-5622-4BBA-A805-5E9F541BD8C9}: the riid for `BeginList`'s
/// removed-destinations out-param.
const IID_IOBJECT_ARRAY: Guid = Guid {
    data1: 0x92CA_9DCD,
    data2: 0x5622,
    data3: 0x4BBA,
    data4: [0xA8, 0x05, 0x5E, 0x9F, 0x54, 0x1B, 0xD8, 0xC9],
};
/// `CLSID_ShellLink` {00021401-0000-0000-C000-000000000046}.
const CLSID_SHELL_LINK: Guid = Guid {
    data1: 0x0002_1401,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};
/// `IID_IShellLinkW` {000214F9-0000-0000-C000-000000000046}.
const IID_ISHELL_LINK_W: Guid = Guid {
    data1: 0x0002_14F9,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};
/// `IID_IPropertyStore` {886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99}.
const IID_IPROPERTY_STORE: Guid = Guid {
    data1: 0x886D_8EEB,
    data2: 0x8CF2,
    data3: 0x4446,
    data4: [0x8D, 0x02, 0xCD, 0xBA, 0x1D, 0xBD, 0xCF, 0x99],
};

/// `PROPERTYKEY` (propsys.h): a fmtid GUID plus a property id.
#[repr(C)]
struct PropertyKey {
    fmtid: Guid,
    pid: u32,
}

/// `PKEY_Title` ({F29F85E0-4FF9-1068-AB91-08002B27B3D9}, 2): the text the jump-list row
/// displays.
const PKEY_TITLE: PropertyKey = PropertyKey {
    fmtid: Guid {
        data1: 0xF29F_85E0,
        data2: 0x4FF9,
        data3: 0x1068,
        data4: [0xAB, 0x91, 0x08, 0x00, 0x2B, 0x27, 0xB3, 0xD9],
    },
    pid: 2,
};

const VT_LPWSTR: u16 = 31;

/// A shape-only `PROPVARIANT`: 24 bytes on x64 (8-byte header, 16-byte union); the callee
/// never reads past its own `sizeof`, so the over-size on x86 is harmless.
#[repr(C)]
struct PropVariant {
    vt: u16,
    reserved1: u16,
    reserved2: u16,
    reserved3: u16,
    ptr: *const u16,
    pad: [u64; 1],
}

/// `ICustomDestinationList` (shobjidl_core.h).
#[repr(C)]
struct ICustomDestinationListVtbl {
    query_interface: unsafe extern "system" fn(
        *mut ICustomDestinationList,
        *const Guid,
        *mut *mut c_void,
    ) -> i32,
    add_ref: unsafe extern "system" fn(*mut ICustomDestinationList) -> u32,
    release: unsafe extern "system" fn(*mut ICustomDestinationList) -> u32,
    set_app_id: unsafe extern "system" fn(*mut ICustomDestinationList, *const u16) -> i32,
    begin_list: unsafe extern "system" fn(
        *mut ICustomDestinationList,
        *mut u32,
        *const Guid,
        *mut *mut c_void,
    ) -> i32,
    append_category:
        unsafe extern "system" fn(*mut ICustomDestinationList, *const u16, *mut c_void) -> i32,
    append_known_category: unsafe extern "system" fn(*mut ICustomDestinationList, i32) -> i32,
    add_user_tasks: unsafe extern "system" fn(*mut ICustomDestinationList, *mut c_void) -> i32,
    commit_list: unsafe extern "system" fn(*mut ICustomDestinationList) -> i32,
    get_removed_destinations: unsafe extern "system" fn(
        *mut ICustomDestinationList,
        *const Guid,
        *mut *mut c_void,
    ) -> i32,
    delete_list: unsafe extern "system" fn(*mut ICustomDestinationList, *const u16) -> i32,
    abort_list: unsafe extern "system" fn(*mut ICustomDestinationList) -> i32,
}

#[repr(C)]
struct ICustomDestinationList {
    vtbl: *const ICustomDestinationListVtbl,
}

/// `IObjectCollection` (IUnknown → IObjectArray → IObjectCollection).
#[repr(C)]
struct IObjectCollectionVtbl {
    query_interface:
        unsafe extern "system" fn(*mut IObjectCollection, *const Guid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut IObjectCollection) -> u32,
    release: unsafe extern "system" fn(*mut IObjectCollection) -> u32,
    get_count: unsafe extern "system" fn(*mut IObjectCollection, *mut u32) -> i32,
    get_at: unsafe extern "system" fn(
        *mut IObjectCollection,
        u32,
        *const Guid,
        *mut *mut c_void,
    ) -> i32,
    add_object: unsafe extern "system" fn(*mut IObjectCollection, *mut c_void) -> i32,
    add_from_array: unsafe extern "system" fn(*mut IObjectCollection, *mut c_void) -> i32,
    remove_object_at: unsafe extern "system" fn(*mut IObjectCollection, u32) -> i32,
    clear: unsafe extern "system" fn(*mut IObjectCollection) -> i32,
}

#[repr(C)]
struct IObjectCollection {
    vtbl: *const IObjectCollectionVtbl,
}

/// `IShellLinkW` (shobjidl_core.h). The setters a row needs and
/// `GetWorkingDirectory` (a removed row's directory) are called; every slot
/// keeps the documented method order so those land on the right vtable entries
/// (checked against `windows` 0.62's `IShellLinkW_Vtbl`).
#[repr(C)]
struct IShellLinkWVtbl {
    query_interface:
        unsafe extern "system" fn(*mut IShellLinkW, *const Guid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut IShellLinkW) -> u32,
    release: unsafe extern "system" fn(*mut IShellLinkW) -> u32,
    get_path: unsafe extern "system" fn(*mut IShellLinkW, *mut u16, i32, *mut c_void, u32) -> i32,
    get_id_list: unsafe extern "system" fn(*mut IShellLinkW, *mut *mut c_void) -> i32,
    set_id_list: unsafe extern "system" fn(*mut IShellLinkW, *const c_void) -> i32,
    get_description: unsafe extern "system" fn(*mut IShellLinkW, *mut u16, i32) -> i32,
    set_description: unsafe extern "system" fn(*mut IShellLinkW, *const u16) -> i32,
    get_working_directory: unsafe extern "system" fn(*mut IShellLinkW, *mut u16, i32) -> i32,
    set_working_directory: unsafe extern "system" fn(*mut IShellLinkW, *const u16) -> i32,
    get_arguments: unsafe extern "system" fn(*mut IShellLinkW, *mut u16, i32) -> i32,
    set_arguments: unsafe extern "system" fn(*mut IShellLinkW, *const u16) -> i32,
    get_hotkey: unsafe extern "system" fn(*mut IShellLinkW, *mut u16) -> i32,
    set_hotkey: unsafe extern "system" fn(*mut IShellLinkW, u16) -> i32,
    get_show_cmd: unsafe extern "system" fn(*mut IShellLinkW, *mut i32) -> i32,
    set_show_cmd: unsafe extern "system" fn(*mut IShellLinkW, i32) -> i32,
    get_icon_location: unsafe extern "system" fn(*mut IShellLinkW, *mut u16, i32, *mut i32) -> i32,
    set_icon_location: unsafe extern "system" fn(*mut IShellLinkW, *const u16, i32) -> i32,
    set_relative_path: unsafe extern "system" fn(*mut IShellLinkW, *const u16, u32) -> i32,
    resolve: unsafe extern "system" fn(*mut IShellLinkW, isize, u32) -> i32,
    set_path: unsafe extern "system" fn(*mut IShellLinkW, *const u16) -> i32,
}

#[repr(C)]
struct IShellLinkW {
    vtbl: *const IShellLinkWVtbl,
}

/// `IPropertyStore` (propsys.h).
#[repr(C)]
struct IPropertyStoreVtbl {
    query_interface:
        unsafe extern "system" fn(*mut IPropertyStore, *const Guid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut IPropertyStore) -> u32,
    release: unsafe extern "system" fn(*mut IPropertyStore) -> u32,
    get_count: unsafe extern "system" fn(*mut IPropertyStore, *mut u32) -> i32,
    get_at: unsafe extern "system" fn(*mut IPropertyStore, u32, *mut PropertyKey) -> i32,
    get_value:
        unsafe extern "system" fn(*mut IPropertyStore, *const PropertyKey, *mut PropVariant) -> i32,
    set_value: unsafe extern "system" fn(
        *mut IPropertyStore,
        *const PropertyKey,
        *const PropVariant,
    ) -> i32,
    commit: unsafe extern "system" fn(*mut IPropertyStore) -> i32,
}

#[repr(C)]
struct IPropertyStore {
    vtbl: *const IPropertyStoreVtbl,
}

/// Balances a successful `CoInitializeEx` when [`build`] returns by any path.
struct CoUninitGuard;

impl Drop for CoUninitGuard {
    fn drop(&mut self) {
        // SAFETY: Constructed only after `CoInitializeEx` succeeded on this thread.
        unsafe { CoUninitialize() };
    }
}

/// Register (or refresh) the jump list for this process's AUMID.
pub(crate) fn install() {
    let private = aterm_types::dirs::runs_under_private_state_root();
    if !owns_the_jump_list(crate::win32::running_image(), private) {
        aterm_log::debug!(
            "taskbar jump list skipped ({})",
            if private {
                "private state root"
            } else {
                "not the windowed image"
            }
        );
        return;
    }
    let current = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            aterm_log::warn!("taskbar jump list skipped (current_exe: {error})");
            return;
        }
    };
    let exe = front_door_exe(
        &current,
        std::path::Path::is_file,
        crate::win32::in_cargo_profile_dir(&current),
    );
    let exe_w: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let attach = attach_policy_is_on();
    let Some(notes) = crate::jumplist_recent::take_notes() else {
        // a second install in one process: the first one's worker serves
        return; // a second install in one process: the first one's worker serves
    };
    let mut worker = Worker {
        exe: exe_w,
        attach,
        file: aterm_types::dirs::state_dir().map(|dir| dir.join(crate::jumplist_recent::FILE_NAME)),
        committed: None,
        warned: false,
        denied: false,
        tracked_before: false,
    };
    if !worker.refresh(&[]) {
        crate::jumplist_recent::stop_recording();
        return;
    }
    while let Ok(mut batch) = notes.recv() {
        while let Ok(more) = notes.try_recv() {
            batch.extend(more);
        }
        worker.refresh(&batch);
    }
}

/// Whether this process may commit the owner's jump list: a window of the
/// WINDOWED image ([`install`] says why), and never a private window — a
/// development build under its own `ATERM_STATE_HOME`, which stands in for
/// nobody (`aterm_types::dirs::runs_under_private_state_root`).
fn owns_the_jump_list(image: crate::ImageSubsystem, private_state_root: bool) -> bool {
    image == crate::ImageSubsystem::Windowed && !private_state_root
}

/// The jump list's worker state (its own thread; see [`install`]).
struct Worker {
    /// The front door every row launches, NUL-terminated ([`front_door_exe`]).
    exe: Vec<u16>,
    /// Whether the "New Tab" task is offered ([`attach_policy_is_on`]).
    attach: bool,
    /// `<aterm state dir>\jumplist-recent.txt` (`%LOCALAPPDATA%\aterm`).
    file: Option<std::path::PathBuf>,
    /// The Recent rows the last commit asked for, less the removed ones (`None` before the first).
    committed: Option<Vec<String>>,
    /// One warn line per process for a failing commit, not one per refresh.
    warned: bool,
    /// The shell refused the Recent category with `E_ACCESSDENIED`.
    denied: bool,
    /// [`recents_tracked`] at the previous refresh.
    tracked_before: bool,
}

impl Worker {
    /// Fold `notes` (directories, oldest first) into the file and commit the
    /// list if the rows it shows changed.
    fn refresh(&mut self, notes: &[String]) -> bool {
        let (exe, attach) = (self.exe.clone(), self.attach);
        self.refresh_with(
            notes,
            recents_tracked(),
            &|dir| std::path::Path::new(dir).is_dir(),
            &mut |rows| {
                // SAFETY: `build` is one linear pass of documented shell COM calls.
                unsafe { build(crate::win32::AUMID, &exe, attach, rows) }
            },
        )
    }

    /// [`Self::refresh`] with its outside facts injected, so the tests can
    /// drive it: whether Windows keeps recent items ([`recents_tracked`]),
    /// whether a directory exists, and the commit ([`build`]).
    fn refresh_with(
        &mut self,
        notes: &[String],
        tracked: bool,
        is_dir: &dyn Fn(&str) -> bool,
        commit: &mut dyn FnMut(&[String]) -> Result<Built, StepError>,
    ) -> bool {
        use crate::jumplist_recent::{KEEP, SHOWN, merge, parse, render, shown, without};
        let tracked_now = tracked && self.file.is_some();
        if tracked_now && !self.tracked_before {
            self.denied = false;
        }
        self.tracked_before = tracked_now;
        let tracked = tracked_now && !self.denied;
        let read = self
            .file
            .as_deref()
            .and_then(|file| std::fs::read_to_string(file).ok());
        let mut list = match (&read, tracked) {
            (Some(text), true) => parse(text),
            _ => Vec::new(),
        };
        if tracked {
            list = merge(&list, notes, KEEP);
        }
        let rows = shown(&list, is_dir, SHOWN);
        if self.committed.as_ref() != Some(&rows) {
            match commit(&rows) {
                Ok(built) => {
                    list = without(&list, &built.removed);
                    let refused = built.refused;
                    aterm_log::debug!(
                        "taskbar jump list committed (tasks: {}; recent: {} row(s){})",
                        if self.attach {
                            "New Tab, New Window"
                        } else {
                            "New Window"
                        },
                        built.shown.len(),
                        refused.map_or(String::new(), |hr| format!(
                            ", category refused hr={hr:#010x}"
                        )),
                    );
                    self.committed = Some(if refused.is_some() {
                        Vec::new()
                    } else {
                        without(&rows, &built.removed)
                    });
                    if refused == Some(E_ACCESSDENIED) {
                        self.denied = true;
                        list.clear();
                    }
                }
                Err((step, hr)) => {
                    if !self.warned {
                        aterm_log::warn!("taskbar jump list skipped ({step}: hr={hr:#010x})");
                        self.warned = true;
                    }
                    if self.committed.is_none() {
                        return false;
                    }
                }
            }
        }
        if let Some(file) = &self.file {
            if list.is_empty() {
                // Nothing kept (the switch is off, or nothing ran yet): no file.
                if read.is_some() {
                    let _ = std::fs::remove_file(file);
                }
            } else {
                let text = render(&list);
                if read.as_deref() != Some(text.as_str()) {
                    write_replacing(file, &text);
                }
            }
        }
        true
    }
}

/// `E_ACCESSDENIED`: what `AppendCategory` answers while the owner has turned
/// recent items off (the shell's half of [`recents_tracked`]).
const E_ACCESSDENIED: i32 = 0x8007_0005_u32 as i32;

/// Write `text` to `file` through a sibling and a rename, so a reader (another
/// aterm's worker) sees the old list or the new one, never half of one.
fn write_replacing(file: &std::path::Path, text: &str) {
    let Some(dir) = file.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let sibling = file.with_extension(format!("txt.{}.tmp", std::process::id()));
    if std::fs::write(&sibling, text).is_ok() && std::fs::rename(&sibling, file).is_err() {
        let _ = std::fs::remove_file(&sibling);
    }
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(
        key: isize,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        ty: *mut u32,
        data: *mut c_void,
        cb: *mut u32,
    ) -> i32;
}

/// Spelled as `win_ffi::HKEY_CURRENT_USER` is (the registry recognizes a
/// predefined key by its low 32 bits).
const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as isize;
/// `RRF_RT_REG_DWORD`: a value of another type reads as absent.
const RRF_RT_REG_DWORD: u32 = 0x0000_0010;

/// One `REG_DWORD`, or `None` when absent, of another type or unreadable.
fn reg_dword(root: isize, subkey: &str, value: &str) -> Option<u32> {
    let subkey = wide(subkey);
    let value = wide(value);
    let mut data: u32 = 0;
    let mut cb: u32 = core::mem::size_of::<u32>() as u32;
    // SAFETY: One documented flat-C registry read.
    let rc = unsafe {
        RegGetValueW(
            root,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast::<c_void>(),
            &mut cb,
        )
    };
    (rc == 0).then_some(data)
}

/// Whether Windows keeps recent items for this user: Settings ▸
/// Personalization ▸ Start ▸ "Show recently opened items in Start, Jump Lists
/// and File Explorer" (`Explorer\Advanced\Start_TrackDocs`, absent = on) and
/// the `NoRecentDocsHistory` policy (user or machine, absent = allowed).
fn recents_tracked() -> bool {
    const ADVANCED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced";
    const POLICY: &str = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";
    let switch_on = reg_dword(HKEY_CURRENT_USER, ADVANCED, "Start_TrackDocs") != Some(0);
    let forbidden = [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
        .into_iter()
        .any(|root| reg_dword(root, POLICY, "NoRecentDocsHistory").is_some_and(|v| v != 0));
    switch_on && !forbidden
}

/// The executable a jump-list row launches: the windowed front door, an image that routes
/// `new-tab` / `new-window` / `split-pane` and that the shell starts without a console.
fn front_door_exe(
    current: &std::path::Path,
    is_file: impl Fn(&std::path::Path) -> bool,
    cargo_profile_dir: bool,
) -> std::path::PathBuf {
    if let Some(windowed) = crate::windowed_front_door(current, &is_file, cargo_profile_dir) {
        return windowed;
    }
    current
        .parent()
        .map(|dir| dir.join("aterm.exe"))
        .filter(|console| is_file(console))
        .unwrap_or_else(|| current.to_path_buf())
}

/// Whether `windowing_behavior` is `attach`, i.e. whether a "New Tab" row would produce a tab.
fn attach_policy_is_on() -> bool {
    crate::windowing_behavior_setting().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "attach" | "use_existing" | "use-existing" | "useexisting" | "existing"
        )
    })
}

/// What one commit did.
struct Built {
    /// The Recent rows the committed list shows.
    shown: Vec<String>,
    /// The directories of rows the owner removed since the previous commit.
    removed: Vec<String>,
    /// `AppendCategory`'s HRESULT when the shell refused the Recent category.
    refused: Option<i32>,
}

/// Commit the jump list for `aumid`: a Recent category of `recents` (newest
/// first, less any row the owner removed, cut to the shell's slot count), then
/// the tasks.
unsafe fn build(
    aumid: &str,
    exe: &[u16],
    attach: bool,
    recents: &[String],
) -> Result<Built, StepError> {
    let mut removed = Vec::new();
    match unsafe { build_once(aumid, exe, attach, recents, &mut removed) } {
        Ok(shown) => Ok(Built {
            shown,
            removed,
            refused: None,
        }),
        Err(("AppendCategory", hr)) => {
            unsafe { build_once(aumid, exe, attach, &[], &mut removed) }?;
            Ok(Built {
                shown: Vec::new(),
                removed,
                refused: Some(hr),
            })
        }
        Err(error) => Err(error),
    }
}

/// One `BeginList` … `CommitList` transaction ([`build`]); appends the removed
/// rows' directories to `removed` and returns the Recent rows it committed.
unsafe fn build_once(
    aumid: &str,
    exe: &[u16],
    attach: bool,
    recents: &[String],
    removed: &mut Vec<String>,
) -> Result<Vec<String>, StepError> {
    let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    check(hr, "CoInitializeEx")?;
    let _co = CoUninitGuard;

    let list = co_create(
        &CLSID_DESTINATION_LIST,
        &IID_ICUSTOM_DESTINATION_LIST,
        "create DestinationList",
    )?;
    let list_ptr = list.as_raw().cast::<ICustomDestinationList>();
    let list_vt = unsafe { &*(*list_ptr).vtbl };

    let aumid = wide(aumid);
    check(
        unsafe { (list_vt.set_app_id)(list_ptr, aumid.as_ptr()) },
        "SetAppID",
    )?;

    let mut min_slots = 0u32;
    let mut removed_raw: *mut c_void = std::ptr::null_mut();
    check(
        unsafe {
            (list_vt.begin_list)(
                list_ptr,
                &mut min_slots,
                &IID_IOBJECT_ARRAY,
                &mut removed_raw,
            )
        },
        "BeginList",
    )?;
    // SAFETY: A successful BeginList hands over one reference to the array.
    if let Some(array) = unsafe { Com::from_raw(removed_raw) } {
        removed.extend(unsafe { removed_dirs(&array) });
    }

    let rows: Vec<String> = recents
        .iter()
        .filter(|dir| {
            !removed
                .iter()
                .any(|gone| crate::jumplist_recent::same_dir(gone, dir))
        })
        .take(usize::try_from(min_slots).unwrap_or(usize::MAX))
        .cloned()
        .collect();
    if !rows.is_empty() {
        let category = co_create(
            &CLSID_ENUMERABLE_OBJECT_COLLECTION,
            &IID_IOBJECT_COLLECTION,
            "create ObjectCollection",
        )?;
        let category_ptr = category.as_raw().cast::<IObjectCollection>();
        for (dir, title) in rows.iter().zip(crate::jumplist_recent::titles(&rows)) {
            let row = unsafe { recent_row(exe, dir, &title)? };
            check(
                unsafe { ((*(*category_ptr).vtbl).add_object)(category_ptr, row.as_raw()) },
                "AddObject",
            )?;
        }
        let name = wide("Recent");
        check(
            unsafe { (list_vt.append_category)(list_ptr, name.as_ptr(), category.as_raw()) },
            "AppendCategory",
        )?;
    }

    let tasks = co_create(
        &CLSID_ENUMERABLE_OBJECT_COLLECTION,
        &IID_IOBJECT_COLLECTION,
        "create ObjectCollection",
    )?;
    let tasks_ptr = tasks.as_raw().cast::<IObjectCollection>();

    if attach {
        let new_tab = unsafe {
            task(
                exe,
                "new-tab",
                "New Tab",
                "Open a new tab in the running aterm",
            )?
        };
        check(
            unsafe { ((*(*tasks_ptr).vtbl).add_object)(tasks_ptr, new_tab.as_raw()) },
            "AddObject",
        )?;
    }
    let new_window = unsafe { task(exe, "new-window", "New Window", "Open a new aterm window")? };
    check(
        unsafe { ((*(*tasks_ptr).vtbl).add_object)(tasks_ptr, new_window.as_raw()) },
        "AddObject",
    )?;

    check(
        unsafe { (list_vt.add_user_tasks)(list_ptr, tasks.as_raw()) },
        "AddUserTasks",
    )?;
    check(unsafe { (list_vt.commit_list)(list_ptr) }, "CommitList")?;
    Ok(rows)
}

/// Remove `aumid`'s committed jump list (`ICustomDestinationList::DeleteList`):
/// the live read-back's clean-up of its test identity.
#[cfg(debug_assertions)]
unsafe fn delete_list(aumid: &str) -> Result<(), StepError> {
    let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    check(hr, "CoInitializeEx")?;
    let _co = CoUninitGuard;
    let list = co_create(
        &CLSID_DESTINATION_LIST,
        &IID_ICUSTOM_DESTINATION_LIST,
        "create DestinationList",
    )?;
    let list_ptr = list.as_raw().cast::<ICustomDestinationList>();
    let aumid = wide(aumid);
    check(
        unsafe { ((*(*list_ptr).vtbl).delete_list)(list_ptr, aumid.as_ptr()) },
        "DeleteList",
    )
}

/// The directories of the rows in `removed` (`BeginList`'s removed
/// destinations): each `IShellLinkW`'s working directory, which [`recent_row`]
/// sets to the row's directory.
unsafe fn removed_dirs(removed: &Com) -> Vec<String> {
    let array = removed.as_raw().cast::<IObjectCollection>();
    let array_vt = unsafe { &*(*array).vtbl };
    let mut count = 0u32;
    if unsafe { (array_vt.get_count)(array, &mut count) } < 0 {
        return Vec::new();
    }
    let mut dirs = Vec::new();
    for at in 0..count {
        let mut raw: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { (array_vt.get_at)(array, at, &IID_ISHELL_LINK_W, &mut raw) };
        if hr < 0 {
            continue;
        }
        // SAFETY: A successful GetAt hands over one reference to the item.
        let Some(link) = (unsafe { Com::from_raw(raw) }) else {
            continue;
        };
        let link_ptr = link.as_raw().cast::<IShellLinkW>();
        let mut buf = vec![0u16; 2050];
        let len = i32::try_from(buf.len()).unwrap_or(i32::MAX);
        let hr =
            unsafe { ((*(*link_ptr).vtbl).get_working_directory)(link_ptr, buf.as_mut_ptr(), len) };
        if hr < 0 {
            continue;
        }
        let end = buf.iter().position(|&unit| unit == 0).unwrap_or(buf.len());
        if let Some(dir) = crate::jumplist_recent::normalize(&String::from_utf16_lossy(&buf[..end]))
        {
            dirs.push(dir);
        }
    }
    dirs
}

/// The icon Explorer gives a directory (`HKCR\Directory\DefaultIcon` is
/// `%SystemRoot%\System32\shell32.dll,3`), NUL-terminated; `None` when that DLL
/// is not where Windows keeps it.
fn folder_icon() -> Option<Vec<u16>> {
    let dll = std::path::PathBuf::from(std::env::var_os("SystemRoot")?)
        .join("System32")
        .join("shell32.dll");
    dll.is_file().then(|| {
        dll.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    })
}

/// [`folder_icon`]'s index in shell32.dll: the closed folder.
const FOLDER_ICON_INDEX: i32 = 3;

/// One Recent row: `exe new-window -d "<dir>"` (the window verb, as the "New
/// Window" task uses — [`crate::jumplist_recent::row_arguments`]), started in
/// `dir`, with the folder icon, `title` as its text and the full path as its
/// tooltip.
unsafe fn recent_row(exe: &[u16], dir: &str, title: &str) -> Result<Com, StepError> {
    let folder = folder_icon();
    let icon = folder
        .as_deref()
        .map_or((exe, 0), |dll| (dll, FOLDER_ICON_INDEX));
    unsafe {
        link(
            exe,
            &crate::jumplist_recent::row_arguments(dir),
            title,
            dir,
            icon,
            Some(dir),
        )
    }
}

/// One task entry: an in-memory `IShellLinkW` running `exe` ([`front_door_exe`]) with `args`
/// (a front-door verb), with the exe's own icon (index 0) and its row text set through the
/// link's property store ([`PKEY_TITLE`]).
unsafe fn task(exe: &[u16], args: &str, title: &str, tooltip: &str) -> Result<Com, StepError> {
    unsafe { link(exe, args, title, tooltip, (exe, 0), None) }
}

/// The one `IShellLinkW` builder behind [`task`] and [`recent_row`]: `exe`
/// run with `args`, `icon` (a NUL-terminated file and its index), `tooltip`
/// as the hover text, `title` as the row text, and `working_dir` when given.
unsafe fn link(
    exe: &[u16],
    args: &str,
    title: &str,
    tooltip: &str,
    icon: (&[u16], i32),
    working_dir: Option<&str>,
) -> Result<Com, StepError> {
    let link = co_create(&CLSID_SHELL_LINK, &IID_ISHELL_LINK_W, "create ShellLink")?;
    let link_ptr = link.as_raw().cast::<IShellLinkW>();
    let link_vt = unsafe { &*(*link_ptr).vtbl };

    check(
        unsafe { (link_vt.set_path)(link_ptr, exe.as_ptr()) },
        "SetPath",
    )?;
    let args_w = wide(args);
    check(
        unsafe { (link_vt.set_arguments)(link_ptr, args_w.as_ptr()) },
        "SetArguments",
    )?;
    if let Some(dir) = working_dir {
        let dir_w = wide(dir);
        check(
            unsafe { (link_vt.set_working_directory)(link_ptr, dir_w.as_ptr()) },
            "SetWorkingDirectory",
        )?;
    }
    check(
        unsafe { (link_vt.set_icon_location)(link_ptr, icon.0.as_ptr(), icon.1) },
        "SetIconLocation",
    )?;
    let tip = wide(tooltip);
    check(
        unsafe { (link_vt.set_description)(link_ptr, tip.as_ptr()) },
        "SetDescription",
    )?;

    let store = link
        .query(&IID_IPROPERTY_STORE)
        .map_err(|hr| ("QueryInterface IPropertyStore", hr))?;
    let store_ptr = store.as_raw().cast::<IPropertyStore>();
    let store_vt = unsafe { &*(*store_ptr).vtbl };

    let title_w = wide(title);
    let variant = PropVariant {
        vt: VT_LPWSTR,
        reserved1: 0,
        reserved2: 0,
        reserved3: 0,
        ptr: title_w.as_ptr(),
        pad: [0],
    };
    check(
        unsafe { (store_vt.set_value)(store_ptr, &PKEY_TITLE, &variant) },
        "SetValue PKEY_Title",
    )?;
    check(
        unsafe { (store_vt.commit)(store_ptr) },
        "IPropertyStore::Commit",
    )?;
    Ok(link)
}

#[cfg(test)]
mod tests {
    use super::front_door_exe;
    use std::path::{Path, PathBuf};

    fn install_dir() -> PathBuf {
        PathBuf::from(r"C:\Users\me\AppData\Local\Programs\aterm")
    }

    fn exe(name: &str) -> String {
        format!("{name}{}", std::env::consts::EXE_SUFFIX)
    }

    /// The shipped layout: whichever image commits the list, every row names the windowed
    /// `aterm-gui.exe` (a row naming the console image flashes a console on every click).
    #[test]
    fn a_row_names_the_windowed_image_whoever_commits_the_list() {
        let dir = install_dir();
        let both =
            |path: &Path| path == dir.join(exe("aterm")) || path == dir.join(exe("aterm-gui"));
        for running in [
            "aterm-gui",
            "aterm",
            "aterm-ctl",
            "aterm-cli",
            "atpkg",
            "aterm-fleet",
        ] {
            let current = dir.join(exe(running));
            assert_eq!(
                front_door_exe(&current, both, false),
                dir.join(exe("aterm-gui")),
                "a list committed by {running} must launch the windowed front door"
            );
        }
    }

    /// A build tree: the row names `aterm-windowed.exe`, never the thin dev `aterm-gui.exe`
    /// (no router: `new-window` is `unknown command` there); without it the console
    /// `aterm.exe` still routes the verb.
    #[test]
    fn a_build_tree_row_skips_the_thin_dev_bin() {
        let dir = PathBuf::from(r"C:\src\aterm\target\debug");
        let thin = dir.join(exe("aterm-gui"));
        let windowed = dir.join(exe("aterm-windowed"));
        let console = dir.join(exe("aterm"));
        let everything = |path: &Path| path == thin || path == windowed || path == console;
        assert_eq!(front_door_exe(&thin, everything, true), windowed);
        let no_windowed = |path: &Path| path == thin || path == console;
        assert_eq!(front_door_exe(&thin, no_windowed, true), console);
    }

    /// No front door on disk: fall back to this process, never a row naming a missing file.
    #[test]
    fn a_lone_copy_falls_back_to_itself() {
        let current = Path::new(r"C:\tmp\scratch").join(exe("aterm-ctl"));
        assert_eq!(front_door_exe(&current, |_| false, false), current);
    }

    /// Only the windowed image commits the owner's list, and a private window
    /// (its own `ATERM_STATE_HOME`) never does: its rows would launch a scratch
    /// build and its empty Recent file would empty the owner's Recent rows.
    #[test]
    fn a_private_window_and_the_console_image_leave_the_list_alone() {
        use super::owns_the_jump_list;
        use crate::ImageSubsystem::{NotWindowed, Windowed};
        assert!(owns_the_jump_list(Windowed, false));
        assert!(!owns_the_jump_list(Windowed, true), "a private window");
        assert!(!owns_the_jump_list(NotWindowed, false), "the console image");
        assert!(!owns_the_jump_list(NotWindowed, true));
    }

    use super::{Built, E_ACCESSDENIED, StepError, Worker};
    use crate::jumplist_recent::{parse, render};
    use std::cell::RefCell;

    fn rows(list: &[&str]) -> Vec<String> {
        list.iter().map(|d| (*d).to_owned()).collect()
    }

    /// A worker over a scratch file; the tests hand it their own commit.
    fn scratch_worker(file: &Path) -> Worker {
        Worker {
            exe: Vec::new(),
            attach: false,
            file: Some(file.to_path_buf()),
            committed: None,
            warned: false,
            denied: false,
            tracked_before: false,
        }
    }

    fn kept(file: &Path) -> Vec<String> {
        parse(&std::fs::read_to_string(file).unwrap_or_default())
    }

    /// Notes reach the file newest first; the list is committed only when the
    /// rows it would show change; a row the owner removed leaves the file; a
    /// shell that takes no list at all stops the worker.
    #[test]
    fn the_worker_commits_only_when_the_rows_change() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let file = scratch.path().join(crate::jumplist_recent::FILE_NAME);
        let mut worker = scratch_worker(&file);
        let commits = RefCell::new(Vec::<Vec<String>>::new());
        let removed = RefCell::new(Vec::<String>::new());
        let mut commit = |shown: &[String]| -> Result<Built, StepError> {
            commits.borrow_mut().push(shown.to_vec());
            let gone = std::mem::take(&mut *removed.borrow_mut());
            Ok(Built {
                shown: shown
                    .iter()
                    .filter(|r| !gone.contains(r))
                    .cloned()
                    .collect(),
                removed: gone,
                refused: None,
            })
        };
        let exists = |_: &str| true;
        assert!(worker.refresh_with(&[], true, &exists, &mut commit));
        assert_eq!(commits.borrow().as_slice(), [Vec::<String>::new()]);
        assert!(!file.exists());
        worker.refresh_with(&rows(&[r"C:\a"]), true, &exists, &mut commit);
        assert_eq!(commits.borrow().last(), Some(&rows(&[r"C:\a"])));
        assert_eq!(kept(&file), rows(&[r"C:\a"]));
        worker.refresh_with(&rows(&[r"c:\A"]), true, &exists, &mut commit);
        assert_eq!(commits.borrow().len(), 2);
        worker.refresh_with(&rows(&[r"C:\b"]), true, &exists, &mut commit);
        assert_eq!(commits.borrow().last(), Some(&rows(&[r"C:\b", r"C:\a"])));
        *removed.borrow_mut() = rows(&[r"C:\b"]);
        worker.refresh_with(&rows(&[r"C:\c"]), true, &exists, &mut commit);
        assert_eq!(kept(&file), rows(&[r"C:\c", r"C:\a"]));
        worker.refresh_with(&[], true, &exists, &mut commit);
        assert_eq!(commits.borrow().len(), 4, "the rows shown did not change");
        worker.refresh_with(&[], true, &|dir: &str| dir != r"C:\c", &mut commit);
        assert_eq!(commits.borrow().last(), Some(&rows(&[r"C:\a"])));
        assert_eq!(kept(&file), rows(&[r"C:\c", r"C:\a"]));

        let mut failing = |_: &[String]| -> Result<Built, StepError> { Err(("CommitList", -1)) };
        let mut fresh = scratch_worker(&file);
        assert!(!fresh.refresh_with(&[], true, &exists, &mut failing));
    }

    /// A shell that shows fewer rows than asked (a short
    /// `JumpListItems_Maximum`) is not a change: the same ask commits once.
    #[test]
    fn the_shells_slot_cut_does_not_recommit_the_same_rows() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let file = scratch.path().join(crate::jumplist_recent::FILE_NAME);
        let mut worker = scratch_worker(&file);
        let commits = RefCell::new(0usize);
        let mut two_slots = |shown: &[String]| -> Result<Built, StepError> {
            *commits.borrow_mut() += 1;
            Ok(Built {
                shown: shown.iter().take(2).cloned().collect(),
                removed: Vec::new(),
                refused: None,
            })
        };
        let exists = |_: &str| true;
        let notes = rows(&[r"C:\a", r"C:\b", r"C:\c"]);
        worker.refresh_with(&notes, true, &exists, &mut two_slots);
        assert_eq!(*commits.borrow(), 1);
        worker.refresh_with(&rows(&[r"C:\c"]), true, &exists, &mut two_slots);
        worker.refresh_with(&[], true, &exists, &mut two_slots);
        assert_eq!(*commits.borrow(), 1, "the rows asked for did not change");
        worker.refresh_with(&rows(&[r"C:\a"]), true, &exists, &mut two_slots);
        assert_eq!(*commits.borrow(), 2, "C:\\a moved to the front");
    }

    /// With Windows' recent items off, the file goes and no Recent row is
    /// committed; the shell's own refusal (`E_ACCESSDENIED`) does the same, and
    /// holds until the switch is seen off and on again.
    #[test]
    fn recent_items_off_keeps_nothing() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let file = scratch.path().join(crate::jumplist_recent::FILE_NAME);
        std::fs::write(&file, render(&rows(&[r"C:\a"]))).expect("seed");
        let mut worker = scratch_worker(&file);
        let commits = RefCell::new(Vec::<Vec<String>>::new());
        let mut commit = |shown: &[String]| -> Result<Built, StepError> {
            commits.borrow_mut().push(shown.to_vec());
            Ok(Built {
                shown: shown.to_vec(),
                removed: Vec::new(),
                refused: None,
            })
        };
        let exists = |_: &str| true;
        worker.refresh_with(&rows(&[r"C:\b"]), false, &exists, &mut commit);
        assert!(!file.exists(), "nothing is kept while recent items are off");
        assert_eq!(commits.borrow().last(), Some(&Vec::new()));
        worker.refresh_with(&rows(&[r"C:\b"]), true, &exists, &mut commit);
        assert_eq!(kept(&file), rows(&[r"C:\b"]));

        let mut refusing = |shown: &[String]| -> Result<Built, StepError> {
            commits.borrow_mut().push(shown.to_vec());
            Ok(Built {
                shown: Vec::new(),
                removed: Vec::new(),
                refused: Some(E_ACCESSDENIED),
            })
        };
        worker.refresh_with(&rows(&[r"C:\c"]), true, &exists, &mut refusing);
        assert!(!file.exists(), "the shell said recent items are off");
        worker.refresh_with(&rows(&[r"C:\d"]), true, &exists, &mut commit);
        assert!(
            !file.exists(),
            "the refusal holds while the switch reads the same"
        );
        worker.refresh_with(&[], false, &exists, &mut commit);
        worker.refresh_with(&rows(&[r"C:\e"]), true, &exists, &mut commit);
        assert_eq!(kept(&file), rows(&[r"C:\e"]));
    }
}

#[cfg(debug_assertions)]
mod acceptance {
    use std::path::{Path, PathBuf};

    /// The identity the live read-back commits under: never aterm's own
    /// ([`crate::win32::AUMID`]), so the owner's jump list is never touched.
    const TEST_AUMID: &str = "Aterm.Test.JumpListReadBack";

    /// The real shell, end to end, under [`TEST_AUMID`]: [`super::build`]
    /// commits a Recent category of two directories and both tasks, and the
    /// shell's own store for that identity
    /// (`%APPDATA%\Microsoft\Windows\Recent\CustomDestinations\*.customDestinations-ms`),
    /// parsed by [`read_back`], holds what was asked: a `Recent` category whose
    /// rows carry `new-window -d "<dir>"`, the directory as working directory
    /// and tooltip, the folder icon and the row's title in the link's property
    /// store (`PKEY_Title`); then the tasks, `new-tab` and `new-window`, with the
    /// exe's icon, their tooltips and titles.
    ///
    /// # Errors
    /// Returns a setup failure when APPDATA or the current executable is unavailable.
    pub fn the_shell_keeps_what_a_commit_puts_in_a_test_identitys_jump_list(
        scratch: &Path,
    ) -> std::io::Result<()> {
        use std::os::windows::ffi::OsStrExt as _;
        let store = PathBuf::from(std::env::var_os("APPDATA").ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "APPDATA is unavailable")
        })?)
        .join(r"Microsoft\Windows\Recent\CustomDestinations");
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                // SAFETY: This bench owns only its distinct test identity.
                let _ = unsafe { super::delete_list(TEST_AUMID) };
            }
        }
        let _cleanup = Cleanup;
        // SAFETY: One COM transaction on this thread (`delete_list`); a clean slate.
        let _ = unsafe { super::delete_list(TEST_AUMID) };
        let before = read_back::snapshot(&store);

        let dirs: Vec<String> = ["jl-probe-alpha", "jl-probe-beta"]
            .iter()
            .map(|name| {
                let dir = scratch.join(name);
                std::fs::create_dir(&dir).expect("probe dir");
                crate::jumplist_recent::normalize(&dir.to_string_lossy()).expect("a kept path")
            })
            .collect();
        let exe = std::env::current_exe()?;
        let exe_text = exe.to_string_lossy().into_owned();
        let exe_w: Vec<u16> = exe
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `build` is one linear pass of documented shell COM calls on this thread.
        let built = unsafe { super::build(TEST_AUMID, &exe_w, true, &dirs) }
            .unwrap_or_else(|(step, hr)| panic!("commit: {step} hr={hr:#010x}"));
        let tracked = super::recents_tracked();
        assert_eq!(built.refused.is_some(), !tracked, "{:?}", built.refused);

        let (file, categories) = read_back::changed(&store, &before)
            .into_iter()
            .find_map(|path| {
                let categories = read_back::categories(&std::fs::read(&path).ok()?)?;
                let ours = categories
                    .iter()
                    .flat_map(|category| &category.entries)
                    .any(|entry| entry.icon.as_deref() == Some(exe_text.as_str()));
                ours.then_some((path, categories))
            })
            .expect("the shell wrote the test identity's list");
        eprintln!("{}: {categories:#?}", file.display());

        let mut categories = categories.into_iter();
        if tracked {
            let recent = categories.next().expect("the Recent category");
            assert_eq!(recent.name.as_deref(), Some("Recent"));
            let titles = crate::jumplist_recent::titles(&dirs);
            assert_eq!(recent.entries.len(), dirs.len());
            for ((row, dir), title) in recent.entries.iter().zip(&dirs).zip(&titles) {
                assert_eq!(
                    row.arguments.as_deref(),
                    Some(crate::jumplist_recent::row_arguments(dir).as_str())
                );
                assert_eq!(row.working_dir.as_deref(), Some(dir.as_str()));
                assert_eq!(row.tooltip.as_deref(), Some(dir.as_str()));
                let icon = row.icon.as_deref().unwrap_or_default().to_ascii_lowercase();
                assert!(icon.ends_with(r"\system32\shell32.dll"), "{icon}");
                assert!(row.holds(title), "title {title}: {row:?}");
            }
        }
        let tasks = categories.next().expect("the tasks");
        assert_eq!(tasks.name, None);
        let verbs: Vec<_> = tasks
            .entries
            .iter()
            .map(|task| task.arguments.as_deref())
            .collect();
        assert_eq!(verbs, [Some("new-tab"), Some("new-window")]);
        for (task, (title, tooltip)) in tasks.entries.iter().zip([
            ("New Tab", "Open a new tab in the running aterm"),
            ("New Window", "Open a new aterm window"),
        ]) {
            assert_eq!(task.tooltip.as_deref(), Some(tooltip));
            assert_eq!(task.icon.as_deref(), Some(exe_text.as_str()));
            assert_eq!(task.working_dir, None);
            assert!(task.holds(title), "title {title}: {task:?}");
        }
        assert!(categories.next().is_none(), "nothing else was committed");

        // SAFETY: As above.
        unsafe { super::delete_list(TEST_AUMID) }
            .unwrap_or_else(|(step, hr)| panic!("DeleteList: {step} hr={hr:#010x}"));
        assert!(!file.exists(), "DeleteList removed {}", file.display());
        Ok(())
    }

    /// A reader for the shell's `.customDestinations-ms` store, enough to read
    /// back what [`crate::jumplist_win::build`] committed: a version-2 header and a category
    /// count, then per category its type (0 a custom category with a counted
    /// UTF-16 name, 2 the tasks), its entry count, each entry a
    /// `CLSID_ShellLink` tag and an MS-SHLLINK stream, and a `0xBABFFBAB`
    /// footer.
    mod read_back {
        use std::collections::HashMap;
        use std::path::{Path, PathBuf};
        use std::time::SystemTime;

        #[derive(Debug)]
        pub(super) struct Category {
            /// A custom category's name; `None` for the tasks.
            pub(super) name: Option<String>,
            pub(super) entries: Vec<Entry>,
        }

        /// One link's strings (MS-SHLLINK `StringData`).
        pub(super) struct Entry {
            /// `SetDescription`: the row's tooltip.
            pub(super) tooltip: Option<String>,
            pub(super) working_dir: Option<String>,
            pub(super) arguments: Option<String>,
            pub(super) icon: Option<String>,
            /// The extra data blocks, raw: the property store among them keeps
            /// `PKEY_Title`.
            pub(super) extra: Vec<u8>,
        }

        impl std::fmt::Debug for Entry {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct("Entry")
                    .field("tooltip", &self.tooltip)
                    .field("working_dir", &self.working_dir)
                    .field("arguments", &self.arguments)
                    .field("icon", &self.icon)
                    .field("extra_bytes", &self.extra.len())
                    .finish()
            }
        }

        impl Entry {
            /// Whether the extra data holds `text` as UTF-16.
            pub(super) fn holds(&self, text: &str) -> bool {
                let needle: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
                self.extra
                    .windows(needle.len())
                    .any(|window| window == needle)
            }
        }

        type Snapshot = HashMap<PathBuf, (u64, Option<SystemTime>)>;

        /// Each file in `dir`, with its length and modification time.
        pub(super) fn snapshot(dir: &Path) -> Snapshot {
            std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|entry| {
                    let meta = entry.metadata().ok()?;
                    Some((entry.path(), (meta.len(), meta.modified().ok())))
                })
                .collect()
        }

        /// The files in `dir` that are new or changed since `before`.
        pub(super) fn changed(dir: &Path, before: &Snapshot) -> Vec<PathBuf> {
            snapshot(dir)
                .into_iter()
                .filter(|(path, state)| before.get(path) != Some(state))
                .map(|(path, _)| path)
                .collect()
        }

        struct Cursor<'a> {
            bytes: &'a [u8],
            at: usize,
        }

        impl<'a> Cursor<'a> {
            fn take(&mut self, n: usize) -> Option<&'a [u8]> {
                let out = self.bytes.get(self.at..self.at.checked_add(n)?)?;
                self.at += n;
                Some(out)
            }

            fn u16(&mut self) -> Option<u16> {
                Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
            }

            fn u32(&mut self) -> Option<u32> {
                Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
            }

            fn utf16(&mut self, units: usize) -> Option<String> {
                Some(utf16(self.take(units.checked_mul(2)?)?))
            }
        }

        fn utf16(raw: &[u8]) -> String {
            let units: Vec<u16> = raw
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }

        /// `CLSID_ShellLink` {00021401-0000-0000-C000-000000000046}, as stored.
        const SHELL_LINK: [u8; 16] = [
            0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x46,
        ];
        const FOOTER: u32 = 0xBABF_FBAB;

        /// The categories of a store, or `None` when it is not one.
        pub(super) fn categories(bytes: &[u8]) -> Option<Vec<Category>> {
            let mut at = Cursor { bytes, at: 0 };
            if at.u32()? != 2 {
                return None;
            }
            let count = at.u32()?;
            let _reserved = at.u32()?;
            let mut out = Vec::new();
            for _ in 0..count {
                let name = match at.u32()? {
                    0 => {
                        let chars = at.u16()?;
                        Some(at.utf16(usize::from(chars))?)
                    }
                    2 => None,
                    _ => return None,
                };
                let mut entries = Vec::new();
                for _ in 0..at.u32()? {
                    if at.take(16)? != SHELL_LINK {
                        return None;
                    }
                    entries.push(link(&mut at)?);
                }
                if at.u32()? != FOOTER {
                    return None;
                }
                out.push(Category { name, entries });
            }
            Some(out)
        }

        /// One MS-SHLLINK stream: the header (`LinkFlags` says which parts
        /// follow), the ID list, the link info, the strings and the extra data
        /// blocks up to the terminal one.
        fn link(at: &mut Cursor<'_>) -> Option<Entry> {
            const HEADER: usize = 0x4C;
            if at.u32()? != HEADER as u32 {
                return None;
            }
            let _clsid = at.take(16)?;
            let flags = at.u32()?;
            at.take(HEADER - 24)?;
            let has = |bit: u32| flags & bit != 0;
            if has(0x1) {
                let len = at.u16()?;
                at.take(usize::from(len))?;
            }
            if has(0x2) {
                let len = usize::try_from(at.u32()?).ok()?;
                at.take(len.checked_sub(4)?)?;
            }
            let unicode = has(0x80);
            let mut string = |bit: u32| -> Option<Option<String>> {
                if !has(bit) {
                    return Some(None);
                }
                let chars = usize::from(at.u16()?);
                Some(Some(if unicode {
                    at.utf16(chars)?
                } else {
                    String::from_utf8_lossy(at.take(chars)?).into_owned()
                }))
            };
            let tooltip = string(0x4)?;
            let _relative_path = string(0x8)?;
            let working_dir = string(0x10)?;
            let arguments = string(0x20)?;
            let icon = string(0x40)?;
            let extra_start = at.at;
            loop {
                let size = usize::try_from(at.u32()?).ok()?;
                if size < 4 {
                    break;
                }
                at.take(size - 4)?;
            }
            Some(Entry {
                tooltip,
                working_dir,
                arguments,
                icon,
                extra: at.bytes[extra_start..at.at].to_vec(),
            })
        }
    }
}

#[cfg(debug_assertions)]
pub use acceptance::the_shell_keeps_what_a_commit_puts_in_a_test_identitys_jump_list;
