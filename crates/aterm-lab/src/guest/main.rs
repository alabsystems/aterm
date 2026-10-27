// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-lab-guest`: the guest side of `lab mac nux`, copied into a macOS guest and run there by
//! the lane (dev-only: never packed, shipped or run on the Mac that builds it).
//!
//! - `census OUT`: every 250 ms, the on-screen windows (`CGWindowListCopyWindowInfo`: a window's
//!   number, owner name, owner pid, layer and bounds need no Screen Recording grant; its title,
//!   which does, is never read): the first sweep's windows as `base` lines, then each window that
//!   appears (`new`) and goes (`gone`).
//! - `person OUT [--password PW]`: the person at the guest's console. A one-point pointer move
//!   every [`NUDGE`] (a posted HID event, which resets `HIDIdleTime`: each input's line carries
//!   the idle clock read just before and just after it), and in a window of an owner it acts on
//!   ([`ACTS_ON`]): a password field filled with posted keys and Return, a System Settings switch
//!   whose row names aterm turned on, or an allowing button ([`ALLOW`]) pressed, each with a
//!   posted click at its centre. It needs Accessibility (to read the dialogs) and the event-post
//!   grant, which the lane's person base writes for this file's path.
//! - `watch FILE OUT [--kill PID --when-running MASK]`: FILE's text, on one line, each time it
//!   changes (looked at every 50 ms); with `--kill`, PID is sent SIGKILL the first time the
//!   record's `running` stage is one of MASK's bits.
//!
//! Every line starts with its kind and the guest's clock in epoch ms, and is flushed as written;
//! the lane reads them (`mac_nux::read_census`, `read_person`, `read_watch`). Each verb runs until
//! it is killed.

#![deny(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How often `census` and `person` sweep the windows.
const SWEEP: Duration = Duration::from_millis(250);
/// How often `person` moves the pointer: presence is HID input within the last minute.
const NUDGE: Duration = Duration::from_secs(5);
/// How often `watch` reads its file.
const WATCH_EVERY: Duration = Duration::from_millis(50);
/// The owners `person` acts in: the authorization dialog, the consent and Gatekeeper alerts, the
/// Accessibility prompt, the Local Network alert, and System Settings (its switches and sheets).
const ACTS_ON: [&str; 6] = [
    "SecurityAgent",
    "UserNotificationCenter",
    "CoreServicesUIAgent",
    "universalAccessAuthWarn",
    "nehelper",
    "System Settings",
];
/// The buttons `person` presses in a dialog (never in System Settings itself): what a person
/// letting aterm go on presses.
const ALLOW: [&str; 7] = [
    "Allow",
    "OK",
    "Open",
    "Continue",
    "Modify Settings",
    "Unlock",
    "Install Helper",
];
/// How long the same control is left alone after `person` pressed it.
const AGAIN: Duration = Duration::from_secs(3);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(()) => 0,
        Err(why) => {
            eprintln!("aterm-lab-guest: {why}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: aterm-lab-guest census OUT | person OUT [--password PW] | watch FILE OUT [--kill PID --when-running MASK]";
    match args.first().map(String::as_str) {
        Some("census") => census(&Out::open(args.get(1).ok_or(usage)?)?),
        Some("person") => {
            let out = Out::open(args.get(1).ok_or(usage)?)?;
            let password = flag(args, "--password").unwrap_or_default();
            person(&out, &password)
        }
        Some("watch") => {
            let file = args.get(1).ok_or(usage)?;
            let out = Out::open(args.get(2).ok_or(usage)?)?;
            let kill = match (flag(args, "--kill"), flag(args, "--when-running")) {
                (Some(pid), Some(bit)) => Some((
                    pid.parse::<i32>()
                        .map_err(|_| format!("--kill {pid}: no pid"))?,
                    bit.parse::<u64>()
                        .map_err(|_| format!("--when-running {bit}: no stage mask"))?,
                )),
                (None, None) => None,
                _ => return Err("--kill and --when-running go together".into()),
            };
            watch(file, &out, kill)
        }
        _ => Err(usage.into()),
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let at = args.iter().position(|a| a == name)?;
    args.get(at + 1).cloned()
}

/// The guest's clock, in epoch ms.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// A record file, each line flushed as written.
struct Out(std::sync::Mutex<std::fs::File>);

impl Out {
    fn open(path: &str) -> Result<Out, String> {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map(|f| Out(std::sync::Mutex::new(f)))
            .map_err(|e| format!("{path}: {e}"))
    }

    fn line(&self, kind: &str, rest: &str) {
        let mut f = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writeln!(f, "{kind} {} {rest}", now_ms());
        let _ = f.flush();
    }
}

/// One on-screen window, as the window list names it.
#[derive(Clone, Debug)]
struct Window {
    id: u64,
    pid: i32,
    layer: i64,
    owner: String,
    frame: (f64, f64, f64, f64),
}

impl Window {
    fn describe(&self) -> String {
        format!(
            "{} {} {} {:.0}x{:.0} {}",
            self.id, self.pid, self.layer, self.frame.2, self.frame.3, self.owner
        )
    }
}

fn census(out: &Out) -> Result<(), String> {
    let Some(first) = cg::on_screen() else {
        out.line(
            "census-error",
            "the window list is unreadable: no window server session",
        );
        return Err("no window list".into());
    };
    out.line(
        "census",
        &format!("every={} windows={}", SWEEP.as_millis(), first.len()),
    );
    let mut known: BTreeSet<u64> = BTreeSet::new();
    for w in &first {
        out.line("base", &w.describe());
        known.insert(w.id);
    }
    loop {
        std::thread::sleep(SWEEP);
        let Some(now) = cg::on_screen() else {
            continue;
        };
        let ids: BTreeSet<u64> = now.iter().map(|w| w.id).collect();
        for w in now.iter().filter(|w| !known.contains(&w.id)) {
            out.line("new", &w.describe());
        }
        for gone in known.difference(&ids) {
            out.line("gone", &gone.to_string());
        }
        known = ids;
    }
}

/// `HIDIdleTime` (ns since the last HID input) as `ioreg` reads it, `None` unreadable.
fn hid_idle_ns() -> Option<u64> {
    let o = aterm_uds::spawnfd::command("/usr/sbin/ioreg")
        .args(["-c", "IOHIDSystem", "-r", "-d", "1", "-k", "HIDIdleTime"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("\"HIDIdleTime\" = "))
        .and_then(|n| n.trim().parse().ok())
}

/// Post `input` with the idle clock read around it: an `input` line.
fn proven(out: &Out, what: &str, input: impl FnOnce()) {
    let before = hid_idle_ns();
    input();
    std::thread::sleep(Duration::from_millis(20));
    let after = hid_idle_ns();
    let ns = |v: Option<u64>| v.map_or_else(|| "?".to_owned(), |n| n.to_string());
    out.line(
        "input",
        &format!(
            "{what} idle_before_ns={} idle_after_ns={}",
            ns(before),
            ns(after)
        ),
    );
}

fn person(out: &Out, password: &str) -> Result<(), String> {
    out.line(
        "person",
        &format!(
            "trusted={} post={}",
            if ax::trusted() { "yes" } else { "no" },
            if cg::may_post() { "yes" } else { "no" }
        ),
    );
    let mut last_nudge: Option<Instant> = None;
    let mut step = 1.0;
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    let mut pressed: BTreeMap<String, Instant> = BTreeMap::new();
    let mut typed: BTreeSet<String> = BTreeSet::new();
    loop {
        if last_nudge.is_none_or(|t| t.elapsed() >= NUDGE) {
            proven(out, "nudge", || cg::nudge(step));
            step = -step;
            last_nudge = Some(Instant::now());
        }
        let windows = cg::on_screen().unwrap_or_default();
        let ids: BTreeSet<u64> = windows.iter().map(|w| w.id).collect();
        seen.retain(|id| {
            let stays = ids.contains(id);
            if !stays {
                out.line("gone", &id.to_string());
            }
            stays
        });
        // Each app acted in, its owner name and its windows' numbers (the key a typed password
        // is kept under, so a new dialog of the same app is typed into again).
        let mut apps: BTreeMap<i32, (String, Vec<u64>)> = BTreeMap::new();
        for w in windows
            .iter()
            .filter(|w| ACTS_ON.contains(&w.owner.as_str()))
        {
            if seen.insert(w.id) {
                out.line("seen", &format!("{} {}", w.id, w.owner));
            }
            let app = apps
                .entry(w.pid)
                .or_insert_with(|| (w.owner.clone(), Vec::new()));
            app.1.push(w.id);
        }
        let keys: BTreeSet<String> = apps
            .iter()
            .map(|(pid, (_, ids))| format!("{pid}:{ids:?}"))
            .collect();
        typed.retain(|k| keys.contains(k));
        for (pid, (owner, ids)) in &apps {
            let key = format!("{pid}:{ids:?}");
            act(out, *pid, owner, password, &mut pressed, &mut typed, &key);
        }
        std::thread::sleep(SWEEP);
    }
}

/// One pass over `pid`'s dialogs: the first thing a person would do there, done.
fn act(
    out: &Out,
    pid: i32,
    owner: &str,
    password: &str,
    pressed: &mut BTreeMap<String, Instant>,
    typed: &mut BTreeSet<String>,
    windows: &str,
) {
    let nodes = ax::nodes(pid);
    let fresh = |key: &str, pressed: &BTreeMap<String, Instant>| {
        pressed.get(key).is_none_or(|t| t.elapsed() >= AGAIN)
    };
    if !password.is_empty()
        && !typed.contains(windows)
        && let Some(field) = nodes.iter().find(|n| n.secure)
    {
        proven(out, "click", || cg::click(field.centre()));
        proven(out, "key", || cg::type_text(password));
        proven(out, "key", cg::press_return);
        out.line(
            "typed",
            &format!("{pid} {owner} {}", password.chars().count()),
        );
        typed.insert(windows.to_owned());
        return;
    }
    if owner == "System Settings" {
        for switch in nodes
            .iter()
            .filter(|n| n.role == "AXCheckBox" && n.value == Some(0))
        {
            let names_aterm = switch.text.to_lowercase().contains("aterm")
                || nodes
                    .iter()
                    .any(|n| n.parent == switch.parent && n.text.to_lowercase().contains("aterm"));
            let key = format!("{pid} switch {:.0},{:.0}", switch.frame.0, switch.frame.1);
            if names_aterm && fresh(&key, pressed) {
                proven(out, "click", || cg::click(switch.centre()));
                out.line(
                    "click",
                    &format!("{pid} {owner} switch:{}", one_line(&switch.text)),
                );
                pressed.insert(key, Instant::now());
                return;
            }
        }
        return;
    }
    for name in ALLOW {
        let Some(button) = nodes
            .iter()
            .find(|n| n.role == "AXButton" && n.text == name && n.enabled)
        else {
            continue;
        };
        let key = format!("{pid} {name}");
        if fresh(&key, pressed) {
            proven(out, "click", || cg::click(button.centre()));
            out.line("click", &format!("{pid} {owner} {}", one_line(name)));
            pressed.insert(key, Instant::now());
        }
        return;
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join("_")
}

fn watch(file: &str, out: &Out, mut kill: Option<(i32, u64)>) -> Result<(), String> {
    // `None` before the first look, so the first text (or its absence) is written too.
    let mut last: Option<Option<String>> = None;
    loop {
        let text = std::fs::read_to_string(file).ok();
        if last.as_ref() != Some(&text) {
            match &text {
                Some(t) => out.line("at", &t.split_whitespace().collect::<Vec<_>>().join(" ")),
                None => out.line("at", "absent"),
            }
            if let (Some((pid, mask)), Some(t)) = (kill, &text) {
                let running = aterm_json::from_str::<aterm_json::Value>(t)
                    .ok()
                    .and_then(|v| v.get("running").and_then(aterm_json::Value::as_u64));
                if let Some(bit) = running.filter(|r| r & mask != 0) {
                    // SAFETY: kill(2) takes any pid and signal number; a gone pid is ESRCH.
                    #[expect(unsafe_code, reason = "kill(2): the one way to send SIGKILL")]
                    let sent = unsafe { libc::kill(pid, libc::SIGKILL) } == 0;
                    out.line(
                        "killed",
                        &format!(
                            "{pid} running={bit} sent={}",
                            if sent { "yes" } else { "no" }
                        ),
                    );
                    kill = None;
                }
            }
            last = Some(text);
        }
        std::thread::sleep(WATCH_EVERY);
    }
}

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "CoreFoundation and CoreGraphics calls: the window list and posted HID events"
)]
mod cg {
    //! The window list and posted HID events (CoreGraphics), and the CoreFoundation reads they
    //! take.

    use std::ffi::{c_char, c_void};

    use super::Window;

    pub(crate) type CfRef = *const c_void;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub(crate) struct Point {
        pub x: f64,
        pub y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub(crate) struct Size {
        pub w: f64,
        pub h: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        origin: Point,
        size: Size,
    }

    const UTF8: u32 = 0x0800_0100;
    const SINT64: isize = 4;
    const ON_SCREEN_ONLY: u32 = 1;
    const EXCLUDE_DESKTOP: u32 = 1 << 4;
    const HID_TAP: u32 = 0;
    const MOUSE_MOVED: u32 = 5;
    const LEFT_DOWN: u32 = 1;
    const LEFT_UP: u32 = 2;
    const LEFT: u32 = 0;
    const RETURN_KEY: u16 = 36;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: CfRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CfRef, at: isize) -> CfRef;
        fn CFDictionaryGetValue(dict: CfRef, key: CfRef) -> CfRef;
        fn CFStringGetCString(s: CfRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFStringCreateWithCString(alloc: CfRef, s: *const c_char, encoding: u32) -> CfRef;
        fn CFNumberGetValue(n: CfRef, kind: isize, out: *mut c_void) -> u8;
        fn CFGetTypeID(v: CfRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFArrayGetTypeID() -> usize;
        fn CFBooleanGetValue(b: CfRef) -> u8;
        fn CFRelease(v: CfRef);
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        static kCGWindowNumber: CfRef;
        static kCGWindowOwnerName: CfRef;
        static kCGWindowOwnerPID: CfRef;
        static kCGWindowLayer: CfRef;
        static kCGWindowBounds: CfRef;
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CfRef;
        fn CGRectMakeWithDictionaryRepresentation(dict: CfRef, rect: *mut Rect) -> bool;
        fn CGEventCreate(source: CfRef) -> CfRef;
        fn CGEventGetLocation(event: CfRef) -> Point;
        fn CGEventCreateMouseEvent(source: CfRef, kind: u32, at: Point, button: u32) -> CfRef;
        fn CGEventCreateKeyboardEvent(source: CfRef, key: u16, down: bool) -> CfRef;
        fn CGEventKeyboardSetUnicodeString(event: CfRef, len: usize, text: *const u16);
        fn CGEventPost(tap: u32, event: CfRef);
        fn CGPreflightPostEventAccess() -> bool;
    }

    /// A CoreFoundation object this code owns (a Create or Copy result), released once.
    pub(crate) struct Owned(pub CfRef);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: a non-null Create/Copy result, released exactly once here.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    /// A CFString of `text` (owned).
    pub(crate) fn string(text: &str) -> Owned {
        let c = std::ffi::CString::new(text).unwrap_or_default();
        // SAFETY: a NUL-terminated UTF-8 string; the result is owned by the caller.
        Owned(unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) })
    }

    /// `v` as text when it is a CFString.
    pub(crate) fn text(v: CfRef) -> Option<String> {
        // SAFETY: CFGetTypeID takes any CF object; a null one is refused first.
        if v.is_null() || unsafe { CFGetTypeID(v) != CFStringGetTypeID() } {
            return None;
        }
        let mut buf = vec![0 as c_char; 1024];
        // SAFETY: a CFString and a buffer of the length passed.
        let ok = unsafe { CFStringGetCString(v, buf.as_mut_ptr(), 1024, UTF8) } != 0;
        ok.then(|| {
            // SAFETY: CFStringGetCString wrote a NUL-terminated string into buf.
            unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        })
    }

    /// `v` as a whole number when it is a CFNumber or CFBoolean.
    pub(crate) fn number(v: CfRef) -> Option<i64> {
        if v.is_null() {
            return None;
        }
        // SAFETY: CFGetTypeID takes any CF object.
        let kind = unsafe { CFGetTypeID(v) };
        // SAFETY: type ids are plain reads.
        if kind == unsafe { CFBooleanGetTypeID() } {
            // SAFETY: a CFBoolean.
            return Some(i64::from(unsafe { CFBooleanGetValue(v) }));
        }
        // SAFETY: a type id read.
        if kind != unsafe { CFNumberGetTypeID() } {
            return None;
        }
        let mut n: i64 = 0;
        // SAFETY: a CFNumber read into an i64, the kind named.
        let ok = unsafe { CFNumberGetValue(v, SINT64, (&raw mut n).cast()) } != 0;
        ok.then_some(n)
    }

    /// `v`'s items when it is a CFArray (borrowed from it).
    pub(crate) fn items(v: CfRef) -> Vec<CfRef> {
        // SAFETY: CFGetTypeID takes any CF object; a null one is refused first.
        if v.is_null() || unsafe { CFGetTypeID(v) != CFArrayGetTypeID() } {
            return Vec::new();
        }
        // SAFETY: a CFArray, read within its count.
        let n = unsafe { CFArrayGetCount(v) };
        // SAFETY: as above.
        (0..n)
            .map(|i| unsafe { CFArrayGetValueAtIndex(v, i) })
            .collect()
    }

    /// The on-screen windows, `None` when the window server gives no list (no session).
    pub(crate) fn on_screen() -> Option<Vec<Window>> {
        // SAFETY: a Copy call; its result is owned and released by `Owned`.
        let list =
            Owned(unsafe { CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0) });
        if list.0.is_null() {
            return None;
        }
        Some(
            items(list.0)
                .into_iter()
                .filter_map(|dict| {
                    // SAFETY: the keys are CoreGraphics' own constants; each value is borrowed
                    // from the dictionary, which the list holds.
                    let get = |key: CfRef| unsafe { CFDictionaryGetValue(dict, key) };
                    // SAFETY: reads of CoreGraphics' key constants.
                    let (k_number, k_owner, k_pid, k_layer, k_bounds) = unsafe {
                        (
                            kCGWindowNumber,
                            kCGWindowOwnerName,
                            kCGWindowOwnerPID,
                            kCGWindowLayer,
                            kCGWindowBounds,
                        )
                    };
                    let mut rect = Rect::default();
                    let b = get(k_bounds);
                    // SAFETY: a bounds dictionary (or null, refused) and a Rect to fill.
                    let framed = !b.is_null()
                        && unsafe { CGRectMakeWithDictionaryRepresentation(b, &raw mut rect) };
                    Some(Window {
                        id: u64::try_from(number(get(k_number))?).ok()?,
                        pid: i32::try_from(number(get(k_pid))?).ok()?,
                        layer: number(get(k_layer)).unwrap_or(0),
                        owner: text(get(k_owner)).unwrap_or_else(|| "?".to_owned()),
                        frame: if framed {
                            (rect.origin.x, rect.origin.y, rect.size.w, rect.size.h)
                        } else {
                            (0.0, 0.0, 0.0, 0.0)
                        },
                    })
                })
                .collect(),
        )
    }

    /// Whether this process may post events (the event-post grant).
    pub(crate) fn may_post() -> bool {
        // SAFETY: a plain query.
        unsafe { CGPreflightPostEventAccess() }
    }

    fn post(event: &Owned) {
        if !event.0.is_null() {
            // SAFETY: an event this code created.
            unsafe { CGEventPost(HID_TAP, event.0) };
        }
    }

    fn pointer() -> Point {
        // SAFETY: a fresh event, owned and released; its location read.
        let e = Owned(unsafe { CGEventCreate(std::ptr::null()) });
        if e.0.is_null() {
            return Point::default();
        }
        // SAFETY: an event this code created.
        unsafe { CGEventGetLocation(e.0) }
    }

    fn mouse(kind: u32, at: Point) {
        // SAFETY: a fresh event, owned, posted and released.
        post(&Owned(unsafe {
            CGEventCreateMouseEvent(std::ptr::null(), kind, at, LEFT)
        }));
    }

    /// Move the pointer `dx` points sideways.
    pub(crate) fn nudge(dx: f64) {
        let at = pointer();
        mouse(
            MOUSE_MOVED,
            Point {
                x: at.x + dx,
                y: at.y,
            },
        );
    }

    /// Move to `at` and click there.
    pub(crate) fn click(at: Point) {
        mouse(MOUSE_MOVED, at);
        std::thread::sleep(std::time::Duration::from_millis(30));
        mouse(LEFT_DOWN, at);
        std::thread::sleep(std::time::Duration::from_millis(30));
        mouse(LEFT_UP, at);
    }

    fn key(code: u16, text: Option<u16>) {
        for down in [true, false] {
            // SAFETY: a fresh event, owned, posted and released; the text is one UTF-16 unit.
            let e = Owned(unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), code, down) });
            if let Some(unit) = text
                && !e.0.is_null()
            {
                // SAFETY: an event this code created and one UTF-16 unit.
                unsafe { CGEventKeyboardSetUnicodeString(e.0, 1, &raw const unit) };
            }
            post(&e);
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }

    /// Type `text` as posted key presses.
    pub(crate) fn type_text(text: &str) {
        for unit in text.encode_utf16() {
            key(0, Some(unit));
        }
    }

    pub(crate) fn press_return() {
        key(RETURN_KEY, None);
    }
}

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "the Accessibility API: reading another app's dialogs"
)]
mod ax {
    //! Another app's controls, read through the Accessibility API.

    use std::ffi::c_void;

    use super::cg::{self, CfRef, Owned, Point, Size};

    const POINT: u32 = 1;
    const SIZE: u32 = 2;
    /// How deep a window's tree is read.
    const DEPTH: usize = 12;
    /// The most controls read from one app in a pass.
    const MOST: usize = 4000;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXUIElementCreateApplication(pid: i32) -> CfRef;
        fn AXUIElementCopyAttributeValue(el: CfRef, attr: CfRef, out: *mut CfRef) -> i32;
        fn AXUIElementSetMessagingTimeout(el: CfRef, secs: f32) -> i32;
        fn AXValueGetValue(v: CfRef, kind: u32, out: *mut c_void) -> bool;
    }

    /// One control of a dialog.
    pub(crate) struct Node {
        pub role: String,
        /// Its title, description and value text, joined.
        pub text: String,
        pub value: Option<i64>,
        pub enabled: bool,
        pub secure: bool,
        /// The index of its parent in the pass's list.
        pub parent: usize,
        pub frame: (f64, f64, f64, f64),
    }

    impl Node {
        pub(crate) fn centre(&self) -> Point {
            Point {
                x: self.frame.0 + self.frame.2 / 2.0,
                y: self.frame.1 + self.frame.3 / 2.0,
            }
        }
    }

    pub(crate) fn trusted() -> bool {
        // SAFETY: a plain query.
        unsafe { AXIsProcessTrusted() }
    }

    fn attr(el: CfRef, name: &str) -> Owned {
        let key = cg::string(name);
        let mut out: CfRef = std::ptr::null();
        // SAFETY: an AX element, an attribute name and an out pointer; the value is owned.
        let err = unsafe { AXUIElementCopyAttributeValue(el, key.0, &raw mut out) };
        Owned(if err == 0 { out } else { std::ptr::null() })
    }

    fn frame(el: CfRef) -> (f64, f64, f64, f64) {
        let (mut p, mut s) = (Point::default(), Size::default());
        let at = attr(el, "AXPosition");
        let size = attr(el, "AXSize");
        // SAFETY: AXValues of the kinds named (or null, refused), read into matching structs.
        unsafe {
            if !at.0.is_null() {
                AXValueGetValue(at.0, POINT, (&raw mut p).cast());
            }
            if !size.0.is_null() {
                AXValueGetValue(size.0, SIZE, (&raw mut s).cast());
            }
        }
        (p.x, p.y, s.w, s.h)
    }

    /// `pid`'s windows' controls, depth first, each with its parent.
    pub(crate) fn nodes(pid: i32) -> Vec<Node> {
        // SAFETY: an app element for a pid, owned; a short timeout for a busy app.
        let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
        if app.0.is_null() {
            return Vec::new();
        }
        // SAFETY: the element this code created.
        unsafe { AXUIElementSetMessagingTimeout(app.0, 0.5) };
        let windows = attr(app.0, "AXWindows");
        let mut out = Vec::new();
        for w in cg::items(windows.0) {
            walk(w, usize::MAX, 0, &mut out);
        }
        out
    }

    fn walk(el: CfRef, parent: usize, depth: usize, out: &mut Vec<Node>) {
        if depth > DEPTH || out.len() >= MOST {
            return;
        }
        let role = cg::text(attr(el, "AXRole").0).unwrap_or_default();
        let subrole = cg::text(attr(el, "AXSubrole").0).unwrap_or_default();
        let value = attr(el, "AXValue");
        let text = ["AXTitle", "AXDescription"]
            .iter()
            .filter_map(|a| cg::text(attr(el, a).0))
            .chain(cg::text(value.0))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let at = out.len();
        out.push(Node {
            secure: subrole == "AXSecureTextField",
            enabled: cg::number(attr(el, "AXEnabled").0) != Some(0),
            value: cg::number(value.0),
            frame: frame(el),
            role,
            text,
            parent,
        });
        let children = attr(el, "AXChildren");
        for child in cg::items(children.0) {
            walk(child, at, depth + 1, out);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod cg {
    //! Elsewhere there is no window list and nothing is posted.

    use super::Window;

    #[derive(Clone, Copy, Default)]
    pub(crate) struct Point;

    pub(crate) fn on_screen() -> Option<Vec<Window>> {
        None
    }
    pub(crate) fn may_post() -> bool {
        false
    }
    pub(crate) fn nudge(_dx: f64) {}
    pub(crate) fn click(_at: Point) {}
    pub(crate) fn type_text(_text: &str) {}
    pub(crate) fn press_return() {}
}

#[cfg(not(target_os = "macos"))]
mod ax {
    //! Elsewhere no other app's controls are read.

    use super::cg::Point;

    pub(crate) struct Node {
        pub role: String,
        pub text: String,
        pub value: Option<i64>,
        pub enabled: bool,
        pub secure: bool,
        pub parent: usize,
        pub frame: (f64, f64, f64, f64),
    }

    impl Node {
        pub(crate) fn centre(&self) -> Point {
            Point
        }
    }

    pub(crate) fn trusted() -> bool {
        false
    }
    pub(crate) fn nodes(_pid: i32) -> Vec<Node> {
        Vec::new()
    }
}
