// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Platform clipboard and X11 `PRIMARY` selection layer.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Where one clipboard read or write goes ([`route`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ClipboardRoute {
    /// The platform clipboard.
    System,
    /// `ATERM_DEBUG_CLIPBOARD_FILE=<path>`.
    File(PathBuf),
    /// A test build's thread-local board ([`PBPASTE_STUB`], and PRIMARY's twin on Linux).
    #[cfg(test)]
    TestBoard,
}

fn route_for(seam: Option<OsString>) -> ClipboardRoute {
    match seam {
        None => ClipboardRoute::System,
        Some(path) => ClipboardRoute::File(PathBuf::from(path)),
    }
}

fn route() -> ClipboardRoute {
    #[cfg(test)]
    {
        LAB_FILE_FOR_TEST
            .with(|file| file.borrow().clone())
            .map_or(ClipboardRoute::TestBoard, ClipboardRoute::File)
    }
    #[cfg(not(test))]
    {
        route_for(aterm_types::dev_seam!("ATERM_DEBUG_CLIPBOARD_FILE"))
    }
}

#[cfg(test)]
thread_local! {
    /// A test's stand-in for `ATERM_DEBUG_CLIPBOARD_FILE` on this thread: `Some(path)` routes
    /// the clipboard to that lab file in place of the test board.
    pub(crate) static LAB_FILE_FOR_TEST: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// WHETHER A COPY OR A PASTE REACHES THE PERSON'S OWN CLIPBOARD: the platform's, as it always
/// is in a release build, which never reads the seam. A debug or `dev-seams` build pointed at a
/// lab file (`ATERM_DEBUG_CLIPBOARD_FILE`) copies to and pastes from that file, which is nobody's
/// clipboard, so there a control client's copy or paste reaches no person's clipboard and is not
/// refused for it (`App::clipboard_gesture_refused`): a lab can drive a Copy button to its
/// `Copied` and read the file. A test board stands for the person's.
pub(crate) fn reaches_persons_clipboard() -> bool {
    route_reaches_person(&route())
}

fn route_reaches_person(route: &ClipboardRoute) -> bool {
    !matches!(route, ClipboardRoute::File(_))
}

const CLIPBOARD_FILE_CAP: u64 = 64 << 20;

/// A clipboard write needs an owner until after the clipboard has closed.
#[cfg(any(windows, test))]
pub(crate) fn write_with_owner<O, G>(
    owner: impl FnOnce() -> Option<O>,
    open: impl FnOnce(&O) -> Option<G>,
    write: impl FnOnce() -> bool,
) -> bool {
    let Some(owner) = owner() else {
        return false;
    };
    let Some(_open) = open(&owner) else {
        return false;
    };
    write()
}

fn file_copy(path: &Path, text: &str) -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let Some(name) = path.file_name() else {
        return false;
    };
    let mut tmp_name = OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = path.with_file_name(tmp_name);
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    if opts
        .open(&tmp)
        .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()))
        .is_ok()
        && std::fs::rename(&tmp, path).is_ok()
    {
        return true;
    }
    let _ = std::fs::remove_file(&tmp);
    false
}

fn file_paste(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let (file, meta) = aterm_uds::regular_file::open(path).ok()?;
    let cap = usize::try_from(meta.len().min(CLIPBOARD_FILE_CAP))
        .unwrap_or(0)
        .saturating_add(1);
    let mut text = String::with_capacity(cap);
    file.take(CLIPBOARD_FILE_CAP)
        .read_to_string(&mut text)
        .ok()?;
    (!text.is_empty()).then_some(text)
}

/// PRIMARY's stand-in, `<path>.primary` ([`primary_set`]).
#[cfg(any(test, target_os = "linux"))]
fn primary_file(path: &Path) -> PathBuf {
    let mut primary = path.as_os_str().to_owned();
    primary.push(".primary");
    PathBuf::from(primary)
}

#[cfg(test)]
thread_local! {
    pub(crate) static PBPASTE_STUB: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// A thread-local test board, as [`ClipboardRoute::TestBoard`] reads and writes it.
#[cfg(test)]
type TestBoard = std::thread::LocalKey<std::cell::RefCell<Option<String>>>;

/// A copy onto a test board: always placed.
#[cfg(test)]
fn board_copy(board: &'static TestBoard, text: &str) -> bool {
    board.with(|b| *b.borrow_mut() = Some(text.to_owned()));
    true
}

/// A read of a test board: `None` when it holds nothing or an empty string.
#[cfg(test)]
fn board_paste(board: &'static TestBoard) -> Option<String> {
    board
        .with(|b| b.borrow().clone())
        .filter(|text| !text.is_empty())
}

/// Place `text` on the clipboard.
pub(crate) fn pbcopy(text: &str) -> bool {
    match route() {
        ClipboardRoute::System => system_pbcopy(text),
        ClipboardRoute::File(path) => file_copy(&path, text),
        #[cfg(test)]
        ClipboardRoute::TestBoard => board_copy(&PBPASTE_STUB, text),
    }
}

fn system_pbcopy(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        use aterm_objc::{Id, Sel, autoreleasepool, class, sel};

        use crate::appkit;
        aterm_macos_ui::appkit::load();
        // SAFETY: `+generalPasteboard` is `-(id)`, a valid singleton on any thread;
        // `-clearContents` is `-(NSInteger)`, `-setString:forType:` is
        // `-(BOOL)(NSString *, NSPasteboardType)`, and both arguments outlive the call.
        autoreleasepool(|_| unsafe {
            let Some(ns) = appkit::nsstring(text) else {
                return false;
            };
            let pb = appkit::send_id(class(c"NSPasteboard").as_id(), sel!(generalPasteboard));
            if pb.is_null() || pasteboard_type_string().is_null() {
                return false;
            }
            let _ = appkit::send_isize(pb, sel!(clearContents));
            let set: unsafe extern "C-unwind" fn(Id, Sel, Id, Id) -> aterm_objc::Bool =
                aterm_objc::checked_msg();
            set(
                pb,
                sel!(setString:forType:),
                ns.id(),
                pasteboard_type_string(),
            )
            .as_bool()
        })
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(wayland) = crate::clipboard_wayland::handle() {
            return wayland.copy(crate::clipboard_wayland::WaylandSelection::Clipboard, text);
        }
        crate::clipboard_x11::X11Clipboard::get_handle()
            .is_some_and(|c| c.set(crate::clipboard_x11::Sel::Clipboard, text))
    }
    #[cfg(windows)]
    {
        crate::clipboard_win::set(text)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = text;
        false
    }
}

/// `NSPasteboardTypeString`, the UTF-8 plain-text pasteboard type.
#[cfg(target_os = "macos")]
fn pasteboard_type_string() -> aterm_objc::Id {
    aterm_macos_ui::appkit::pasteboard_type_string()
}

/// Read the clipboard as UTF-8 text, or `None` when empty or unavailable.
pub(crate) fn pbpaste() -> Option<String> {
    match route() {
        ClipboardRoute::System => system_pbpaste(),
        ClipboardRoute::File(path) => file_paste(&path),
        #[cfg(test)]
        ClipboardRoute::TestBoard => board_paste(&PBPASTE_STUB),
    }
}

fn system_pbpaste() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use aterm_objc::{autoreleasepool, class, sel};

        use crate::appkit;
        aterm_macos_ui::appkit::load();
        // SAFETY: `+generalPasteboard` is `-(id)`, a valid singleton on any thread;
        // `-stringForType:` is `-(NSString *)(NSPasteboardType)`, and its borrowed result is
        // copied out inside this pool.
        let s = autoreleasepool(|_| unsafe {
            let pb = appkit::send_id(class(c"NSPasteboard").as_id(), sel!(generalPasteboard));
            if pb.is_null() || pasteboard_type_string().is_null() {
                return String::new();
            }
            appkit::nsstring_to_rust(appkit::send_id_id(
                pb,
                sel!(stringForType:),
                pasteboard_type_string(),
            ))
        });
        (!s.is_empty()).then_some(s)
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(wayland) = crate::clipboard_wayland::handle() {
            return wayland.paste(crate::clipboard_wayland::WaylandSelection::Clipboard);
        }
        crate::clipboard_x11::X11Clipboard::get_handle()
            .and_then(|c| c.get(crate::clipboard_x11::Sel::Clipboard))
    }
    #[cfg(windows)]
    {
        crate::clipboard_win::get()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        None
    }
}

/// Read the `CF_HDROP` file list from the Windows clipboard.
#[cfg(windows)]
pub(crate) fn pbpaste_paths() -> Option<Vec<String>> {
    match route() {
        ClipboardRoute::System => crate::clipboard_win::get_paths(),
        ClipboardRoute::File(_) => None,
        #[cfg(test)]
        ClipboardRoute::TestBoard => None,
    }
}

/// Non-blocking X11 twin of [`pbpaste`].
#[cfg(target_os = "linux")]
pub(crate) fn pbpaste_owned() -> Option<String> {
    match route() {
        ClipboardRoute::System => system_pbpaste_owned(),
        ClipboardRoute::File(path) => file_paste(&path),
        #[cfg(test)]
        ClipboardRoute::TestBoard => board_paste(&PBPASTE_STUB),
    }
}

/// [`pbpaste_owned`]'s platform arm.
#[cfg(target_os = "linux")]
fn system_pbpaste_owned() -> Option<String> {
    if let Some(wayland) = crate::clipboard_wayland::handle() {
        return wayland.paste_owned(crate::clipboard_wayland::WaylandSelection::Clipboard);
    }
    crate::clipboard_x11::X11Clipboard::get_handle()
        .and_then(|c| c.get_owned(crate::clipboard_x11::Sel::Clipboard))
}

/// Set the X11 PRIMARY selection (select-to-copy, middle-click paste) to `text`.
#[cfg(target_os = "linux")]
pub(crate) fn primary_set(text: &str) -> bool {
    match route() {
        ClipboardRoute::System => system_primary_set(text),
        ClipboardRoute::File(path) => file_copy(&primary_file(&path), text),
        #[cfg(test)]
        ClipboardRoute::TestBoard => board_copy(&PRIMARY_STUB, text),
    }
}

/// [`primary_set`]'s platform arm.
#[cfg(target_os = "linux")]
fn system_primary_set(text: &str) -> bool {
    if let Some(wayland) = crate::clipboard_wayland::handle() {
        return wayland.copy(crate::clipboard_wayland::WaylandSelection::Primary, text);
    }
    crate::clipboard_x11::X11Clipboard::get_handle()
        .is_some_and(|c| c.set(crate::clipboard_x11::Sel::Primary, text))
}

/// Read the X11 PRIMARY selection as UTF-8 text, or `None` when empty or off X11.
#[cfg(target_os = "linux")]
pub(crate) fn primary_get() -> Option<String> {
    match route() {
        ClipboardRoute::System => system_primary_get(),
        ClipboardRoute::File(path) => file_paste(&primary_file(&path)),
        #[cfg(test)]
        ClipboardRoute::TestBoard => board_paste(&PRIMARY_STUB),
    }
}

/// [`primary_get`]'s platform arm.
#[cfg(target_os = "linux")]
fn system_primary_get() -> Option<String> {
    if let Some(wayland) = crate::clipboard_wayland::handle() {
        return wayland.paste(crate::clipboard_wayland::WaylandSelection::Primary);
    }
    crate::clipboard_x11::X11Clipboard::get_handle()
        .and_then(|c| c.get(crate::clipboard_x11::Sel::Primary))
}

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    pub(crate) static PRIMARY_STUB: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Non-blocking twin of [`primary_get`].
#[cfg(target_os = "linux")]
pub(crate) fn primary_get_owned() -> Option<String> {
    match route() {
        ClipboardRoute::System => system_primary_get_owned(),
        ClipboardRoute::File(path) => file_paste(&primary_file(&path)),
        #[cfg(test)]
        ClipboardRoute::TestBoard => board_paste(&PRIMARY_STUB),
    }
}

/// [`primary_get_owned`]'s platform arm.
#[cfg(target_os = "linux")]
fn system_primary_get_owned() -> Option<String> {
    if let Some(wayland) = crate::clipboard_wayland::handle() {
        return wayland.paste_owned(crate::clipboard_wayland::WaylandSelection::Primary);
    }
    crate::clipboard_x11::X11Clipboard::get_handle()
        .and_then(|c| c.get_owned(crate::clipboard_x11::Sel::Primary))
}

#[cfg(test)]
#[path = "clipboard_tests.rs"]
mod tests;
