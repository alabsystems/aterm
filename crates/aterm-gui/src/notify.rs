// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native desktop notification DELIVERY for OSC 9 / OSC 99 / OSC 777.
//!
//! The engine (`aterm-core`) already PARSES these escapes and fires
//! `set_notification_callback` (OSC 9 / 777, a plain body string) and
//! `set_advanced_notification_callback` (OSC 99 / kitty, a structured
//! [`Notification`] with title + body), gated by the host's
//! `authorize_notifications`. This module is the GUI side that turns those
//! callbacks into real macOS / Windows notifications, with two hard rules
//! borrowed from the OSC 52 clipboard path:
//!
//! 1. **Never block the engine.** The callbacks fire on a tab's PTY reader
//!    thread, under the `Terminal` lock. They MUST NOT spawn a subprocess
//!    there. Instead each callback does a single lock-free, NON-BLOCKING
//!    `mpsc::SyncSender::try_send` of a [`NotifyMsg`] onto a BOUNDED channel (cap
//!    [`NOTIFY_QUEUE_CAP`]) — DROPPING the message when the queue is full, so a
//!    notification flood can never grow the queue unbounded or block the engine;
//!    a dedicated delivery thread ([`spawn_delivery`]) owns the receiver and runs
//!    the (blocking) notifier subprocess off the hot path.
//!
//! 2. **Untrusted content.** A notification body is program output — over SSH it
//!    is fully attacker-controlled. The osascript fallback builds an AppleScript
//!    string literal, so the body is run through [`applescript_escape`] (and
//!    every argument is passed as a real argv entry, never a shell string) to
//!    foreclose AppleScript / shell injection. `terminal-notifier` takes argv
//!    directly, so no escaping is needed on that path. The Windows path writes
//!    title/body into the fixed-size `NOTIFYICONDATAW` WCHAR fields (no
//!    interpreter anywhere), so [`fold_to_utf16`] only folds control bytes and
//!    truncates to the field capacity.
//!
//! **Focus-aware suppression.** The delivery thread reads a shared SUPPRESSION SET
//! the main (UI) thread keeps current: the active-tab focused-pane session id of
//! EVERY focused window. A notification is suppressed ONLY when its originating
//! session is in that set (the user is already looking at it in some focused
//! window). App unfocused (empty set), OR a background tab fired it → delivered.
//! Carrying the SET (not a single `focused` bool + one `active` id) makes this
//! per-window-correct: with two windows, a focused non-front window's active tab
//! suppresses correctly, and the front window's active tab does NOT suppress when
//! the front window is unfocused. At one window the set is `{active}` (focused) or
//! `{}` (unfocused) — byte-identical to the old two-atomic behavior. This matches
//! the iTerm2 / Terminal.app default and keeps background-tab activity visible.

// Real delivery exists on macOS and Windows; elsewhere (Linux) this module is a
// channel-draining stub (`spawn_delivery`), so the real-notification
// helpers/fields are intentionally unused there.
#![cfg_attr(
    not(any(target_os = "macos", windows)),
    allow(dead_code, unused_imports)
)]

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;

/// One pending notification handed from a tab's engine callback to the delivery
/// thread. `session` is the originating tab's id (for focus-aware suppression).
pub struct NotifyMsg {
    /// Originating session/tab id (matched against the active tab to suppress
    /// self-notifications the user is already watching).
    pub session: u64,
    /// Notification title (OSC 99 carries one; OSC 9/777 do not — `None`).
    pub title: Option<String>,
    /// Notification body.
    pub body: String,
}

/// Bound on the notification delivery queue, mirroring the OSC 52 clipboard
/// pipeline's queue-bounding. Each [`NotifyMsg`] is up to ~4 KiB and the delivery
/// thread blocks ~100 ms per notifier subprocess, so an UNBOUNDED channel let a
/// program flooding OSC 9/99/777 grow the queue without bound (memory exhaustion)
/// and spawn one blocking subprocess per message (subprocess spam). A small
/// `sync_channel` cap bounds queue memory to `N·msg`; the producer (the engine
/// callback) `try_send`s and DROPS on `Full` rather than blocking the PTY reader
/// thread. Unlike the clipboard (last-writer-wins, coalesced), notifications are
/// DISTINCT, so the overflow is dropped — never coalesced.
const NOTIFY_QUEUE_CAP: usize = 16;

/// Whether this build has a host that can turn a queued notification into an
/// operating-system notification. Keep Settings' availability projection tied
/// to the same cfg as [`spawn_delivery`] so a channel-draining portability stub
/// can never be presented as working delivery.
#[must_use]
pub(crate) const fn delivery_available() -> bool {
    cfg!(any(target_os = "macos", windows))
}

/// Spawn the single, process-wide notification delivery thread and return the
/// `SyncSender` each tab clones into its engine callbacks. The thread parks on
/// `recv()` (0% idle when no notifications arrive) and exits when every sender
/// is dropped. `suppress` is the live suppression set the UI thread keeps current
/// (the active-tab focused-pane id of every focused window); the thread reads it
/// to apply focus-aware suppression.
#[cfg(any(target_os = "macos", windows))]
pub fn spawn_delivery(
    suppress: Arc<Mutex<HashSet<u64>>>,
    silent: Arc<AtomicBool>,
) -> SyncSender<NotifyMsg> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<NotifyMsg>(NOTIFY_QUEUE_CAP);
    std::thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            // Suppress ONLY when the firing session is the active tab of SOME
            // focused window — the user is already looking at it. App unfocused
            // (empty set) OR a background tab fired it → deliver (a background
            // tab's activity still surfaces, mirroring `App::on_bell`).
            if suppress
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains(&msg.session)
            {
                continue;
            }
            deliver(
                msg.title.as_deref(),
                &msg.body,
                silent.load(Ordering::Acquire),
            );
        }
    });
    tx
}

/// Non-macOS/-Windows stub: drain the channel so senders never block and the
/// workspace builds everywhere. There is no portable native notifier wired up
/// off macOS/Windows.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn spawn_delivery(
    _suppress: Arc<Mutex<HashSet<u64>>>,
    _silent: Arc<AtomicBool>,
) -> SyncSender<NotifyMsg> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<NotifyMsg>(NOTIFY_QUEUE_CAP);
    std::thread::spawn(move || while rx.recv().is_ok() {});
    tx
}

/// Deliver one notification natively. Prefers `terminal-notifier` (click-to-
/// activate, app sender) when installed; otherwise falls back to `osascript`'s
/// `display notification`. Runs ONLY on the delivery thread (blocking is fine
/// there). Best-effort: a missing notifier or a non-zero exit is swallowed.
#[cfg(target_os = "macos")]
pub fn deliver(title: Option<&str>, body: &str, _silent: bool) {
    use std::process::{Command, Stdio};

    let title = title.unwrap_or("aterm");

    // Preferred path: terminal-notifier. Arguments are real argv entries, so the
    // (untrusted) title/body cannot inject. `.status()` both waits (reaping the
    // child) and tells us whether the binary exists — `Err` means not installed,
    // so we fall through to osascript. A non-zero exit means it IS installed but
    // failed; we do NOT double-notify via osascript in that case.
    let mut tn = Command::new("terminal-notifier");
    tn.arg("-title")
        .arg(title)
        .arg("-message")
        .arg(body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // `-sender` claims an identity, so it is read from the RUNNING bundle
    // rather than written as the release channel's literal: a dev build stamped
    // `com.aterm.aterm.dev` would otherwise post notifications that click
    // through to the release copy (design §3.1 blast radius). When there is no
    // bundle to read — a bare `target/debug` binary, or a bundle whose
    // `Info.plist` the consent guard refuses — the flag is OMITTED entirely:
    // terminal-notifier then speaks as itself, which is true, instead of aterm
    // claiming an identity this process does not have.
    if let Some(sender) = crate::control_privacy::running_bundle_id() {
        tn.arg("-sender").arg(sender);
    }
    let tn = tn.status();
    if tn.is_ok() {
        return;
    }

    // Fallback: osascript. The body + title go into an AppleScript string
    // literal, so both are escaped. The script itself is passed as a single
    // `-e` argv entry (never via a shell), closing the shell-injection vector;
    // `applescript_escape` closes the AppleScript-injection vector.
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        applescript_escape(body),
        applescript_escape(title),
    );
    let _ = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Deliver one notification natively on Windows: a `Shell_NotifyIconW`
/// `NIF_INFO` balloon from a transient tray icon owned by a hidden message-only
/// window (on Win10/11 the shell renders it as a toast). Runs ONLY on the
/// delivery thread (the window has thread affinity and blocking is fine there).
/// Best-effort: any Win32 failure is swallowed.
#[cfg(windows)]
pub fn deliver(title: Option<&str>, body: &str, silent: bool) {
    let title = title.unwrap_or("aterm");
    // An empty `szInfo` HIDES the balloon entirely, so a title-only OSC 99
    // (body absent) promotes the title into the body slot.
    let (title, body) = if body.is_empty() {
        ("aterm", title)
    } else {
        (title, body)
    };
    win_balloon::deliver(title, body, silent);
}

/// Non-macOS/-Windows stub.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn deliver(_title: Option<&str>, _body: &str, _silent: bool) {}

/// Windows `NOTIFYICONDATAW.dwInfoFlags` for a normal or serious-mode alert.
/// `0x10` is `NIIF_NOSOUND`; keeping this pure makes the serious-mode sound
/// contract testable on every host, not only Windows CI.
#[cfg(any(test, windows))]
const fn windows_info_flags(silent: bool) -> u32 {
    const NIIF_INFO: u32 = 0x1;
    const NIIF_NOSOUND: u32 = 0x10;
    const NIIF_RESPECT_QUIET_TIME: u32 = 0x80;
    NIIF_INFO | NIIF_RESPECT_QUIET_TIME | if silent { NIIF_NOSOUND } else { 0 }
}

/// Windows balloon/toast delivery over direct `unsafe extern "system"` FFI
/// (user32 + kernel32 + shell32, the same hand-rolled style as
/// `clipboard_win`). A lazily created hidden MESSAGE-ONLY window (parent
/// `HWND_MESSAGE`) owns a TRANSIENT tray icon: each notification is one
/// `NIM_ADD` carrying `NIF_INFO` (the shell shows the balloon — a toast on
/// Win10/11 — the moment the icon appears), a short message-pumping linger so
/// the tray can validate the window while the balloon displays, then
/// `NIM_DELETE` so no permanent tray icon accumulates.
#[cfg(windows)]
mod win_balloon {
    // Win32 ABI names are kept verbatim (PascalCase fields, SCREAMING struct
    // names) so they can be checked against the SDK headers line by line.
    #![allow(non_snake_case, clippy::upper_case_acronyms)]

    use std::cell::OnceCell;
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    /// A Win32 HWND/HICON/HINSTANCE as a plain pointer-sized integer (the
    /// `aterm-pty` FFI convention).
    type HWND = isize;

    /// `CreateWindowExW` parent sentinel for a message-only window.
    const HWND_MESSAGE: HWND = -3;
    /// `PeekMessageW` remove flag.
    const PM_REMOVE: u32 = 0x1;
    /// `Shell_NotifyIconW` verbs.
    const NIM_ADD: u32 = 0x0;
    const NIM_DELETE: u32 = 0x2;
    /// `NOTIFYICONDATAW.uFlags`: which fields are valid.
    const NIF_ICON: u32 = 0x2;
    const NIF_TIP: u32 = 0x4;
    const NIF_INFO: u32 = 0x10;
    /// Stock application icon resource id for `LoadIconW(0, ..)`.
    const IDI_APPLICATION: usize = 32512;
    /// `RegisterClassW` "failure" that is fine: the class already exists.
    const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;

    /// How long the transient icon (and thus the balloon/toast) stays before
    /// `NIM_DELETE`. The shell's default balloon display time is ~5 s; this
    /// covers it, and the bounded queue drops overflow while we linger, so a
    /// flood costs at most `NOTIFY_QUEUE_CAP` lingers.
    const BALLOON_LINGER: Duration = Duration::from_secs(6);

    /// `POINT`.
    #[repr(C)]
    struct POINT {
        x: i32,
        y: i32,
    }

    /// `MSG` — pumped, never inspected.
    #[repr(C)]
    struct MSG {
        hwnd: HWND,
        message: u32,
        wParam: usize,
        lParam: isize,
        time: u32,
        pt: POINT,
    }

    /// `WNDCLASSW` for the hidden window; `DefWindowProcW` handles everything.
    #[repr(C)]
    struct WNDCLASSW {
        style: u32,
        lpfnWndProc: unsafe extern "system" fn(HWND, u32, usize, isize) -> isize,
        cbClsExtra: i32,
        cbWndExtra: i32,
        hInstance: isize,
        hIcon: isize,
        hCursor: isize,
        hbrBackground: isize,
        lpszMenuName: *const u16,
        lpszClassName: *const u16,
    }

    /// `NOTIFYICONDATAW` (Vista+ layout, 976 bytes on x64). `guidItem` is a
    /// `GUID` transcribed as bytes (never set; same size and offset).
    #[repr(C)]
    struct NOTIFYICONDATAW {
        cbSize: u32,
        hWnd: HWND,
        uID: u32,
        uFlags: u32,
        uCallbackMessage: u32,
        hIcon: isize,
        szTip: [u16; 128],
        dwState: u32,
        dwStateMask: u32,
        szInfo: [u16; 256],
        uTimeout: u32,
        szInfoTitle: [u16; 64],
        dwInfoFlags: u32,
        guidItem: [u8; 16],
        hBalloonIcon: isize,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassW(lpWndClass: *const WNDCLASSW) -> u16;
        fn CreateWindowExW(
            dwExStyle: u32,
            lpClassName: *const u16,
            lpWindowName: *const u16,
            dwStyle: u32,
            X: i32,
            Y: i32,
            nWidth: i32,
            nHeight: i32,
            hWndParent: HWND,
            hMenu: isize,
            hInstance: isize,
            lpParam: *const c_void,
        ) -> HWND;
        fn DestroyWindow(hWnd: HWND) -> i32;
        fn DefWindowProcW(hWnd: HWND, Msg: u32, wParam: usize, lParam: isize) -> isize;
        fn LoadIconW(hInstance: isize, lpIconName: *const u16) -> isize;
        fn PeekMessageW(
            lpMsg: *mut MSG,
            hWnd: HWND,
            wMsgFilterMin: u32,
            wMsgFilterMax: u32,
            wRemoveMsg: u32,
        ) -> i32;
        fn TranslateMessage(lpMsg: *const MSG) -> i32;
        fn DispatchMessageW(lpMsg: *const MSG) -> isize;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(lpModuleName: *const u16) -> isize;
        fn GetLastError() -> u32;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn Shell_NotifyIconW(dwMessage: u32, lpData: *mut NOTIFYICONDATAW) -> i32;
    }

    /// The hidden message-only window + shared icon (the exe's embedded aterm
    /// icon, or the stock fallback) the balloons hang off. Windows have THREAD
    /// AFFINITY, so this lives in a `thread_local` — in practice only the
    /// single delivery thread ever touches it.
    struct Notifier {
        hwnd: HWND,
        hicon: isize,
    }

    thread_local! {
        static NOTIFIER: OnceCell<Option<Notifier>> = const { OnceCell::new() };
    }

    /// Show one balloon (blocking this thread for [`BALLOON_LINGER`]); silently
    /// a no-op when the hidden window could not be created.
    pub(super) fn deliver(title: &str, body: &str, silent: bool) {
        NOTIFIER.with(|cell| {
            if let Some(n) = cell.get_or_init(Notifier::create) {
                n.balloon(title, body, silent);
            }
        });
    }

    impl Notifier {
        /// Register the (idempotent) window class and create the hidden
        /// message-only window; `None` on any failure (delivery then stays a
        /// silent drain, matching the pre-Windows-arm behavior).
        fn create() -> Option<Self> {
            let class: Vec<u16> = "aterm_notify_wnd\0".encode_utf16().collect();
            // SAFETY: `class` is a live NUL-terminated UTF-16 buffer for the
            // duration of both calls; the WNDCLASSW points only at it and at
            // `DefWindowProcW`. CreateWindowExW's returned window is owned by
            // this thread and destroyed in Drop.
            unsafe {
                let hinstance = GetModuleHandleW(std::ptr::null());
                let wc = WNDCLASSW {
                    style: 0,
                    lpfnWndProc: DefWindowProcW,
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: hinstance,
                    hIcon: 0,
                    hCursor: 0,
                    hbrBackground: 0,
                    lpszMenuName: std::ptr::null(),
                    lpszClassName: class.as_ptr(),
                };
                if RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                    return None;
                }
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    0,
                    hinstance,
                    std::ptr::null(),
                );
                if hwnd == 0 {
                    return None;
                }
                // The balloon's icon: the aterm icon compiled into THIS exe
                // (resource ordinal 1 — the `1 ICON` crates/aterm/build.rs
                // embeds; the same ordinal the window chrome loads via
                // `Icon::from_resource(1, ..)` in app_window.rs, so the two
                // ends stay coupled by that number). The previous
                // `LoadIconW(0, IDI_APPLICATION)` put the GENERIC Windows
                // program glyph in the notification area for every balloon's
                // 6 s linger. Falls back to that stock icon for a binary with
                // no resource section (dev `cargo run` when the resource
                // compiler was missing — build.rs already printed a banner).
                // Both are SHARED icons (module-resource / system), so
                // neither may be destroyed — no guard, as before.
                let own = LoadIconW(hinstance, 1 as *const u16);
                let hicon = if own != 0 {
                    own
                } else {
                    LoadIconW(0, IDI_APPLICATION as *const u16)
                };
                Some(Self { hwnd, hicon })
            }
        }

        /// One `NIM_ADD`(balloon) → pump → `NIM_DELETE` cycle.
        fn balloon(&self, title: &str, body: &str, silent: bool) {
            // SAFETY: all-zero is a valid NOTIFYICONDATAW (null handles, empty
            // NUL-terminated strings, no flags).
            let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = self.hwnd;
            nid.uID = 1;
            nid.uFlags = NIF_TIP | NIF_INFO;
            if self.hicon != 0 {
                nid.uFlags |= NIF_ICON;
                nid.hIcon = self.hicon;
            }
            super::fold_to_utf16("aterm", &mut nid.szTip);
            super::fold_to_utf16(title, &mut nid.szInfoTitle);
            super::fold_to_utf16(body, &mut nid.szInfo);
            nid.dwInfoFlags = super::windows_info_flags(silent);
            // SAFETY: `nid` is fully initialized and `hwnd` is a live window
            // owned by this thread.
            if unsafe { Shell_NotifyIconW(NIM_ADD, &mut nid) } == 0 {
                return;
            }
            // Keep the icon alive while the balloon shows, PUMPING messages so
            // the tray's liveness pings on our window are answered (an
            // unresponsive owner gets its icon reaped mid-balloon).
            pump_for(BALLOON_LINGER);
            // SAFETY: same `nid` identifies the icon added above.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &mut nid) };
        }
    }

    impl Drop for Notifier {
        fn drop(&mut self) {
            // SAFETY: `hwnd` was created on this thread and is destroyed at
            // most once (Drop runs once; nothing else destroys it).
            unsafe { DestroyWindow(self.hwnd) };
        }
    }

    /// Dispatch pending thread messages, sleeping in short slices, until `dur`
    /// elapses.
    fn pump_for(dur: Duration) {
        let deadline = Instant::now() + dur;
        loop {
            // SAFETY: `msg` is a valid out-param; PeekMessageW fills it before
            // Translate/Dispatch read it.
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                while PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(100)));
        }
    }
}

/// Escape a string for embedding inside an AppleScript double-quoted string
/// literal. Backslash and double-quote are the literal's two metacharacters and
/// are backslash-escaped; newlines/carriage-returns would break the single-line
/// literal and are folded to spaces; any other C0 control byte is also folded to
/// a space (notifications are one-liners — control bytes carry no display value
/// and only invite terminal/AppleScript quirks). Defined platform-independently
/// so it is unit-tested on every target.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn applescript_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' | '\t' => out.push(' '),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Sanitize + truncate a notification string into a fixed-size NUL-terminated
/// UTF-16 field (the Windows `NOTIFYICONDATAW.szInfo`/`szInfoTitle`/`szTip`
/// buffers). Control bytes are folded to spaces exactly as
/// [`applescript_escape`] folds them (no escaping — the bytes land in a plain
/// WCHAR buffer, never an interpreter); the copy stops at `out.len() - 1` units
/// WITHOUT splitting a surrogate pair (truncation is per `char`). Defined
/// platform-independently so it is unit-tested on every target.
#[cfg_attr(not(windows), allow(dead_code))]
fn fold_to_utf16(s: &str, out: &mut [u16]) {
    let Some(cap) = out.len().checked_sub(1) else {
        return;
    };
    let mut len = 0usize;
    for c in s.chars() {
        let c = match c {
            '\n' | '\r' | '\t' => ' ',
            c if (c as u32) < 0x20 => ' ',
            c => c,
        };
        let mut units = [0u16; 2];
        let encoded = c.encode_utf16(&mut units);
        if len + encoded.len() > cap {
            break;
        }
        out[len..len + encoded.len()].copy_from_slice(encoded);
        len += encoded.len();
    }
    out[len] = 0;
}

#[cfg(test)]
mod tests {
    use super::{
        NOTIFY_QUEUE_CAP, NotifyMsg, applescript_escape, fold_to_utf16, windows_info_flags,
    };
    use std::sync::mpsc::TrySendError;

    fn msg(session: u64) -> NotifyMsg {
        NotifyMsg {
            session,
            title: None,
            body: "flood".to_string(),
        }
    }

    /// The bounded delivery channel caps queue memory: with no receiver draining,
    /// exactly `NOTIFY_QUEUE_CAP` `try_send`s fit, and the (N+1)th returns
    /// `Full` WITHOUT blocking — so the engine callback drops the overflow and the
    /// channel can never grow unbounded under an OSC 9/99/777 flood. Regression:
    /// the queue was an UNBOUNDED `channel()` served by one blocking-subprocess
    /// thread, so a flood grew memory without bound / spammed subprocesses.
    #[test]
    fn bounded_channel_drops_on_full_without_blocking() {
        // Hold the receiver so try_send reports `Full` (a dropped rx would report
        // `Disconnected` instead) — this mirrors the live delivery thread parked
        // on recv() while a burst outruns the ~100ms-per-notifier drain.
        let (tx, _rx) = std::sync::mpsc::sync_channel::<NotifyMsg>(NOTIFY_QUEUE_CAP);

        // The first N messages fit in the bounded buffer.
        for i in 0..NOTIFY_QUEUE_CAP {
            tx.try_send(msg(i as u64))
                .expect("the first NOTIFY_QUEUE_CAP messages fit the bounded queue");
        }

        // The (N+1)th try_send does NOT block; it returns Full so the producer
        // drops it — queue memory stays bounded by NOTIFY_QUEUE_CAP.
        assert!(
            matches!(tx.try_send(msg(999)), Err(TrySendError::Full(_))),
            "once the bounded queue is full, try_send must return Full (drop-on-full), \
             never block or grow the queue"
        );

        // Draining one slot re-opens exactly one, confirming the bound holds
        // steady-state (not a one-shot): after one recv, one more try_send fits,
        // then it is Full again.
        let _ = _rx.recv().expect("a queued message drains");
        tx.try_send(msg(1000))
            .expect("one drained slot admits exactly one more message");
        assert!(
            matches!(tx.try_send(msg(1001)), Err(TrySendError::Full(_))),
            "the queue is Full again after refilling the single drained slot"
        );
    }

    #[test]
    fn serious_notifications_set_windows_no_sound_flag() {
        const NIIF_NOSOUND: u32 = 0x10;
        assert_eq!(windows_info_flags(false) & NIIF_NOSOUND, 0);
        assert_eq!(windows_info_flags(true) & NIIF_NOSOUND, NIIF_NOSOUND);
    }

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(applescript_escape("build finished"), "build finished");
        assert_eq!(applescript_escape("café — 100% ✅"), "café — 100% ✅");
    }

    #[test]
    fn quotes_and_backslashes_are_escaped() {
        assert_eq!(applescript_escape(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(applescript_escape(r"a\b"), r"a\\b");
        // A backslash immediately before a quote must not let the quote escape
        // the literal: `\"` -> `\\\"` (escaped backslash, then escaped quote).
        assert_eq!(applescript_escape("\\\""), "\\\\\\\"");
    }

    #[test]
    fn applescript_injection_is_neutralized() {
        // The classic break-out: close the string, run a shell command, reopen.
        // After escaping, the embedded quotes are inert — the whole thing stays
        // one string literal, so nothing executes.
        let attack = r#"" & (do shell script "rm -rf ~") & ""#;
        let escaped = applescript_escape(attack);
        assert!(!escaped.contains('"') || escaped.contains("\\\""));
        // Every double-quote in the output is backslash-escaped.
        let bytes = escaped.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'"' {
                assert!(i > 0 && bytes[i - 1] == b'\\', "unescaped quote at {i}");
            }
        }
    }

    /// Decode a `fold_to_utf16` output buffer back to a String (up to the NUL).
    fn utf16_field(buf: &[u16]) -> String {
        let len = buf.iter().position(|&u| u == 0).unwrap_or(buf.len());
        String::from_utf16(&buf[..len]).expect("fold_to_utf16 emits well-formed UTF-16")
    }

    #[test]
    fn fold_to_utf16_copies_and_nul_terminates() {
        let mut buf = [0xFFFFu16; 16];
        fold_to_utf16("build done ✅", &mut buf);
        assert_eq!(utf16_field(&buf), "build done ✅");
        // Control bytes fold to spaces, same as the AppleScript arm.
        fold_to_utf16("a\r\nb\x07c", &mut buf);
        assert_eq!(utf16_field(&buf), "a  b c");
    }

    #[test]
    fn fold_to_utf16_truncates_to_capacity_reserving_the_nul() {
        let mut buf = [0xFFFFu16; 4];
        fold_to_utf16("abcdef", &mut buf);
        // 3 data units + the terminating NUL fill the 4-unit field exactly.
        assert_eq!(buf, [u16::from(b'a'), u16::from(b'b'), u16::from(b'c'), 0]);
    }

    #[test]
    fn fold_to_utf16_never_splits_a_surrogate_pair() {
        // "a" + 🎉 (2 units) = 3 data units, but only 2 fit: the pair is
        // dropped WHOLE rather than leaving a lone lead surrogate.
        let mut buf = [0xFFFFu16; 3];
        fold_to_utf16("a🎉", &mut buf);
        assert_eq!(buf, [u16::from(b'a'), 0, 0xFFFF]);
        // With room, the pair lands intact.
        let mut buf = [0u16; 8];
        fold_to_utf16("a🎉", &mut buf);
        assert_eq!(utf16_field(&buf), "a🎉");
    }

    #[test]
    fn fold_to_utf16_tolerates_degenerate_buffers() {
        // Zero-length: nothing to write, must not panic.
        fold_to_utf16("abc", &mut []);
        // One unit: only the NUL fits.
        let mut buf = [0xFFFFu16; 1];
        fold_to_utf16("abc", &mut buf);
        assert_eq!(buf, [0]);
    }

    #[test]
    fn control_chars_and_newlines_are_folded() {
        assert_eq!(applescript_escape("line1\nline2"), "line1 line2");
        assert_eq!(applescript_escape("a\r\nb"), "a  b");
        assert_eq!(applescript_escape("tab\there"), "tab here");
        assert_eq!(applescript_escape("bell\x07x"), "bell x");
        assert_eq!(applescript_escape("esc\x1b[0m"), "esc [0m");
        // NUL is folded, not dropped or terminating.
        assert_eq!(applescript_escape("a\0b"), "a b");
    }
}
