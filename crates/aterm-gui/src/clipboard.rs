// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The platform clipboard layer: the general clipboard (`pbcopy`/`pbpaste`) and the X11
//! PRIMARY selection (`primary_set`/`primary_get`), over macOS `NSPasteboard`, the X11 and
//! Wayland backends and the Win32 clipboard. Reached through `crate::control`'s re-exports.
//!
//! Nothing else in the crate touches a platform clipboard: every copy, paste, OSC 52 operation
//! and Windows file-list paste comes through here and asks [`route`] first. The routes: the
//! platform clipboard (the only one a shipped binary has), the development seam's stand-in file
//! (`ATERM_DEBUG_CLIPBOARD_FILE`, so a live test never writes the user's clipboard), and a test
//! build's thread-local board.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Where one clipboard read or write goes. [`route`] picks it and every function
/// in this file obeys it, so the choice is made in exactly one place.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ClipboardRoute {
    /// The platform clipboard: `NSPasteboard`, the X11 / Wayland selections, the
    /// Win32 clipboard.
    System,
    /// `ATERM_DEBUG_CLIPBOARD_FILE=<path>`: the file stands in for CLIPBOARD (and
    /// its `.primary` sibling for PRIMARY); the platform clipboard is untouched.
    File(PathBuf),
    /// A test build's thread-local board ([`PBPASTE_STUB`], and PRIMARY's twin on
    /// Linux): a unit test never reads or writes the developer's clipboard.
    #[cfg(test)]
    TestBoard,
}

/// The routing decision; `seam` is `ATERM_DEBUG_CLIPBOARD_FILE`'s value. Unset
/// keeps the platform clipboard. Any value, even empty (no file: copies fail,
/// reads find nothing), keeps the platform clipboard out of it.
fn route_for(seam: Option<OsString>) -> ClipboardRoute {
    match seam {
        None => ClipboardRoute::System,
        Some(path) => ClipboardRoute::File(PathBuf::from(path)),
    }
}

/// The route every read and write takes. The seam is read per call through
/// `dev_seam!`, so a shipped binary compiles it out and always takes
/// [`ClipboardRoute::System`]. A test build always takes the thread-local board.
fn route() -> ClipboardRoute {
    #[cfg(test)]
    {
        ClipboardRoute::TestBoard
    }
    #[cfg(not(test))]
    {
        route_for(aterm_types::dev_seam!("ATERM_DEBUG_CLIPBOARD_FILE"))
    }
}

/// How much of the stand-in file a read takes (the paste seam's bound, 64 MiB).
const CLIPBOARD_FILE_CAP: u64 = 64 << 20;

/// A clipboard write needs an owner until after the clipboard has closed.
/// Keep acquisition and teardown ordering independent of the platform API so
/// failure paths can be exercised without touching the system clipboard.
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

/// A copy under [`ClipboardRoute::File`]: `path` is overwritten with `text` via a
/// sibling and rename, so a poller never reads a half-written copy (the OSC 52
/// worker and the event loop can both write). `false` when the file cannot be
/// written.
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
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, path).is_ok() {
        return true;
    }
    let _ = std::fs::remove_file(&tmp);
    false
}

/// A read under [`ClipboardRoute::File`]: the file's text (bounded by
/// [`CLIPBOARD_FILE_CAP`]), or `None` when missing, empty or not UTF-8, as
/// [`pbpaste`] maps an empty pasteboard.
fn file_paste(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(CLIPBOARD_FILE_CAP)
        .read_to_string(&mut text)
        .ok()?;
    (!text.is_empty()).then_some(text)
}

/// PRIMARY's stand-in, `<path>.primary`: separate so a drag-select never
/// clobbers an explicit copy (see [`primary_set`]).
#[cfg(any(test, target_os = "linux"))]
fn primary_file(path: &Path) -> PathBuf {
    let mut primary = path.as_os_str().to_owned();
    primary.push(".primary");
    PathBuf::from(primary)
}

// The test build's CLIPBOARD board (`ClipboardRoute::TestBoard`), which `pbcopy` writes and
// `pbpaste`/`pbpaste_owned` read. Thread-local because libtest runs each test on its own thread
// and the paths under test read the clipboard on the calling thread; a worker thread sees its
// own empty board. A plain comment: rustc discards doc comments on macro invocations.
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

/// Place `text` on the clipboard; returns whether it was placed. Shared by the `copy` verb, the
/// GUI copy shortcut, copy-on-select and OSC 52.
///
/// macOS writes the general `NSPasteboard` in-process (no fork/exec on the event loop; `NSString`
/// ⇄ `String` is a direct UTF-8 conversion, so the launch locale does not matter). The OSC 52
/// worker and the `copy` verb call this off the main thread, which `NSPasteboard` string get/set
/// allows.
///
/// On Wayland the event loop does the copy and the compositor may refuse it. Off the loop thread
/// this waits for the loop's verdict, so `aterm ctl copy` reports a dropped copy; on the loop
/// thread (copy shortcut, copy-on-select, which carry the input serial the compositor wants) it
/// cannot wait on itself and answers `true` for an accepted request. The return value is the
/// only report (`tracing` is a no-op facade).
pub(crate) fn pbcopy(text: &str) -> bool {
    match route() {
        ClipboardRoute::System => system_pbcopy(text),
        ClipboardRoute::File(path) => file_copy(&path, text),
        #[cfg(test)]
        ClipboardRoute::TestBoard => board_copy(&PBPASTE_STUB, text),
    }
}

/// [`pbcopy`]'s platform arm: the system clipboard itself.
fn system_pbcopy(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        use aterm_objc::{Id, Sel, autoreleasepool, class, sel};

        use crate::appkit;
        // SAFETY: `+generalPasteboard` is `-(id)`, a valid singleton on any
        // thread; `-clearContents` is `-(NSInteger)` (result discarded) and takes
        // ownership before writing, per the documented protocol;
        // `-setString:forType:` is `-(BOOL)(NSString *, NSPasteboardType)`, the
        // type is an `NSString *`, and both arguments outlive the call.
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
        // A window on Wayland owns its selections through its own seat
        // (`clipboard_wayland`); everything else is the X11 backend.
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

/// `NSPasteboardTypeString`, the UTF-8 plain-text pasteboard type. Linked rather
/// than transcribed: unlike most AppKit constants (header-only, see
/// `appkit::consts`), the `NSPasteboardType` family is an exported
/// `APPKIT_EXTERN NSPasteboardType const`.
#[cfg(target_os = "macos")]
fn pasteboard_type_string() -> aterm_objc::Id {
    // SAFETY: AppKit exports this as an immortal `NSString *` constant; reading
    // the symbol is a load, and the object it names outlives the process.
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSPasteboardTypeString: *mut std::ffi::c_void;
    }
    // SAFETY: as above.
    aterm_objc::Id::from_ptr(unsafe { NSPasteboardTypeString })
}

/// Read the clipboard as UTF-8 text, or `None` when empty or unavailable (so no Paste event
/// fires on an empty clipboard). Used by the GUI paste shortcut and the menu Paste.
pub(crate) fn pbpaste() -> Option<String> {
    match route() {
        ClipboardRoute::System => system_pbpaste(),
        ClipboardRoute::File(path) => file_paste(&path),
        #[cfg(test)]
        ClipboardRoute::TestBoard => board_paste(&PBPASTE_STUB),
    }
}

/// [`pbpaste`]'s platform arm: the system clipboard itself.
fn system_pbpaste() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use aterm_objc::{autoreleasepool, class, sel};

        use crate::appkit;
        // SAFETY: `+generalPasteboard` is `-(id)`, a valid singleton on any
        // thread; `-stringForType:` is `-(NSString *)(NSPasteboardType)` and
        // returns a borrowed string, copied out to Rust inside this pool.
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

/// The file list on the Windows clipboard (`CF_HDROP`, as Explorer's Ctrl+C puts
/// up), for the terminal paste's no-text arm. The stand-in file and test board
/// hold text only, so under either there is no file list.
#[cfg(windows)]
pub(crate) fn pbpaste_paths() -> Option<Vec<String>> {
    match route() {
        ClipboardRoute::System => crate::clipboard_win::get_paths(),
        ClipboardRoute::File(_) => None,
        #[cfg(test)]
        ClipboardRoute::TestBoard => None,
    }
}

/// Non-blocking X11 twin of [`pbpaste`]: the CLIPBOARD text only when this
/// process owns the selection (`X11Clipboard::get_owned`), `None` when a foreign
/// client does. The GUI paste delivers the own-selection case synchronously and
/// offloads only the blocking foreign read. Linux-only; the stand-in file is
/// always instant, so under the seam this answers with it.
#[cfg(target_os = "linux")]
pub(crate) fn pbpaste_owned() -> Option<String> {
    match route() {
        ClipboardRoute::System => system_pbpaste_owned(),
        ClipboardRoute::File(path) => file_paste(&path),
        // The same board as `pbpaste`, answered here so a Linux test never falls
        // to the foreign-owner worker thread.
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
/// Linux-only. Kept distinct from CLIPBOARD ([`pbcopy`]) so a drag-select never
/// clobbers an explicit copy; the seam gives it its own file ([`primary_file`]).
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
///
/// Blocks on a foreign owner (a `ConvertSelection` round-trip bounded at ~1 s),
/// so the middle-click path probes [`primary_get_owned`] first and runs this
/// only on its paste worker (`App::paste_primary_into`).
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

// The test build's PRIMARY board (thread-local, as `PBPASTE_STUB`). Only the own-slot fast
// path (`primary_get_owned`) sees it — the foreign-owner worker runs on another thread with an
// empty board — so a test that set it always takes the synchronous branch.
#[cfg(all(test, target_os = "linux"))]
thread_local! {
    pub(crate) static PRIMARY_STUB: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Non-blocking twin of [`primary_get`]: the PRIMARY text only when this process
/// owns the selection, `None` when a foreign client does. The common case: a
/// drag-select in aterm takes PRIMARY (`finish_selection`), so select-here /
/// paste-here stays on the UI thread.
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
mod tests {
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    use super::{ClipboardRoute, file_copy, file_paste, primary_file, route_for};

    #[test]
    fn a_clipboard_write_keeps_its_owner_until_after_close() {
        #[derive(Default)]
        struct State {
            owner: bool,
            open: bool,
            calls: Vec<&'static str>,
        }
        struct Lease(Rc<RefCell<State>>, bool);
        impl Drop for Lease {
            fn drop(&mut self) {
                let mut state = self.0.borrow_mut();
                if self.1 {
                    assert!(!state.open, "the owner must outlive the open clipboard");
                    state.owner = false;
                    state.calls.push("destroy");
                } else {
                    state.open = false;
                    state.calls.push("close");
                }
            }
        }
        let place = |state: &Rc<RefCell<State>>| {
            let mut state = state.borrow_mut();
            state.calls.push("write");
            state.owner && state.open
        };
        // Negative control: the Win32 NULL-owner sequence opens the clipboard,
        // but cannot transfer data after EmptyClipboard clears its owner.
        let unowned = Rc::new(RefCell::new(State {
            open: true,
            ..State::default()
        }));
        assert!(!place(&unowned));

        for failure in ["owner", "open", "write", "none"] {
            let state = Rc::new(RefCell::new(State::default()));
            let accepted = super::write_with_owner(
                || {
                    state.borrow_mut().calls.push("owner");
                    if failure == "owner" {
                        return None;
                    }
                    state.borrow_mut().owner = true;
                    Some(Lease(state.clone(), true))
                },
                |owner| {
                    assert!(owner.1 && owner.0.borrow().owner);
                    state.borrow_mut().calls.push("open");
                    if failure == "open" {
                        return None;
                    }
                    state.borrow_mut().open = true;
                    Some(Lease(state.clone(), false))
                },
                || place(&state) && failure != "write",
            );
            assert_eq!(accepted, failure == "none", "{failure}");
            let state = state.borrow();
            assert!(!state.owner && !state.open, "no lease retained: {failure}");
            let expected: &[&str] = match failure {
                "owner" => &["owner"],
                "open" => &["owner", "open", "destroy"],
                _ => &["owner", "open", "write", "close", "destroy"],
            };
            assert_eq!(state.calls, expected, "{failure}");
        }
    }

    /// Unset, the seam leaves every clipboard read and write on the platform;
    /// set, its file stands in whatever the path — an empty value included,
    /// which names no file, so a copy fails and a read finds nothing and the
    /// platform clipboard is untouched. PRIMARY rides a sibling file.
    #[test]
    fn the_seam_routes_to_the_system_clipboard_or_its_file() {
        assert_eq!(route_for(None), ClipboardRoute::System);
        for path in ["/scratch/clip.txt", "relative/clip", "clip", ""] {
            assert_eq!(
                route_for(Some(OsString::from(path))),
                ClipboardRoute::File(PathBuf::from(path)),
                "{path:?}"
            );
        }
        assert!(!file_copy(Path::new(""), "x"), "no file to write");
        assert_eq!(file_paste(Path::new("")), None);
        assert_eq!(
            primary_file(Path::new("/scratch/clipboard.txt")),
            PathBuf::from("/scratch/clipboard.txt.primary"),
        );
    }

    /// The stand-in file behaves as a clipboard: a missing file is empty, a copy
    /// overwrites it with the latest text (no temporary left beside it), an empty
    /// copy reads as empty, and a directory that is not there refuses the copy.
    #[test]
    fn the_file_stands_in_for_the_clipboard() {
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let clip = dir.path().join("clipboard.txt");
        assert_eq!(file_paste(&clip), None, "no file yet: an empty clipboard");
        assert!(file_copy(&clip, "first"));
        assert!(file_copy(
            &clip,
            "second — ünïcödé
line two"
        ));
        assert_eq!(
            std::fs::read_to_string(&clip).unwrap(),
            "second — ünïcödé
line two",
            "a copy overwrites with the latest contents"
        );
        assert_eq!(
            file_paste(&clip).as_deref(),
            Some(
                "second — ünïcödé
line two"
            )
        );
        let entries = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(entries, 1, "the write-and-rename leaves no temporary");
        assert!(file_copy(&clip, ""), "an OSC 52 clear is a copy of nothing");
        assert_eq!(file_paste(&clip), None, "and reads back as empty");
        assert!(!file_copy(&dir.path().join("absent/clip.txt"), "x"));
    }
}
