// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The platform CLIPBOARD layer: the general clipboard (`pbcopy`/`pbpaste`) and
//! the X11 PRIMARY selection (`primary_set`/`primary_get`), over macOS
//! NSPasteboard, the native x11rb backend ([`crate::clipboard_x11`]) and the
//! Win32 clipboard ([`crate::clipboard_win`]).
//!
//! Split out of `control_selection.rs` when the selection VERBS moved to the
//! winit-free `aterm-control` crate: these five cannot follow them (they are
//! windowing-platform code with ~15 GUI callers), so they stayed and took the
//! name that actually describes them. Reached through the stable
//! `crate::control::NAME` path.
//!
//! ONE ROUTER. Every read and write here asks [`route`] first, and nothing else in
//! the crate reaches `NSPasteboard`, the X11/Wayland selections or the Win32
//! clipboard: Copy, Copy All and Copy Build Information (the native pages'
//! `AppEffect::Clipboard`), copy-selection and copy-on-select, the tab menu's Copy
//! Session ID / Copy CWD, the `copy` verb, OSC 52's set, clear and query, every
//! paste (the terminal, the find bar, the rename field, middle-click PRIMARY) and
//! the Windows file-list paste all come through a function in this file. A new
//! clipboard caller does too. The routes: the platform clipboard (the only one a
//! shipped binary has), the development seam's stand-in file
//! (`ATERM_DEBUG_CLIPBOARD_FILE`, so a live test of an isolated instance never
//! writes the owner's clipboard), and a test build's thread-local board.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Where one clipboard read or write goes. [`route`] picks it and every function
/// in this file obeys it, so the choice is made in exactly one place.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ClipboardRoute {
    /// The platform clipboard: `NSPasteboard`, the X11 / Wayland selections, the
    /// Win32 clipboard.
    System,
    /// `ATERM_DEBUG_CLIPBOARD_FILE=<path>`: the file stands in for the CLIPBOARD
    /// (and its `.primary` sibling for PRIMARY). The platform clipboard is
    /// neither read nor written.
    File(PathBuf),
    /// A test build's thread-local board ([`PBPASTE_STUB`], and PRIMARY's twin on
    /// Linux): a unit test never reads or writes the developer's clipboard.
    #[cfg(test)]
    TestBoard,
}

/// The routing decision, pure: `seam` is `ATERM_DEBUG_CLIPBOARD_FILE`'s value.
/// Unset keeps the platform clipboard. ANY value keeps the platform clipboard out
/// of it — an empty one names no file, so its copies fail and its reads find
/// nothing — so a live test that sets the seam badly fails its own copies instead
/// of writing the owner's clipboard.
fn route_for(seam: Option<OsString>) -> ClipboardRoute {
    match seam {
        None => ClipboardRoute::System,
        Some(path) => ClipboardRoute::File(PathBuf::from(path)),
    }
}

/// The route every read and write here takes. The seam is read per call (one
/// environment lookup, nothing cached to go stale) through `dev_seam!`, so a
/// shipped binary compiles the read out and always takes [`ClipboardRoute::System`].
/// A test build takes the thread-local board whatever the environment says.
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

/// A copy under [`ClipboardRoute::File`]: `path` is overwritten with `text`, the
/// clipboard as the latest write left it. Written to a sibling and renamed over,
/// so a test polling the file never reads a half-written copy (the OSC 52 worker
/// and the event loop can both be writing). `false` when the file cannot be
/// written, which every caller reports as it reports a refused pasteboard.
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
/// [`CLIPBOARD_FILE_CAP`]), or `None` when it is missing, empty or not UTF-8 — an
/// empty clipboard, exactly as [`pbpaste`] maps an empty pasteboard.
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

/// The X11 PRIMARY selection's stand-in beside the CLIPBOARD's: `<path>.primary`.
/// A separate file, because the platform keeps the two apart on purpose (a
/// drag-select never clobbers an explicit copy — see [`primary_set`]).
#[cfg(any(test, target_os = "linux"))]
fn primary_file(path: &Path) -> PathBuf {
    let mut primary = path.as_os_str().to_owned();
    primary.push(".primary");
    PathBuf::from(primary)
}

// The test build's CLIPBOARD board (see [`ClipboardRoute::TestBoard`]): every
// [`pbcopy`] in a test lands here and every [`pbpaste`] (and its Linux
// `pbpaste_owned` twin) reads it, so a test can make "the clipboard says X"
// deterministic — and a copy a test drives never clobbers the developer's own copy
// buffer, which is machine-global state that every concurrently running clipboard
// test would race. Thread-local, not static, because libtest runs tests on
// separate threads and the paths under test read the clipboard SYNCHRONOUSLY on
// the calling thread — the board is visible to exactly the test that uses it (a
// worker thread's read finds its own, empty board). An empty string reads as an
// empty clipboard. Compiled out of every shipping binary.
// (A plain comment: rustc discards doc comments on macro invocations.)
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

/// Place `text` on the system CLIPBOARD. macOS writes the general `NSPasteboard`
/// IN-PROCESS (a subprocess `pbcopy` would cost a fork/exec + wait — tens of ms —
/// on every Cmd-C / copy-on-select on the winit event-loop thread); Linux/X11
/// takes ownership of the CLIPBOARD selection via the native x11rb backend
/// ([`crate::clipboard_x11`]) — so no external helper (`xclip`/`wl-copy`) is
/// required; Windows goes through the Win32 clipboard
/// ([`crate::clipboard_win`]). Shared by the `copy` verb, the GUI copy shortcut, copy-on-select,
/// and OSC 52. Returns whether the text was placed. (Named `pbcopy` for historical
/// continuity across the stable `crate::control::pbcopy` path.)
///
/// LOCALE: the old subprocess path had to pin `LC_ALL`/`LC_CTYPE` to UTF-8 (a
/// Finder/.app launch hands the process a non-UTF-8 locale and `pbcopy`/`pbpaste`
/// transcode against the C codeset — mojibake). The in-process path has NO
/// locale-sensitive transcode: `NSString` ⇄ Rust `String` is a direct UTF-8
/// conversion, so multibyte text round-trips regardless of the launch locale.
///
/// THREADING: called off the main thread by the OSC 52 worker
/// ([`crate::spawn`]) and the control-server `copy` verb. `NSPasteboard`
/// string get/set from a non-main thread is established AppKit practice
/// (Alacritty/copypasta ship exactly this) and the class is not on Apple's
/// main-thread-only list.
///
/// WHAT THE `bool` MEANS ON WAYLAND. A Wayland copy is done by the event loop, not
/// by the calling thread, and a compositor can REFUSE to hand this client the
/// selection (see `clipboard.rs` in the vendored winit backend). Called off the
/// loop thread — the two callers above — this waits for the loop's verdict and
/// answers with it, so `aterm ctl copy` says `ERR pbcopy failed` for a copy the
/// compositor dropped instead of `OK <bytes>`. Called ON the loop thread (the GUI
/// copy shortcut, copy-on-select) it cannot wait for a thread it is already
/// standing on, so it answers `true` for a request the loop accepted — and those
/// are the two callers a refusal cannot reach, each carrying the focus and the
/// fresh input serial the compositor asks for. (`warn!` is not the reporting
/// channel here: this workspace patches `tracing` to a facade whose macros expand
/// to nothing, so the answer a caller gets is the only report there is.)
///
/// ROUTED ([`route`]): under `ATERM_DEBUG_CLIPBOARD_FILE` the text overwrites
/// that file instead, and in a test build it lands on the thread-local board.
pub(crate) fn pbcopy(text: &str) -> bool {
    match route() {
        ClipboardRoute::System => system_pbcopy(text),
        ClipboardRoute::File(path) => file_copy(&path, text),
        #[cfg(test)]
        ClipboardRoute::TestBoard => board_copy(&PBPASTE_STUB, text),
    }
}

/// [`pbcopy`]'s platform arm: the system CLIPBOARD itself.
fn system_pbcopy(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        use aterm_objc::{Id, Sel, autoreleasepool, class, sel};

        use crate::appkit;
        // SAFETY: `+generalPasteboard` is `-(id)`, a valid singleton on any
        // thread; `-clearContents` is `-(NSInteger)` (the new change count,
        // discarded) and takes ownership of the pasteboard before writing — the
        // documented write protocol; `-setString:forType:` is
        // `-(BOOL)(NSString *, NSPasteboardType)`, where the type IS an
        // `NSString *`, and both arguments outlive the call.
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
                aterm_objc::msg();
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

/// `NSPasteboardTypeString`, the UTF-8 plain-text pasteboard type.
///
/// LINKED, and it is the exception that proves the rule the rest of this port
/// found: nearly every AppKit "constant" aterm reaches is a header-only
/// `static const` or enumerator with no symbol (see `appkit::consts`), but the
/// `NSPasteboardType` family really is `APPKIT_EXTERN NSPasteboardType const`
/// — a genuine exported `NSString *`. So this one is bound, not transcribed;
/// transcribing it would mean inventing the string `"public.utf8-plain-text"`
/// and hoping.
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

/// Read the system CLIPBOARD as UTF-8 text, or `None` when empty / unavailable.
/// macOS reads the general `NSPasteboard` in-process (see [`pbcopy`] for the
/// locale + threading notes); Linux/X11 reads the CLIPBOARD selection via the
/// native x11rb backend. The platform twin of [`pbcopy`], used by the GUI paste
/// shortcut and the menu Paste. An empty pasteboard string maps to `None` so no
/// Paste event fires on an empty clipboard.
///
/// ROUTED ([`route`]): under `ATERM_DEBUG_CLIPBOARD_FILE` it reads that file, and
/// in a test build the thread-local board.
pub(crate) fn pbpaste() -> Option<String> {
    match route() {
        ClipboardRoute::System => system_pbpaste(),
        ClipboardRoute::File(path) => file_paste(&path),
        #[cfg(test)]
        ClipboardRoute::TestBoard => board_paste(&PBPASTE_STUB),
    }
}

/// [`pbpaste`]'s platform arm: the system CLIPBOARD itself.
fn system_pbpaste() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use aterm_objc::{autoreleasepool, class, sel};

        use crate::appkit;
        // SAFETY: `+generalPasteboard` is `-(id)`, a valid singleton on any
        // thread, and `-stringForType:` is `-(NSString *)(NSPasteboardType)`, a
        // read that returns a BORROWED string — copied out to Rust inside this
        // pool, which is why the pool is here rather than at the caller.
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

/// The FILE LIST on the Windows clipboard (Explorer's Ctrl+C puts up `CF_HDROP`),
/// for the terminal paste's no-text arm. Routed like every read here: the
/// stand-in file and the test board hold text only, so under either there is no
/// file list and the platform clipboard is not asked.
#[cfg(windows)]
pub(crate) fn pbpaste_paths() -> Option<Vec<String>> {
    match route() {
        ClipboardRoute::System => crate::clipboard_win::get_paths(),
        ClipboardRoute::File(_) => None,
        #[cfg(test)]
        ClipboardRoute::TestBoard => None,
    }
}

/// The non-blocking twin of [`pbpaste`] for X11: return the CLIPBOARD text only when
/// we OWN the selection (the stored slot, instant — `X11Clipboard::get_owned`),
/// `None` when a FOREIGN client owns it. Lets the GUI paste deliver the own-selection
/// case synchronously and offload only the blocking foreign read off the UI thread.
/// Linux-only: macOS / Windows reads are already in-process, so those paths call
/// [`pbpaste`] directly and never need this fast-path (an unused import off Linux).
/// The stand-in file is always instant, so under the seam this answers with it.
#[cfg(target_os = "linux")]
pub(crate) fn pbpaste_owned() -> Option<String> {
    match route() {
        ClipboardRoute::System => system_pbpaste_owned(),
        ClipboardRoute::File(path) => file_paste(&path),
        // The same board as `pbpaste`: it must answer HERE too, or a Linux test
        // run would fall to the foreign-owner worker thread and lose the board.
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

/// Set the X11 PRIMARY selection (the select-to-copy / middle-click-paste buffer)
/// to `text`. Linux-only — PRIMARY has no macOS or Windows analogue, so nothing
/// calls this elsewhere. Distinct from the CLIPBOARD ([`pbcopy`]) so a drag-select
/// never clobbers an explicit Ctrl+Shift+C copy — which is why the seam gives it
/// its own stand-in file ([`primary_file`]).
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

/// Read the X11 PRIMARY selection as UTF-8 text (the middle-click-paste source), or
/// `None` when empty / off X11.
///
/// BLOCKING on a foreign owner (a `ConvertSelection` round-trip bounded at ~1 s),
/// so the GUI middle-click path must never call this on the UI thread — it probes
/// [`primary_get_owned`] first and runs this only on its paste worker
/// (`App::paste_primary_into`), mirroring the [`pbpaste`]/[`pbpaste_owned`] split.
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

// The PRIMARY twin of `PBPASTE_STUB` (see its comment for the thread-local
// rationale): the test build's PRIMARY board, so a test drives the middle-click
// paste path (`App::paste_primary_into`) with a deterministic PRIMARY selection
// instead of whatever the developer last drag-selected. Visible to the own-slot
// fast path (`primary_get_owned`) only — the foreign-owner worker runs on ANOTHER
// thread, whose own board is empty, so a test that set this always takes the
// synchronous branch. Compiled out of every shipping binary.
#[cfg(all(test, target_os = "linux"))]
thread_local! {
    pub(crate) static PRIMARY_STUB: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// The non-blocking twin of [`primary_get`]: the PRIMARY text only when we OWN the
/// selection (the stored slot, instant), `None` when a FOREIGN client owns it — the
/// same fast-path/offload split [`pbpaste_owned`] gives the CLIPBOARD. This is the
/// COMMON middle-click case: a drag-select in aterm takes PRIMARY ownership
/// (`finish_selection`), so select-here/paste-here never leaves the UI thread.
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
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::{
        ClipboardRoute, PBPASTE_STUB, file_copy, file_paste, pbcopy, pbpaste, primary_file, route,
        route_for,
    };

    /// Unset, the seam leaves every clipboard read and write on the platform.
    #[test]
    fn an_unset_seam_routes_to_the_system_clipboard() {
        assert_eq!(route_for(None), ClipboardRoute::System);
    }

    /// Set, the seam's file stands in for the clipboard, whatever the path.
    #[test]
    fn a_set_seam_routes_to_its_file() {
        for path in ["/scratch/clip.txt", "relative/clip", "clip"] {
            assert_eq!(
                route_for(Some(OsString::from(path))),
                ClipboardRoute::File(PathBuf::from(path)),
            );
        }
    }

    /// An EMPTY value is still set: it names no file, so a copy fails and a read
    /// finds nothing — and the platform clipboard stays out of it.
    #[test]
    fn an_empty_seam_fails_its_copies_instead_of_reaching_the_system() {
        assert_eq!(
            route_for(Some(OsString::new())),
            ClipboardRoute::File(PathBuf::new())
        );
        assert!(!file_copy(Path::new(""), "x"), "no file to write");
        assert_eq!(file_paste(Path::new("")), None);
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

    /// PRIMARY rides its own file beside the CLIPBOARD's.
    #[test]
    fn primary_rides_a_sibling_file() {
        assert_eq!(
            primary_file(Path::new("/scratch/clipboard.txt")),
            PathBuf::from("/scratch/clipboard.txt.primary"),
        );
    }

    /// A test build never reaches the platform clipboard: the route is the
    /// thread-local board, a copy lands there and a paste reads it back.
    #[test]
    fn a_test_build_copies_onto_its_board_not_the_system_clipboard() {
        assert_eq!(route(), ClipboardRoute::TestBoard);
        assert_eq!(pbpaste(), None, "a fresh test thread's board is empty");
        assert!(pbcopy("copied in a test"));
        assert_eq!(
            PBPASTE_STUB.with(|b| b.borrow().clone()).as_deref(),
            Some("copied in a test")
        );
        assert_eq!(pbpaste().as_deref(), Some("copied in a test"));
    }
}
