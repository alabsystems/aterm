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
pub(crate) fn pbcopy(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        use objc2_foundation::NSString;
        // SAFETY: generalPasteboard is a valid singleton on any thread;
        // clearContents/setString:forType: take ownership of the pasteboard
        // before writing (the documented write protocol) and the NSString
        // argument outlives the call.
        unsafe {
            let pb = NSPasteboard::generalPasteboard();
            pb.clearContents();
            pb.setString_forType(&NSString::from_str(text), NSPasteboardTypeString)
        }
    }
    #[cfg(target_os = "linux")]
    {
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

/// Read the system CLIPBOARD as UTF-8 text, or `None` when empty / unavailable.
/// macOS reads the general `NSPasteboard` in-process (see [`pbcopy`] for the
/// locale + threading notes); Linux/X11 reads the CLIPBOARD selection via the
/// native x11rb backend. The platform twin of [`pbcopy`], used by the GUI paste
/// shortcut and the menu Paste. An empty pasteboard string maps to `None` so no
/// Paste event fires on an empty clipboard.
pub(crate) fn pbpaste() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        // SAFETY: generalPasteboard is a valid singleton on any thread and
        // stringForType: only reads it; the returned NSString is retained.
        let s = unsafe {
            NSPasteboard::generalPasteboard()
                .stringForType(NSPasteboardTypeString)?
                .to_string()
        };
        (!s.is_empty()).then_some(s)
    }
    #[cfg(target_os = "linux")]
    {
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

/// The non-blocking twin of [`pbpaste`] for X11: return the CLIPBOARD text only when
/// we OWN the selection (the stored slot, instant — `X11Clipboard::get_owned`),
/// `None` when a FOREIGN client owns it. Lets the GUI paste deliver the own-selection
/// case synchronously and offload only the blocking foreign read off the UI thread.
/// Linux-only: macOS / Windows reads are already in-process, so those paths call
/// [`pbpaste`] directly and never need this fast-path (an unused import off Linux).
#[cfg(target_os = "linux")]
pub(crate) fn pbpaste_owned() -> Option<String> {
    crate::clipboard_x11::X11Clipboard::get_handle()
        .and_then(|c| c.get_owned(crate::clipboard_x11::Sel::Clipboard))
}

/// Set the X11 PRIMARY selection (the select-to-copy / middle-click-paste buffer)
/// to `text`. X11-only — PRIMARY has no macOS/Wayland-headless analogue, so this is
/// a no-op (returns `false`) elsewhere. Distinct from the CLIPBOARD ([`pbcopy`]) so
/// a drag-select never clobbers an explicit Ctrl+Shift+C copy.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn primary_set(text: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::clipboard_x11::X11Clipboard::get_handle()
            .is_some_and(|c| c.set(crate::clipboard_x11::Sel::Primary, text))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = text;
        false
    }
}

/// Read the X11 PRIMARY selection as UTF-8 text (the middle-click-paste source), or
/// `None` when empty / off X11.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn primary_get() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        crate::clipboard_x11::X11Clipboard::get_handle()
            .and_then(|c| c.get(crate::clipboard_x11::Sel::Primary))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
