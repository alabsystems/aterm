// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Windows taskbar **jump list** — the menu behind a right-click (or upward
//! swipe) on aterm's taskbar button.
//!
//! Without this, that menu shows only the shell defaults (the recent-items
//! stub, "Pin to taskbar", "Close window") — a gap every established Windows
//! terminal has closed (Windows Terminal, ConEmu, PuTTY, mintty all ship
//! tasks). This is the **tasks-only cut**: a "New Window" task that launches a
//! fresh aterm, plus a "New Tab" task **when, and only when, the operator has
//! asked for attach behaviour**. Deliberately NOT here, and why:
//!
//! * **"Settings"** — there is no CLI surface that opens the Settings tab
//!   (`cli.rs` has no `--settings`; Ctrl-, is an in-app action), and a jump
//!   list is the wrong reason to invent one. If a settings verb ever lands in
//!   `cli.rs`, add a second [`task`] call below — the plumbing is ready.
//! * **Recent/Frequent destinations** — a terminal's "documents" would be
//!   directories, which wants `SHAddToRecentDocs` plumbing and a privacy
//!   stance (the list persists in the user's shell profile). Out of scope for
//!   the tasks cut; `AppendKnownCategory` is declared in the vtable for the
//!   day it matters.
//!
//! **Identity**: the list is registered against the process AUMID
//! ([`crate::win32::AUMID`], set via `SetCurrentProcessExplicitAppUserModelID`
//! at startup) with `ICustomDestinationList::SetAppID` *before* `BeginList`.
//! The Start-Menu shortcut stamps the SAME AUMID (`apps/aterm-win/
//! install.ps1`, landed with the pin-identity work), so the running button,
//! the pinned tile, and this jump list all resolve to one identity — without
//! `SetAppID` the list would attach to the exe-path identity and a *pinned*
//! aterm would never show it.
//!
//! **The tasks carry VERBS, not a bare launch** (S12). "New Window" runs
//! `aterm.exe new-window`, the one verb the routing policy never redirects, so
//! the row does what it says even when the operator set
//! `windowing_behavior = "attach"` — a bare launch under that policy would open
//! a TAB, and a taskbar row labelled "New Window" that opens a tab is a lie the
//! shell would keep telling from the user's persisted profile. "New Tab" runs
//! `aterm.exe new-tab`, and is only OFFERED under `attach`: under the default
//! `new_window` policy that verb opens a window (deliberately — see
//! `aterm_cli::route_launch`), so the row would be a second "New Window" with a
//! misleading name. One list per policy, each honest about itself.
//!
//! **Self-healing exe path**: the task's target is resolved from
//! `current_exe()` at every registration, not an install-time constant — move
//! the install (or run a dev build) and the next launch rewrites the list to
//! point at itself. The shell persists the committed list in the user profile,
//! so re-committing an identical list each launch is exactly what Windows
//! Terminal does; the write is a few KB once per process.
//!
//! It is `current_exe()`'s *front door*, not `current_exe()` itself — see
//! [`front_door_exe`]. The install directory holds several identical copies of
//! the one binary under the sibling names, the Start-Menu shortcut targets
//! `aterm-gui.exe`, and this list is committed by whichever copy is running, so
//! the row has to name a binary that ROUTES the verb rather than one that hands
//! it to the window's flag parser.
//!
//! **Timing & failure posture**: registration runs once per process on a
//! throwaway background thread spawned *after the first present* (see the
//! `JUMP_LIST` Once next to the font-warm hook in `app_render.rs`) — the shell
//! round-trips registry + profile-disk IO, none of which belongs on
//! time-to-glass. Every failure is silent-best-effort: one `aterm_log` warn
//! line, never a startup error — a terminal that cannot register a jump list
//! is still a terminal (Server Core, shell restarting, policy-restricted
//! profiles all land here).
//!
//! **FFI style**: hand-rolled by-hand-vtable COM against system DLLs (ole32),
//! the same tiny-FFI house style as the `ITaskbarList3` progress block in
//! `app_window.rs` — see the vocabulary note there. The `windows` crate is in
//! this binary for DXGI only; pulling `Win32_UI_Shell` (+ `Win32_System_Com` +
//! the PROPVARIANT stack) for four leaf interfaces used once at startup was
//! rejected as a heavyweight feature add for no safety gain: the vtables below
//! are transcribed from `shobjidl_core.h`/`propsys.h` and every slot before a
//! called one is declared so offsets are load-bearing and checked by shape.
#![cfg(windows)]

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;

// ---------------------------------------------------------------------------
// GUIDs (from shobjidl_core.h / propsys.h / propkey.h)
// ---------------------------------------------------------------------------

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

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
/// `IID_IObjectArray` {92CA9DCD-5622-4BBA-A805-5E9F541BD8C9} — only used as the
/// riid for `BeginList`'s removed-destinations out-param.
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

// ---------------------------------------------------------------------------
// Property-store plumbing for the task title
// ---------------------------------------------------------------------------

/// `PROPERTYKEY` (propsys.h): a fmtid GUID plus a property id.
#[repr(C)]
struct PropertyKey {
    fmtid: Guid,
    pid: u32,
}

/// `PKEY_Title` ({F29F85E0-4FF9-1068-AB91-08002B27B3D9}, 2) — the string the
/// jump-list row actually *displays*. `IShellLinkW::SetDescription` is only the
/// hover tooltip; without a `System.Title` the row shows the link's target
/// filename ("aterm-gui.exe"), which is why the property store is mandatory
/// here and not gold-plating.
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

/// A shape-only `PROPVARIANT`, wide enough for every ABI aterm builds
/// (24 bytes on x64: 8-byte discriminant header + 16-byte data union; the
/// callee never reads past its own `sizeof`, so over-size on x86 is harmless).
/// Only the `VT_LPWSTR` arm is modeled: `ptr` occupies the union's first
/// pointer slot (`pwszVal`), `pad` zero-fills the rest.
///
/// Ownership subtlety, and why no `CoTaskMemAlloc`: `InitPropVariantFromString`
/// copies into CoTaskMem *so that a later `PropVariantClear` can free it*. We
/// never call `PropVariantClear` — `SetValue` copies the value into the store
/// and our variant then simply goes out of scope — so pointing `pwszVal` at a
/// caller-owned NUL-terminated buffer is sound and leak-free.
#[repr(C)]
struct PropVariant {
    vt: u16,
    reserved1: u16,
    reserved2: u16,
    reserved3: u16,
    ptr: *const u16,
    pad: [u64; 1],
}

// ---------------------------------------------------------------------------
// Vtables, in COM inheritance order. Slots before a called one are declared
// with their real shapes and never invoked — their OFFSETS are load-bearing.
// ---------------------------------------------------------------------------

/// The 3-slot `IUnknown` prefix every COM vtable begins with; any interface
/// pointer can be viewed through it for `QueryInterface`/`Release` without
/// caring which concrete interface it is.
#[repr(C)]
#[allow(dead_code)] // query_interface is called; add_ref is layout-only
struct IUnknownVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct IUnknownRepr {
    vtbl: *const IUnknownVtbl,
}

/// `ICustomDestinationList` (shobjidl_core.h).
#[repr(C)]
#[allow(dead_code)] // layout-only slots: their offsets are load-bearing, not their use
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
#[allow(dead_code)] // layout-only slots: their offsets are load-bearing, not their use
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

/// `IShellLinkW` (shobjidl_core.h). Only `SetPath` / `SetIconLocation` /
/// `SetDescription` are called; the fourteen slots before them keep the
/// documented method order so those three land on the right vtable entries.
#[repr(C)]
#[allow(dead_code)] // layout-only slots: their offsets are load-bearing, not their use
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
#[allow(dead_code)] // layout-only slots: their offsets are load-bearing, not their use
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

const CLSCTX_INPROC_SERVER: u32 = 0x1;
const COINIT_APARTMENTTHREADED: u32 = 0x2;

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, co_init: u32) -> i32;
    fn CoUninitialize();
    fn CoCreateInstance(
        rclsid: *const Guid,
        outer: *mut c_void,
        cls_ctx: u32,
        riid: *const Guid,
        ppv: *mut *mut c_void,
    ) -> i32;
}

// ---------------------------------------------------------------------------
// Tiny RAII so error paths cannot leak COM references
// ---------------------------------------------------------------------------

/// An owned COM interface pointer (any interface — released through the
/// `IUnknown` prefix every vtable shares). Exists so `?`-style early returns in
/// [`build`] cannot leak a reference; the linear manual-release style of the
/// `taskbar` module does not survive this many fallible steps.
struct Com(*mut c_void);

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a live interface pointer obtained from
        // CoCreateInstance/QueryInterface on this thread; every COM vtable
        // begins with the IUnknown triple, so the prefix view is sound.
        unsafe {
            let unk = self.0.cast::<IUnknownRepr>();
            ((*(*unk).vtbl).release)(self.0);
        }
    }
}

/// Balances a successful `CoInitializeEx` when [`build`] returns by any path.
struct CoUninitGuard;

impl Drop for CoUninitGuard {
    fn drop(&mut self) {
        // SAFETY: constructed only after CoInitializeEx succeeded on this
        // thread, so the pairing rule is upheld.
        unsafe { CoUninitialize() };
    }
}

/// A failed step: which call, and its HRESULT — the whole error surface of this
/// module (one warn line; jump-list absence must never be louder than that).
type StepError = (&'static str, i32);

fn check(hr: i32, step: &'static str) -> Result<(), StepError> {
    if hr < 0 { Err((step, hr)) } else { Ok(()) }
}

/// NUL-terminated UTF-16, the PCWSTR shape every call below takes.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn co_create(clsid: &Guid, iid: &Guid, step: &'static str) -> Result<Com, StepError> {
    let mut ppv: *mut c_void = std::ptr::null_mut();
    // SAFETY: standard object creation; `ppv` receives the interface pointer
    // only on success, and a success with a null pointer is rejected.
    let hr = unsafe {
        CoCreateInstance(
            clsid,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            iid,
            &mut ppv,
        )
    };
    if hr < 0 || ppv.is_null() {
        return Err((step, hr));
    }
    Ok(Com(ppv))
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register (or refresh) the tasks-only jump list for this process's AUMID.
///
/// Called once per process from the first-present hook in `app_render.rs`, on a
/// short-lived dedicated thread: the shell walks registry + user-profile disk
/// state under `CommitList`, and none of that belongs anywhere near the render
/// loop. The thread is its own COM apartment (STA, as shell objects prefer),
/// initialized and torn down entirely in here.
///
/// Best-effort by design: any failure logs a single warn line and leaves the
/// taskbar menu at shell defaults — identical to today's behaviour, never a
/// startup failure.
pub(crate) fn install() {
    let current = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            aterm_log::warn!("taskbar jump list skipped (current_exe: {error})");
            return;
        }
    };
    let exe = front_door_exe(&current, |path| path.is_file());
    let exe_w: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // Read the routing policy HERE rather than taking it as a parameter: this
    // already runs on its own thread after the first present, a bounded config
    // read costs nothing next to the shell's registry + profile round trip, and
    // it keeps the first-present hook in `app_render.rs` a single argument-less
    // call. The list is re-committed every launch anyway (see the module docs),
    // so flipping the config and restarting rewrites the menu.
    let attach = attach_policy_is_on();
    // SAFETY: `build` is one linear pass of documented shell COM calls; every
    // pointer it hands the shell lives for the duration of its call (the wide
    // strings are locals that outlive each use), every returned interface is
    // RAII-released, and the apartment is balanced by CoUninitGuard.
    match unsafe { build(&exe_w, attach) } {
        Ok(()) => {
            if attach {
                aterm_log::debug!("taskbar jump list committed (tasks: New Tab, New Window)");
            } else {
                aterm_log::debug!("taskbar jump list committed (tasks: New Window)");
            }
        }
        Err((step, hr)) => {
            aterm_log::warn!("taskbar jump list skipped ({step}: hr={hr:#010x})");
        }
    }
}

/// The executable a jump-list row should launch: the FRONT DOOR, which is the
/// binary that routes `new-tab` / `new-window` / `split-pane`.
///
/// Not simply `current_exe()`, and the difference is a shipped-feature bug, not
/// a dev-build nicety. The Windows install directory holds SEVERAL IDENTICAL
/// COPIES of the one binary under the sibling names — `aterm.exe`,
/// `aterm-gui.exe`, `aterm-ctl.exe`, … — and the Start-Menu shortcut targets
/// `aterm-gui.exe`, so the live instance that commits this list is normally
/// running as `aterm-gui.exe`. That name is an argv0 ALIAS: it means "the
/// window", and `aterm-gui.exe new-window` used to reach the window's own flag
/// parser and exit 2 with `unknown option 'new-window'` — to a console that does
/// not exist, because the shell launched it. A working taskbar row went silently
/// dead.
///
/// So: if this process is already `aterm`, use it. Otherwise prefer a sibling
/// `aterm` in the same directory — the canonical front door, present in every
/// install and in every dev `target/<profile>` (which also gets the row the
/// icon `build.rs` compiles into `aterm.exe` alone). Only if there is no such
/// sibling does the row fall back to this process, which the front door's
/// alias dispatch now routes anyway; the two fixes are independent and either
/// one alone is enough.
///
/// `exists` is injected so the rule is unit-testable without laying out a fake
/// install tree on disk.
fn front_door_exe(
    current: &std::path::Path,
    exists: impl Fn(&std::path::Path) -> bool,
) -> std::path::PathBuf {
    // Folded FIRST, suffix included: Windows filenames are case-insensitive, so
    // `ATERM.EXE` is the same file as `aterm.exe` and must take the same arm.
    let name = current
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let suffix = std::env::consts::EXE_SUFFIX.to_ascii_lowercase();
    let stem = name.strip_suffix(&suffix).unwrap_or(&name);
    if stem == "aterm" {
        return current.to_path_buf();
    }
    let sibling = current
        .parent()
        .map(|dir| dir.join(format!("aterm{}", std::env::consts::EXE_SUFFIX)));
    match sibling {
        Some(path) if exists(&path) => path,
        _ => current.to_path_buf(),
    }
}

/// Whether `windowing_behavior` is `attach` — i.e. whether a "New Tab" row would
/// actually produce a tab.
///
/// The spelling test is intentionally duplicated in miniature rather than
/// reached for through `aterm-cli` (this crate does not depend on it, and adding
/// the dependency to decide one menu row would be backwards). Kept in sync by
/// being the SAME two accepted spellings the front door's parser accepts for
/// this value; anything else — including a typo — leaves the list at the safe
/// one-task shape, which is exactly what the front door's own fallback does.
fn attach_policy_is_on() -> bool {
    crate::windowing_behavior_setting().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "attach" | "use_existing" | "use-existing" | "useexisting" | "existing"
        )
    })
}

unsafe fn build(exe: &[u16], attach: bool) -> Result<(), StepError> {
    // A fresh thread, so APARTMENTTHREADED cannot RPC_E_CHANGED_MODE; a real
    // failure here means COM itself is unavailable and the list is off the
    // table for this launch.
    let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED) };
    check(hr, "CoInitializeEx")?;
    let _co = CoUninitGuard;

    let list = unsafe {
        co_create(
            &CLSID_DESTINATION_LIST,
            &IID_ICUSTOM_DESTINATION_LIST,
            "create DestinationList",
        )?
    };
    let list_ptr = list.0.cast::<ICustomDestinationList>();
    let list_vt = unsafe { &*(*list_ptr).vtbl };

    // Identity FIRST: bind the list to the process AUMID before BeginList, or
    // it silently attaches to the exe-path identity — visibly fine on the live
    // button, invisibly absent on the AUMID-stamped pinned tile.
    let aumid = wide(crate::win32::AUMID);
    check(
        unsafe { (list_vt.set_app_id)(list_ptr, aumid.as_ptr()) },
        "SetAppID",
    )?;

    // BeginList opens the transaction and MUST hand back the removed-items
    // array. Tasks cannot be removed by the user (only destinations can), so
    // the array is release-and-ignore — but skipping the retrieval would fail
    // the call's contract.
    let mut min_slots = 0u32;
    let mut removed: *mut c_void = std::ptr::null_mut();
    check(
        unsafe { (list_vt.begin_list)(list_ptr, &mut min_slots, &IID_IOBJECT_ARRAY, &mut removed) },
        "BeginList",
    )?;
    if !removed.is_null() {
        drop(Com(removed));
    }

    let tasks = unsafe {
        co_create(
            &CLSID_ENUMERABLE_OBJECT_COLLECTION,
            &IID_IOBJECT_COLLECTION,
            "create ObjectCollection",
        )?
    };
    let tasks_ptr = tasks.0.cast::<IObjectCollection>();

    // The tasks of the tasks-only cut, in menu order. A future "Settings"
    // (pending a real CLI verb) is one more `task(...)` + `add_object` pair here.
    //
    // "New Tab" leads under `attach` because it is then the ordinary act and
    // "New Window" the deliberate one — the same order Windows Terminal's own
    // jump list uses.
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
            unsafe { ((*(*tasks_ptr).vtbl).add_object)(tasks_ptr, new_tab.0) },
            "AddObject",
        )?;
    }
    let new_window = unsafe { task(exe, "new-window", "New Window", "Open a new aterm window")? };
    check(
        unsafe { ((*(*tasks_ptr).vtbl).add_object)(tasks_ptr, new_window.0) },
        "AddObject",
    )?;

    // IObjectCollection inherits IObjectArray, so the collection pointer IS the
    // IObjectArray AddUserTasks wants — no QueryInterface detour needed.
    check(
        unsafe { (list_vt.add_user_tasks)(list_ptr, tasks.0) },
        "AddUserTasks",
    )?;
    // Commit publishes atomically; an error-path return before this point
    // abandons the transaction on release and the previously committed list
    // (if any) stays in force — the shell's documented AbortList-on-release.
    check(unsafe { (list_vt.commit_list)(list_ptr) }, "CommitList")?;
    Ok(())
}

/// Build one task entry: an in-memory `IShellLinkW` whose target is `exe` run
/// with `args` (a front-door VERB — see the module docs on why a bare launch is
/// no longer good enough), whose icon is the exe's own (index 0 — the aterm icon
/// compiled in by `build.rs`), and whose visible row text is set via the link's
/// property store (`PKEY_Title` — see that const for why `SetDescription` alone
/// would render "aterm-gui.exe").
///
/// `exe` is [`front_door_exe`]'s answer, not raw `current_exe()`: the row must
/// name a binary that ROUTES the verb. That matters most for the thin dev
/// `aterm-gui` bin, which is genuinely not the router and knows none of these
/// verbs — it sits beside a real `target/<profile>/aterm.exe`, which is what the
/// row then points at (and which is also the only one of the two carrying the
/// icon `crates/aterm/build.rs` compiles in).
unsafe fn task(exe: &[u16], args: &str, title: &str, tooltip: &str) -> Result<Com, StepError> {
    let link = unsafe { co_create(&CLSID_SHELL_LINK, &IID_ISHELL_LINK_W, "create ShellLink")? };
    let link_ptr = link.0.cast::<IShellLinkW>();
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
    check(
        unsafe { (link_vt.set_icon_location)(link_ptr, exe.as_ptr(), 0) },
        "SetIconLocation",
    )?;
    let tip = wide(tooltip);
    check(
        unsafe { (link_vt.set_description)(link_ptr, tip.as_ptr()) },
        "SetDescription",
    )?;

    let mut store_raw: *mut c_void = std::ptr::null_mut();
    let unk = link.0.cast::<IUnknownRepr>();
    let hr =
        unsafe { ((*(*unk).vtbl).query_interface)(link.0, &IID_IPROPERTY_STORE, &mut store_raw) };
    if hr < 0 || store_raw.is_null() {
        return Err(("QueryInterface IPropertyStore", hr));
    }
    let store = Com(store_raw);
    let store_ptr = store.0.cast::<IPropertyStore>();
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

    /// THE SHIPPED WINDOWS LAYOUT, which is where this bit went wrong: the
    /// install directory is several IDENTICAL copies of the one binary, the
    /// Start-Menu shortcut targets `aterm-gui.exe`, and the running instance
    /// that commits the jump list is therefore `aterm-gui.exe`. Every row must
    /// still name the front door, because `aterm-gui.exe` is an argv0 alias for
    /// "the window" and a row that hands `new-window` to the window's own flag
    /// parser dies with `unknown option` against a console that does not exist.
    #[test]
    fn a_row_committed_by_an_alias_copy_still_names_the_front_door() {
        let dir = install_dir();
        for alias in [
            "aterm-gui",
            "aterm-ctl",
            "aterm-cli",
            "atpkg",
            "aterm-fleet",
        ] {
            let current = dir.join(exe(alias));
            assert_eq!(
                front_door_exe(&current, |path| path == dir.join(exe("aterm"))),
                dir.join(exe("aterm")),
                "a list committed by {alias} must launch the front door"
            );
        }
    }

    /// `aterm.exe` itself is already the front door — no sibling lookup, no
    /// chance of pointing at a different copy than the one that is running.
    #[test]
    fn the_front_door_itself_is_left_alone() {
        let dir = install_dir();
        let current = dir.join(exe("aterm"));
        assert_eq!(
            front_door_exe(&current, |_| panic!("must not probe for a sibling")),
            current
        );
        // Windows filesystem names are case-insensitive; so is the check.
        let shouty = dir.join(exe("ATERM").to_uppercase());
        assert_eq!(
            front_door_exe(&shouty, |_| panic!("must not probe for a sibling")),
            shouty
        );
    }

    /// No sibling front door on disk (a copy of the exe on its own somewhere):
    /// fall back to this process rather than committing a row that names a file
    /// that is not there. The front door's argv0-alias dispatch routes the verb
    /// under the alias name too, so the row still works.
    #[test]
    fn a_lone_copy_falls_back_to_itself() {
        let current = Path::new(r"C:\tmp\scratch").join(exe("aterm-gui"));
        assert_eq!(front_door_exe(&current, |_| false), current);
    }
}
