// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hardware-path key injection, for measuring keystroke latency.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use aterm_window::event::InputOrigin;

/// A parsed hardware-key injection request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HwKeySpec {
    /// macOS virtual keycode (`kVK_*`).
    pub keycode: u16,
    /// `-[NSEvent characters]`: the text the layout produces with modifiers applied.
    pub chars: String,
    /// `-[NSEvent charactersIgnoringModifiers]`.
    pub chars_ignoring: String,
    /// Raw `NSEventModifierFlags` bits.
    pub flags: u64,
    /// How many press/release pairs to post.
    pub count: u32,
    /// Milliseconds between successive presses.
    pub interval_ms: u64,
    /// `shape=press`: build each event from a CoreGraphics keyboard event wrapped by
    /// `+[NSEvent eventWithCGEvent:]`.
    pub press: bool,
}

const FLAG_SHIFT: u64 = 1 << 17;
const FLAG_CONTROL: u64 = 1 << 18;
const FLAG_OPTION: u64 = 1 << 19;
const FLAG_COMMAND: u64 = 1 << 20;
const FLAG_FUNCTION: u64 = 1 << 23;

const MAX_COUNT: u32 = 5000;
const MAX_INTERVAL_MS: u64 = 1000;
const DEFAULT_INTERVAL_MS: u64 = 40;

/// Set while the front window holds acts only a person may take: System State as its active view
/// (an Apply that admits a setup plan as the window's own, a row's `Open …` or `Turn On Sync`;
/// `docs/DESIGN-system-state-2026-10-21.md` §6) or a gateway card (a palette, the Overview or a
/// tab menu: Take Over and Hand Back behind a key). The main thread sets it
/// ([`publish_persons_front`], from `App::publish_hwkey_front` at every park and at `hwkey`'s own
/// hop). `hwkey`'s keys take the person's own unmarked key path, so a burst reads it before every
/// key and stops once it is set ([`run_burst`]).
static PERSONS_FRONT: AtomicBool = AtomicBool::new(false);

/// Every key a burst has posted in this process, counted before the post so the key's dispatch
/// always finds the count moved. System State reads it beside the control clients' gesture count
/// (`App::take_system_state_press`), so a key that reached the page before its burst saw
/// [`PERSONS_FRONT`] is still never a person's press.
static POSTED: AtomicU64 = AtomicU64::new(0);

/// The main thread's word on the front window: `true` while System State or a gateway card is
/// in front there ([`PERSONS_FRONT`]).
pub(crate) fn publish_persons_front(front: bool) {
    PERSONS_FRONT.store(front, Ordering::Release);
}

/// How many keys `hwkey` bursts have posted in this process ([`POSTED`]).
pub(crate) fn posted_keys() -> u64 {
    POSTED.load(Ordering::Acquire)
}

/// `NSEventTypeKeyDown`.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) const NS_EVENT_TYPE_KEY_DOWN: usize = 10;
/// `NSEventTypeKeyUp`.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) const NS_EVENT_TYPE_KEY_UP: usize = 11;

/// `NSEventTypeLeftMouseUp`, a click tests stand in.
#[cfg(test)]
pub(crate) const NS_EVENT_TYPE_LEFT_MOUSE_UP: usize = 2;

/// Whether an `NSEventType` is a key-down or key-up, the kinds a burst posts.
pub(crate) const fn is_key_kind(kind: usize) -> bool {
    matches!(kind, NS_EVENT_TYPE_KEY_DOWN | NS_EVENT_TYPE_KEY_UP)
}

/// Whether an `NSEventType` can choose a native menu or toolbar item: a key (by its key
/// equivalent) or a mouse button's down or up (`NSEventTypeLeftMouseDown` 1 through
/// `NSEventTypeRightMouseUp` 4, `NSEventTypeOtherMouseDown` 25 and `NSEventTypeOtherMouseUp` 26).
#[cfg(target_os = "macos")]
pub(crate) const fn is_chooser_kind(kind: usize) -> bool {
    is_key_kind(kind) || matches!(kind, 1..=4 | 25 | 26)
}

/// One native key, modifier-change or mouse event, as the main thread can tell a burst's or
/// another process's apart from a person's in `-[NSApp currentEvent]`: its `NSEventType`, virtual
/// keycode (0 for a mouse event) and timestamp (seconds since boot), and the process it came from
/// (`kCGEventSourceUnixProcessID`, read by `aterm_cg::event_source_pid`; [`HID_SYSTEM`] for a
/// keyboard's key or a mouse's click).
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NativeInput {
    pub kind: usize,
    pub keycode: u16,
    pub timestamp: f64,
    pub source_pid: i64,
}

/// The source process of an event the HID system delivered from a keyboard or a mouse: the
/// person's hand.
pub(crate) const HID_SYSTEM: i64 = 0;

impl NativeInput {
    /// Whether `seen` is this event: the same kind and key, stamped within [`STAMP_SLACK`]. The
    /// source is not compared: the ledger waits only for keys this process posted.
    fn is(&self, seen: &Self) -> bool {
        self.kind == seen.kind
            && self.keycode == seen.keycode
            && (self.timestamp - seen.timestamp).abs() <= STAMP_SLACK
    }

    /// Whether another process posted it: System Events' `keystroke` or `click`, `CGEventPost`
    /// from a program in a tab (which inherits aterm's Accessibility grant), or a person's own
    /// relay (Screen Sharing, an on-screen keyboard). Its source is neither the HID system nor
    /// this process, so no hand is known to have pressed it here ([`InputOrigin::Relayed`]).
    fn relayed(&self) -> bool {
        self.source_pid != HID_SYSTEM && self.source_pid != i64::from(std::process::id())
    }
}

/// How far apart two stamps of one event may read: a stamp that went through
/// CoreGraphics' integer clock and back (whole nanoseconds, or mach ticks) moves by less.
const STAMP_SLACK: f64 = 1e-6;

/// How many posted events the ledger keeps waiting for the main thread: every key of four
/// longest bursts, down and up. Past this many a burst posts no more keys ([`note_posted`]): a
/// waiting key is never dropped, since one AppKit then dispatched would run as the person's.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
const LEDGER_CAP: usize = 4 * 2 * MAX_COUNT as usize;

/// The key events bursts posted that the main thread has not yet dispatched, in the order they
/// were posted, and the last one it found, so a second look at the event being dispatched finds
/// it again.
#[derive(Debug, Default)]
struct Ledger {
    waiting: std::collections::VecDeque<NativeInput>,
    found: Option<NativeInput>,
}

impl Ledger {
    /// How many more keys it can wait for.
    fn room(&self) -> usize {
        LEDGER_CAP.saturating_sub(self.waiting.len())
    }

    /// Wait for every one of `keys`, or for none of them when there is no room for all: it fails
    /// closed, never dropping a key it waits for.
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    #[must_use]
    fn note(&mut self, keys: &[NativeInput]) -> bool {
        if keys.len() > self.room() {
            return false;
        }
        self.waiting.extend(keys.iter().copied());
        true
    }

    /// Whether `seen`, the event being dispatched, is a posted key. A key found clears every key
    /// of its kind posted before it: one burst posts at a time and notes each key just before its
    /// post (`hwkey::post`), and AppKit dequeues the key-downs (and the key-ups) in the order they
    /// were posted, so each of those left the queue before it unseen (an input method or a modal
    /// panel took it) and would only hold room.
    fn take(&mut self, seen: &NativeInput) -> bool {
        if self.found.is_some_and(|found| found.is(seen)) {
            return true;
        }
        let Some(at) = self.waiting.iter().position(|key| key.is(seen)) else {
            return false;
        };
        self.found = self.waiting.remove(at);
        let mut index = 0;
        self.waiting.retain(|key| {
            let earlier = index < at;
            index += 1;
            !(earlier && key.kind == seen.kind)
        });
        true
    }
}

#[cfg(not(test))]
static LEDGER: std::sync::Mutex<Ledger> = std::sync::Mutex::new(Ledger {
    waiting: std::collections::VecDeque::new(),
    found: None,
});

#[cfg(test)]
thread_local! {
    /// The test build's ledger: one per test thread, where its burst and its dispatch both run.
    static LEDGER: std::cell::RefCell<Ledger> = std::cell::RefCell::new(Ledger::default());
    /// The test build's stand-in for `-[NSApp currentEvent]` ([`current_input`]).
    pub(crate) static CURRENT_INPUT_STUB: std::cell::Cell<Option<NativeInput>> =
        const { std::cell::Cell::new(None) };
    /// The source verdict retained by the window callback being delivered.
    static WINDOW_EVENT_ORIGIN_STUB: std::cell::Cell<Option<InputOrigin>> =
        const { std::cell::Cell::new(None) };
    /// The test build's [`NOTED`]: one per test thread, as the ledger is, so a burst one test
    /// noted never lets another test's classifier consult the ledger.
    static NOTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Deliver a test callback with its retained source verdict, restoring an enclosing callback
/// even if the delivery unwinds. This never queries AppKit.
#[cfg(test)]
pub(crate) fn with_window_event_origin_for_test<R>(
    origin: InputOrigin,
    f: impl FnOnce() -> R,
) -> R {
    struct Restore(Option<InputOrigin>);
    impl Drop for Restore {
        fn drop(&mut self) {
            WINDOW_EVENT_ORIGIN_STUB.with(|current| current.set(self.0));
        }
    }
    let _restore = Restore(WINDOW_EVENT_ORIGIN_STUB.with(|current| current.replace(Some(origin))));
    f()
}

fn with_ledger<R>(f: impl FnOnce(&mut Ledger) -> R) -> R {
    #[cfg(not(test))]
    {
        f(&mut LEDGER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }
    #[cfg(test)]
    {
        LEDGER.with(|ledger| f(&mut ledger.borrow_mut()))
    }
}

/// Set once a burst noted a key in this process: until then no key is a burst's, and classifying
/// a native key need not consult the ledger.
#[cfg(not(test))]
static NOTED: AtomicBool = AtomicBool::new(false);

/// Whether a burst noted a key ([`NOTED`]).
fn noted() -> bool {
    #[cfg(not(test))]
    {
        NOTED.load(Ordering::Acquire)
    }
    #[cfg(test)]
    {
        NOTED.with(std::cell::Cell::get)
    }
}

/// Record that a burst noted a key ([`NOTED`]).
fn set_noted() {
    #[cfg(not(test))]
    NOTED.store(true, Ordering::Release);
    #[cfg(test)]
    NOTED.with(|noted| noted.set(true));
}

/// Record `keys` (one press: its key-down and key-up) as a burst's, before they are posted, or
/// refuse them all, unposted, while the ledger waits for as many keys as it can hold.
///
/// # Errors
///
/// The `ERR hwkey: busy` reply line when the ledger has no room for all of `keys`.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn note_posted(keys: &[NativeInput]) -> Result<(), String> {
    if !with_ledger(|ledger| ledger.note(keys)) {
        return Err(format!(
            "ERR hwkey: busy: {LEDGER_CAP} keys bursts posted are still waiting for aterm to \
             take them, so this burst posts no more\n"
        ));
    }
    set_noted();
    Ok(())
}

/// Held across each press's note and post (`hwkey::post`), so bursts on several control
/// connections post one at a time and the ledger waits for keys in the order AppKit queues them
/// ([`Ledger::take`]).
#[cfg(target_os = "macos")]
static POSTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The key or mouse event AppKit is dispatching now (a test build's stand-in:
/// [`CURRENT_INPUT_STUB`]).
fn current_input() -> Option<NativeInput> {
    #[cfg(test)]
    {
        CURRENT_INPUT_STUB.with(std::cell::Cell::get)
    }
    #[cfg(all(not(test), target_os = "macos"))]
    {
        crate::platform::current_input_event()
    }
    #[cfg(all(not(test), not(target_os = "macos")))]
    {
        None
    }
}

/// Where the native event AppKit is dispatching now came from ([`classify`]): the key that chose
/// a native menu or toolbar item by its key equivalent, or the click that chose it. Native menu
/// and toolbar callbacks can run in a nested loop beneath a window callback from a different
/// event, so their readers consult the current native event rather than that callback's
/// retained source.
pub(crate) fn current_event_origin() -> InputOrigin {
    current_input().map_or(InputOrigin::Person, classify)
}

/// Where this window callback came from ([`classify`]). The native source classifies a key,
/// modifier change, click or scroll before queueing a callback, including text an input method
/// composes or commits. A retained person's verdict also masks an unrelated current event.
/// Outside a retained window callback, a native reader can still classify the event AppKit is
/// dispatching now.
pub(crate) fn window_callback_origin() -> InputOrigin {
    #[cfg(test)]
    let retained = WINDOW_EVENT_ORIGIN_STUB.with(std::cell::Cell::get);
    #[cfg(all(not(test), target_os = "macos"))]
    let retained = aterm_window::platform::macos::window_event_origin();
    #[cfg(any(test, target_os = "macos"))]
    {
        retained.unwrap_or_else(current_event_origin)
    }
    #[cfg(all(not(test), not(target_os = "macos")))]
    {
        current_event_origin()
    }
}

/// Where `seen`, a native event a native monitor holds before AppKit dispatches it, or the one
/// in dispatch ([`current_event_origin`]), came from: a key a burst posted, taken from the
/// ledger once found as a dispatch takes it ([`InputOrigin::Posted`]); one another process
/// posted ([`NativeInput::relayed`], [`InputOrigin::Relayed`]); else the person's hand.
pub(crate) fn classify(seen: NativeInput) -> InputOrigin {
    if is_key_kind(seen.kind) && noted() && with_ledger(|ledger| ledger.take(&seen)) {
        InputOrigin::Posted
    } else if seen.relayed() {
        InputOrigin::Relayed
    } else {
        InputOrigin::Person
    }
}

/// One burst: `count` presses `interval_ms` apart, each posted by `post_one` (told how many this
/// burst posted before it), with the gates read before every key. Once `front` is set, or
/// `confirmation_up` answers yes (one of the person's confirmations is on screen,
/// [`crate::alert_keys::confirmation_up`]), the burst stops with nothing more posted, and its
/// `Err` says how many were. Each key is counted into `posted` before `post_one` runs. A
/// `post_one` error ends the burst with that error.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn run_burst(
    count: u32,
    interval_ms: u64,
    front: &AtomicBool,
    confirmation_up: impl Fn() -> bool,
    posted: &AtomicU64,
    mut post_one: impl FnMut(u32) -> Result<(), String>,
) -> Result<u32, String> {
    let mut sent = 0u32;
    for i in 0..count {
        if i > 0 && interval_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(interval_ms));
        }
        if front.load(Ordering::Acquire) {
            return Err(format!(
                "ERR hwkey: stopped after posted={sent}: System State, a palette, the Overview or \
                 a tab menu came to the front, and its buttons are the person's\n"
            ));
        }
        if confirmation_up() {
            return Err(crate::alert_keys::confirmation_up_stop(sent));
        }
        posted.fetch_add(1, Ordering::AcqRel);
        post_one(sent)?;
        sent += 1;
    }
    Ok(sent)
}

/// Fence the key queue as one of the person's confirmations opens, after its
/// [`crate::alert_keys::ConfirmationUp`] is held: wait out a key a burst is posting now (each
/// press is noted and posted under [`POSTING`], which reads [`crate::alert_keys::confirmation_up`]
/// first), then take every key-down and key-up already queued, so no key posted (or typed) before
/// the question was on screen can answer it. A key a burst tries after this refuses under the
/// same lock. Modifier changes stay queued, so the window's own modifier state stays true.
/// `false` when the queue would not drain (more than 16384 queued keys): the caller then must not
/// let a key answer the question. `true` at once in a process that launched without a window
/// ([`crate::platform::Windowed`]), which has no key queue.
#[cfg(target_os = "macos")]
pub(crate) fn fence_queued_keys(_mtm: crate::appkit::MainThread) -> bool {
    use aterm_objc::{Bool, Id, Sel, class, sel};

    use crate::appkit;

    // A process that launched without a window has no AppKit key queue to drain.
    let Some(_windowed) = crate::platform::Windowed::get() else {
        return true;
    };
    let _order = POSTING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: on the main thread (`_mtm`). `+sharedApplication` is `-(id)` and returns the
    // immortal singleton; `+distantPast` is `-(NSDate *)`, +0 and autoreleased, used within this
    // frame; `kCFRunLoopDefaultMode` is a CFString constant toll-free bridged to NSString;
    // `-nextEventMatchingMask:untilDate:inMode:dequeue:` is
    // `-(NSEvent *)(NSEventMask, NSDate *, NSRunLoopMode, BOOL)`, and a past date makes it return
    // a queued event or nil at once without running the loop's sources.
    unsafe {
        let app = appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
        let past = appkit::send_id(class(c"NSDate").as_id(), sel!(distantPast));
        if app.is_null() || past.is_null() {
            return false;
        }
        let mode = Id::from_ptr(aterm_objc::cf::kCFRunLoopDefaultMode.cast_mut().cast());
        let next: unsafe extern "C-unwind" fn(Id, Sel, usize, Id, Id, Bool) -> Id =
            aterm_objc::checked_msg();
        for _ in 0..16_384 {
            if next(
                app,
                sel!(nextEventMatchingMask:untilDate:inMode:dequeue:),
                (1 << NS_EVENT_TYPE_KEY_DOWN) | (1 << NS_EVENT_TYPE_KEY_UP),
                past,
                mode,
                Bool::YES,
            )
            .is_null()
            {
                return true;
            }
        }
        false
    }
}

const NAMED: &[(&str, u16, &str)] = &[
    ("enter", 36, "\r"),
    ("return", 36, "\r"),
    ("tab", 48, "\t"),
    ("space", 49, " "),
    ("backspace", 51, "\u{7f}"),
    ("delete", 117, "\u{f728}"),
    ("esc", 53, "\u{1b}"),
    ("escape", 53, "\u{1b}"),
    ("up", 126, "\u{f700}"),
    ("down", 125, "\u{f701}"),
    ("left", 123, "\u{f702}"),
    ("right", 124, "\u{f703}"),
    ("home", 115, "\u{f729}"),
    ("end", 119, "\u{f72b}"),
    ("pageup", 116, "\u{f72c}"),
    ("pagedown", 121, "\u{f72d}"),
    ("f1", 122, "\u{f704}"),
    ("f2", 120, "\u{f705}"),
    ("f3", 99, "\u{f706}"),
    ("f4", 118, "\u{f707}"),
    ("f5", 96, "\u{f708}"),
    ("f6", 97, "\u{f709}"),
    ("f7", 98, "\u{f70a}"),
    ("f8", 100, "\u{f70b}"),
    ("f9", 101, "\u{f70c}"),
    ("f10", 109, "\u{f70d}"),
    ("f11", 103, "\u{f70e}"),
    ("f12", 111, "\u{f70f}"),
];

const FUNCTION_FLAGGED: &[u16] = &[
    123, 124, 125, 126, 115, 116, 117, 119, 121, 96, 97, 98, 99, 100, 101, 103, 109, 111, 118, 120,
    122,
];

/// US-QWERTY virtual keycode for a printable ASCII character: `(keycode, needs_shift)`.
#[must_use]
const fn ansi_keycode(c: char) -> Option<(u16, bool)> {
    let (base, shift) = match c {
        'A'..='Z' => (c.to_ascii_lowercase(), true),
        '!' => ('1', true),
        '@' => ('2', true),
        '#' => ('3', true),
        '$' => ('4', true),
        '%' => ('5', true),
        '^' => ('6', true),
        '&' => ('7', true),
        '*' => ('8', true),
        '(' => ('9', true),
        ')' => ('0', true),
        '_' => ('-', true),
        '+' => ('=', true),
        '{' => ('[', true),
        '}' => (']', true),
        '|' => ('\\', true),
        ':' => (';', true),
        '"' => ('\'', true),
        '<' => (',', true),
        '>' => ('.', true),
        '?' => ('/', true),
        '~' => ('`', true),
        other => (other, false),
    };
    let code = match base {
        'a' => 0,
        's' => 1,
        'd' => 2,
        'f' => 3,
        'h' => 4,
        'g' => 5,
        'z' => 6,
        'x' => 7,
        'c' => 8,
        'v' => 9,
        'b' => 11,
        'q' => 12,
        'w' => 13,
        'e' => 14,
        'r' => 15,
        'y' => 16,
        't' => 17,
        '1' => 18,
        '2' => 19,
        '3' => 20,
        '4' => 21,
        '6' => 22,
        '5' => 23,
        '=' => 24,
        '9' => 25,
        '7' => 26,
        '-' => 27,
        '8' => 28,
        '0' => 29,
        ']' => 30,
        'o' => 31,
        'u' => 32,
        '[' => 33,
        'i' => 34,
        'p' => 35,
        'l' => 37,
        'j' => 38,
        '\'' => 39,
        'k' => 40,
        ';' => 41,
        '\\' => 42,
        ',' => 43,
        '/' => 44,
        'n' => 45,
        'm' => 46,
        '.' => 47,
        ' ' => 49,
        '`' => 50,
        _ => return None,
    };
    Some((code, shift))
}

fn take_inline_mods(body: &str) -> (u64, &str) {
    let mut m = 0u64;
    let mut rest = body;
    while let Some(plus) = rest.find('+') {
        let bit = match &rest[..plus] {
            "shift" => FLAG_SHIFT,
            "ctrl" | "control" => FLAG_CONTROL,
            "alt" | "option" => FLAG_OPTION,
            "super" | "cmd" | "command" => FLAG_COMMAND,
            _ => break,
        };
        m |= bit;
        rest = &rest[plus + 1..];
    }
    (m, rest)
}

fn take_tokens(rest: &str) -> Result<(u64, u32, u64, bool, String), String> {
    let mut mods = 0u64;
    let mut count = 1u32;
    let mut interval = DEFAULT_INTERVAL_MS;
    let mut press = false;
    let mut kept: Vec<&str> = Vec::new();
    for tok in rest.split_whitespace() {
        if let Some(shape) = tok.strip_prefix("shape=") {
            press = match shape {
                "press" => true,
                "built" => false,
                other => {
                    return Err(format!(
                        "ERR hwkey: unknown shape `{other}` (press, built)\n"
                    ));
                }
            };
        } else if let Some(list) = tok.strip_prefix("mods=") {
            for name in list.split(['+', ',']) {
                mods |= match name {
                    "shift" => FLAG_SHIFT,
                    "ctrl" | "control" => FLAG_CONTROL,
                    "alt" | "option" => FLAG_OPTION,
                    "super" | "cmd" | "command" => FLAG_COMMAND,
                    "" => 0,
                    other => return Err(format!("ERR hwkey: unknown modifier `{other}`\n")),
                };
            }
        } else if let Some(n) = tok.strip_prefix("count=") {
            count = (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                .then(|| n.parse::<u32>().ok())
                .flatten()
                .ok_or_else(|| format!("ERR hwkey: bad count `{n}`\n"))?;
            if count == 0 || count > MAX_COUNT {
                return Err(format!("ERR hwkey: count must be 1..={MAX_COUNT}\n"));
            }
        } else if let Some(n) = tok.strip_prefix("interval=") {
            interval = (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                .then(|| n.parse::<u64>().ok())
                .flatten()
                .ok_or_else(|| format!("ERR hwkey: bad interval `{n}`\n"))?;
            if interval > MAX_INTERVAL_MS {
                return Err(format!(
                    "ERR hwkey: interval must be 0..={MAX_INTERVAL_MS} ms\n"
                ));
            }
        } else {
            kept.push(tok);
        }
    }
    Ok((mods, count, interval, press, kept.join(" ")))
}

/// Pure parser for `hwkey <name> [mods=<list>] [count=<n>] [interval=<ms>] [shape=press|built]`.
pub(crate) fn parse_hwkey(rest: &str) -> Result<HwKeySpec, String> {
    let (tok_mods, count, interval_ms, press, body) = take_tokens(rest)?;
    let (prefix_mods, body) = take_inline_mods(body.trim());
    let mut flags = tok_mods | prefix_mods;
    let body = body.trim();
    if body.is_empty() {
        return Err(usage());
    }

    let lower = body.to_ascii_lowercase();
    if let Some(&(_, keycode, chars)) = NAMED.iter().find(|(n, _, _)| *n == lower) {
        if FUNCTION_FLAGGED.contains(&keycode) {
            flags |= FLAG_FUNCTION;
        }
        return Ok(HwKeySpec {
            keycode,
            chars: chars.to_string(),
            chars_ignoring: chars.to_string(),
            flags,
            count,
            interval_ms,
            press,
        });
    }

    let mut chars_iter = body.chars();
    let (Some(c), None) = (chars_iter.next(), chars_iter.next()) else {
        return Err(usage());
    };
    let Some((keycode, needs_shift)) = ansi_keycode(c) else {
        return Err(usage());
    };
    if needs_shift {
        flags |= FLAG_SHIFT;
    }
    let ignoring = if flags & FLAG_SHIFT != 0 {
        c.to_ascii_uppercase().to_string()
    } else {
        c.to_ascii_lowercase().to_string()
    };
    let chars = if flags & FLAG_CONTROL != 0 && c.is_ascii_alphabetic() {
        let ctrl = (c.to_ascii_uppercase() as u8) & 0x1f;
        (ctrl as char).to_string()
    } else {
        ignoring.clone()
    };
    Ok(HwKeySpec {
        keycode,
        chars,
        chars_ignoring: ignoring,
        flags,
        count,
        interval_ms,
        press,
    })
}

fn usage() -> String {
    "ERR usage: hwkey [unread=ok] [<mod>+]<char|name> [mods=<list>] [count=<n>] \
     [interval=<ms>] [shape=press|built] — posts a real NSEvent into this app's own event \
     queue, so the key takes the path a physical key press takes through aterm (macOS \
     only); shape=press builds it from a CoreGraphics key event, so the layout gives its \
     characters and the input method may compose it. <char> is one printable ASCII \
     character; names: enter tab esc space backspace delete up down left right home end \
     pageup pagedown f1..f12\n"
        .to_string()
}

/// Post the request as real `NSEvent`s into this application's event queue, returning the number
/// of presses posted. It runs on the control thread, so a key waits in the queue behind a parked
/// main thread as a physical key does.
#[cfg(target_os = "macos")]
pub(crate) fn post(spec: &HwKeySpec, window_number: i64) -> Result<u32, String> {
    use aterm_objc::{Bool, CGPoint, Id, Sel, autoreleasepool, class, sel};

    use crate::appkit;

    if window_number == 0 {
        return Err("ERR hwkey: no frontmost AppKit window to post into\n".to_string());
    }
    if crate::platform::Windowed::get().is_none() {
        return Err("ERR hwkey: a headless instance has no AppKit event queue\n".to_string());
    }
    let (Some(chars), Some(ignoring)) = (
        appkit::nsstring(&spec.chars),
        appkit::nsstring(&spec.chars_ignoring),
    ) else {
        return Err("ERR hwkey: could not build the key strings\n".to_string());
    };
    let flags = spec.flags as usize;

    // SAFETY: `+sharedApplication` is `-(id)`; the caller resolved a live window number on the
    // main thread first, so `NSApp` exists and this returns the singleton rather than
    // constructing it off-main. The runtime retains it for the process lifetime; borrowed here.
    let app = unsafe { appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) };
    if app.is_null() {
        return Err("ERR hwkey: no shared NSApplication\n".to_string());
    }

    // The pacing, the count and the gate before every key are `run_burst`'s.
    run_burst(
        spec.count,
        spec.interval_ms,
        &PERSONS_FRONT,
        crate::alert_keys::confirmation_up,
        &POSTED,
        |sent| {
            let _posting = crate::askpass::PROMPT_INPUT
                .posting_permit()
                .map_err(|()| "ERR hwkey: a sudo password sheet is up\n".to_string())?;
            autoreleasepool(|_| {
                // SAFETY: `+processInfo` is `-(id)`, an immortal singleton, and `-systemUptime` is
                // `-(NSTimeInterval)`, a plain scalar getter.
                let ts = unsafe {
                    let info = appkit::send_id(class(c"NSProcessInfo").as_id(), sel!(processInfo));
                    if info.is_null() {
                        return Err("ERR hwkey: no NSProcessInfo\n".to_string());
                    }
                    appkit::send_f64(info, sel!(systemUptime))
                };
                // SAFETY: the AppKit event factory with a non-nil characters pair (winit
                // dereferences both unconditionally) and a nil graphics context, exactly as
                // upstream winit's `replace_event` builds a synthetic key event.
                let built = |kind: usize| unsafe {
                    let make: unsafe extern "C-unwind" fn(
                        Id,
                        Sel,
                        usize,
                        CGPoint,
                        usize,
                        f64,
                        isize,
                        Id,
                        Id,
                        Id,
                        Bool,
                        u16,
                    ) -> Id = aterm_objc::checked_msg();
                    make(
                        class(c"NSEvent").as_id(),
                        sel!(
                            keyEventWithType:location:modifierFlags:timestamp:windowNumber:
                            context:characters:charactersIgnoringModifiers:isARepeat:keyCode:
                        ),
                        kind,
                        CGPoint { x: 0.0, y: 0.0 },
                        flags,
                        ts,
                        window_number as isize,
                        Id::NIL,
                        chars.id(),
                        ignoring.id(),
                        Bool::NO,
                        spec.keycode,
                    )
                };
                let key_event = |kind: usize| {
                    if spec.press {
                        press_event(spec.keycode, kind == NS_EVENT_TYPE_KEY_DOWN, spec.flags)
                    } else {
                        built(kind)
                    }
                };
                let down = key_event(NS_EVENT_TYPE_KEY_DOWN);
                if down.is_null() {
                    return Err("ERR hwkey: NSEvent construction failed\n".to_string());
                }
                let up = key_event(NS_EVENT_TYPE_KEY_UP);
                // The main thread tells the key from a person's by these, as it dispatches it
                // (`current_event_origin`), so it is recorded before AppKit can dispatch it.
                // SAFETY: `-type` (`-(NSEventType)`, an `NSUInteger`) and `-timestamp`
                // (`-(NSTimeInterval)`) are scalar getters on a live key event this thread built
                // and has not yet posted.
                let stamp = |event: Id| unsafe {
                    NativeInput {
                        kind: appkit::send_usize(event, sel!(type)),
                        keycode: spec.keycode,
                        timestamp: appkit::send_f64(event, sel!(timestamp)),
                        source_pid: i64::from(std::process::id()),
                    }
                };
                let press: Vec<Id> = [down, up].into_iter().filter(|e| !e.is_null()).collect();
                let keys: Vec<NativeInput> = press.iter().map(|&event| stamp(event)).collect();
                let _order = POSTING
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                // Read again under the lock a confirmation's fence takes (`fence_queued_keys`):
                // a question that opened since the gate above has drained the queue, and this
                // key would land behind its fence.
                if crate::alert_keys::confirmation_up() {
                    return Err(crate::alert_keys::confirmation_up_stop(sent));
                }
                note_posted(&keys)?;
                // SAFETY: `-postEvent:atStart:` is `-(void)(NSEvent *, BOOL)` on the shared
                // application with a live event; AppKit documents it as safe from any thread, and
                // this runs on the control thread.
                unsafe {
                    let post: unsafe extern "C-unwind" fn(Id, Sel, Id, Bool) =
                        aterm_objc::checked_msg();
                    for event in press {
                        post(app, sel!(postEvent:atStart:), event, Bool::NO);
                    }
                }
                Ok(())
            })
        },
    )
}

/// A key-down (`down`) or key-up `NSEvent` built as a key press is: a CoreGraphics
/// keyboard event for `keycode` with `flags` (`CGEventFlags` share the bit positions of
/// `NSEventModifierFlags`), wrapped by `+[NSEvent eventWithCGEvent:]`, so its
/// `characters` are the current layout's for that key and modifiers and its timestamp is
/// the CoreGraphics event's own (the same seconds-since-boot clock).
#[cfg(target_os = "macos")]
fn press_event(keycode: u16, down: bool, flags: u64) -> aterm_objc::Id {
    use std::ffi::c_void;

    use aterm_objc::{Id, Sel, class, sel};

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreateKeyboardEvent(
            source: *const c_void,
            keycode: u16,
            keydown: bool,
        ) -> *mut c_void;
        fn CGEventSetFlags(event: *mut c_void, flags: u64);
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
    }
    // SAFETY: the CoreGraphics prototypes above are the headers'; a null source is
    // documented. `+eventWithCGEvent:` is `@@:^{__CGEvent=}`: its result is autoreleased
    // and holds its own reference to the CGEvent, so releasing the one
    // `CGEventCreateKeyboardEvent` returned is the documented balance.
    unsafe {
        let cg = CGEventCreateKeyboardEvent(std::ptr::null(), keycode, down);
        if cg.is_null() {
            return Id::NIL;
        }
        CGEventSetFlags(cg, flags);
        let wrap: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) -> Id =
            aterm_objc::checked_msg();
        let event = wrap(class(c"NSEvent").as_id(), sel!(eventWithCGEvent:), cg);
        CFRelease(cg);
        event
    }
}

/// Non-macOS: there is no NSEvent queue to post into, and falling back to the socket path would
/// hide the very stall this verb measures, so it refuses.
#[cfg(not(target_os = "macos"))]
pub(crate) fn post(_spec: &HwKeySpec, _window_number: i64) -> Result<u32, String> {
    Err("ERR hwkey: hardware-path injection is implemented on macOS only\n".to_string())
}

#[cfg(test)]
#[path = "hwkey_tests.rs"]
mod tests;
