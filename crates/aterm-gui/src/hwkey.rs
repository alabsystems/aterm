// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hardware-path key injection, for measuring keystroke latency.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
/// (an Apply that admits a setup plan as the window's own, a row's `Turn On…`;
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

/// Whether an `NSEventType` is a key-down or key-up, the kinds a burst posts.
#[cfg_attr(any(test, not(target_os = "macos")), allow(dead_code))]
pub(crate) const fn is_key_kind(kind: usize) -> bool {
    matches!(kind, NS_EVENT_TYPE_KEY_DOWN | NS_EVENT_TYPE_KEY_UP)
}

/// One key event, as the main thread can tell a burst's apart from a person's in
/// `-[NSApp currentEvent]`: its `NSEventType`, virtual keycode and timestamp (seconds since boot).
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PostedKey {
    pub kind: usize,
    pub keycode: u16,
    pub timestamp: f64,
}

impl PostedKey {
    /// Whether `seen` is this event: the same kind and key, stamped within [`STAMP_SLACK`].
    fn is(&self, seen: &Self) -> bool {
        self.kind == seen.kind
            && self.keycode == seen.keycode
            && (self.timestamp - seen.timestamp).abs() <= STAMP_SLACK
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
    waiting: std::collections::VecDeque<PostedKey>,
    found: Option<PostedKey>,
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
    fn note(&mut self, keys: &[PostedKey]) -> bool {
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
    fn take(&mut self, seen: &PostedKey) -> bool {
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
    /// The test build's stand-in for `-[NSApp currentEvent]` ([`current_key`]).
    pub(crate) static CURRENT_KEY_STUB: std::cell::Cell<Option<PostedKey>> =
        const { std::cell::Cell::new(None) };
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

/// Set once a burst noted a key in this process: until then no key is a burst's, and the main
/// thread reads no event to tell (the person's typing pays nothing for `hwkey`).
static NOTED: AtomicBool = AtomicBool::new(false);

/// Record `keys` (one press: its key-down and key-up) as a burst's, before they are posted, or
/// refuse them all, unposted, while the ledger waits for as many keys as it can hold.
///
/// # Errors
///
/// The `ERR hwkey: busy` reply line when the ledger has no room for all of `keys`.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn note_posted(keys: &[PostedKey]) -> Result<(), String> {
    if !with_ledger(|ledger| ledger.note(keys)) {
        return Err(format!(
            "ERR hwkey: busy: {LEDGER_CAP} keys bursts posted are still waiting for aterm to \
             take them, so this burst posts no more\n"
        ));
    }
    NOTED.store(true, Ordering::Release);
    Ok(())
}

/// Held across each press's note and post (`hwkey::post`), so bursts on several control
/// connections post one at a time and the ledger waits for keys in the order AppKit queues them
/// ([`Ledger::take`]).
#[cfg(target_os = "macos")]
static POSTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The key event AppKit is dispatching now (a test build's stand-in: [`CURRENT_KEY_STUB`]).
fn current_key() -> Option<PostedKey> {
    #[cfg(test)]
    {
        CURRENT_KEY_STUB.with(std::cell::Cell::get)
    }
    #[cfg(all(not(test), target_os = "macos"))]
    {
        crate::platform::current_key_event()
    }
    #[cfg(all(not(test), not(target_os = "macos")))]
    {
        None
    }
}

/// Whether the event AppKit is dispatching now is a key a burst posted: it takes the person's own
/// key path (a menu's key equivalent, a binding, the focused view) with no gesture mark of its
/// own, and it is a control client's.
pub(crate) fn current_event_posted() -> bool {
    NOTED.load(Ordering::Acquire) && current_key().is_some_and(event_posted)
}

/// Whether `seen`, a key event a native monitor holds before AppKit dispatches it, is a key a
/// burst posted ([`current_event_posted`] for the event in dispatch). Taken from the ledger once
/// found, as a dispatch takes it.
pub(crate) fn event_posted(seen: PostedKey) -> bool {
    NOTED.load(Ordering::Acquire) && with_ledger(|ledger| ledger.take(&seen))
}

/// One burst: `count` presses `interval_ms` apart, each posted by `post_one`, with the gate read
/// before every key. Once `front` is set the burst stops with nothing more posted, and its `Err`
/// says how many were. Each key is counted into `posted` before `post_one` runs. A `post_one`
/// error ends the burst with that error.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn run_burst(
    count: u32,
    interval_ms: u64,
    front: &AtomicBool,
    posted: &AtomicU64,
    mut post_one: impl FnMut() -> Result<(), String>,
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
        posted.fetch_add(1, Ordering::AcqRel);
        post_one()?;
        sent += 1;
    }
    Ok(sent)
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
            count = n
                .parse::<u32>()
                .map_err(|_| format!("ERR hwkey: bad count `{n}`\n"))?;
            if count == 0 || count > MAX_COUNT {
                return Err(format!("ERR hwkey: count must be 1..={MAX_COUNT}\n"));
            }
        } else if let Some(n) = tok.strip_prefix("interval=") {
            interval = n
                .parse::<u64>()
                .map_err(|_| format!("ERR hwkey: bad interval `{n}`\n"))?;
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
        &POSTED,
        || {
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
                // (`current_event_posted`), so it is recorded before AppKit can dispatch it.
                // SAFETY: `-type` (`-(NSEventType)`, an `NSUInteger`) and `-timestamp`
                // (`-(NSTimeInterval)`) are scalar getters on a live key event this thread built
                // and has not yet posted.
                let stamp = |event: Id| unsafe {
                    PostedKey {
                        kind: appkit::send_usize(event, sel!(type)),
                        keycode: spec.keycode,
                        timestamp: appkit::send_f64(event, sel!(timestamp)),
                    }
                };
                let press: Vec<Id> = [down, up].into_iter().filter(|e| !e.is_null()).collect();
                let keys: Vec<PostedKey> = press.iter().map(|&event| stamp(event)).collect();
                let _order = POSTING
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use super::{
        CURRENT_KEY_STUB, FLAG_CONTROL, FLAG_FUNCTION, FLAG_SHIFT, LEDGER_CAP, Ledger, MAX_COUNT,
        NS_EVENT_TYPE_KEY_DOWN, NS_EVENT_TYPE_KEY_UP, PostedKey, current_event_posted, note_posted,
        parse_hwkey, run_burst,
    };

    /// A FULL LEDGER REFUSES KEYS RATHER THAN FORGET ONE: every posted key must be found when
    /// AppKit dispatches it, however many wait ahead of it, or it runs as the person's (its ⌘V
    /// pastes the clipboard). So once the ledger waits for all it can hold, a burst's next press
    /// is refused whole, unposted (`ERR hwkey: busy`), and the oldest waiting key is still found;
    /// a key found clears every key of its kind posted before it (AppKit took those unseen, an
    /// input method say), which makes room again, and leaves the other kind waiting.
    /// NEGATIVE CONTROL: with `Ledger::note` dropping the oldest waiting key at the cap and
    /// taking the new one (as the branch had it at `0e59d97e00`), the press past the cap was
    /// taken.
    #[test]
    fn a_full_ledger_refuses_keys_rather_than_forget_one() {
        let key = |n: u32, kind: usize| PostedKey {
            kind,
            keycode: 9,
            timestamp: 3_000.0 + f64::from(n),
        };
        let half = u32::try_from(LEDGER_CAP / 2).expect("a small cap");
        let down = |n| key(n, NS_EVENT_TYPE_KEY_DOWN);
        let up = |n| key(n, NS_EVENT_TYPE_KEY_UP);

        let mut ledger = Ledger::default();
        for n in 0..half {
            assert!(ledger.note(&[down(n), up(n)]), "press {n}");
        }
        assert_eq!(ledger.room(), 0, "PRECONDITION: full");
        assert!(!ledger.note(&[down(half), up(half)]), "past the cap");
        assert!(ledger.take(&down(0)), "the oldest key is still found");
        assert!(!ledger.note(&[down(half), up(half)]), "no room for a press");

        assert!(ledger.take(&down(9)), "a later key-down");
        assert_eq!(
            ledger.room(),
            1 + 9,
            "it and the key-downs before it are gone"
        );
        assert!(!ledger.take(&down(5)), "a key-down AppKit took unseen");
        assert!(ledger.take(&up(5)), "the key-ups wait on");
        assert!(ledger.note(&[down(half), up(half)]), "room again");

        let mut refused = Ok(());
        for n in 0..=half {
            refused = note_posted(&[down(n), up(n)]);
            if refused.is_err() {
                assert_eq!(n, half, "refused only at the cap");
            }
        }
        let refused = refused.expect_err("a burst past the cap is refused");
        assert!(refused.starts_with("ERR hwkey: busy"), "{refused}");
    }

    /// THE MAIN THREAD TELLS A POSTED KEY FROM A PERSON'S: a key event the burst noted is found
    /// as it is dispatched, with a stamp CoreGraphics' integer clock moved by a nanosecond, and
    /// again on a second look at the same event (a menu's key equivalent, then a view); the
    /// person's key with another stamp, kind or keycode is not, and takes nothing from the ledger;
    /// a found key is not found again once another was.
    /// NEGATIVE CONTROL: with `STAMP_SLACK` at 0 the posted key with its rounded stamp was not
    /// found.
    #[test]
    fn the_main_thread_tells_a_posted_key_from_a_persons() {
        let at = |key: PostedKey| {
            CURRENT_KEY_STUB.with(|current| current.set(Some(key)));
            let posted = current_event_posted();
            CURRENT_KEY_STUB.with(|current| current.set(None));
            posted
        };
        let down = PostedKey {
            kind: NS_EVENT_TYPE_KEY_DOWN,
            keycode: 9,
            timestamp: 5_000.123_456_789,
        };
        note_posted(&[down]).expect("room");
        assert!(!current_event_posted(), "no event is being dispatched");
        let later = PostedKey {
            timestamp: 5_000.2,
            ..down
        };
        assert!(!at(later), "the person's ⌘V a moment later");
        let up = PostedKey {
            kind: NS_EVENT_TYPE_KEY_UP,
            ..down
        };
        assert!(!at(up), "another kind");
        assert!(!at(PostedKey { keycode: 8, ..down }), "another key");
        let rounded = PostedKey {
            timestamp: 5_000.123_456_79,
            ..down
        };
        assert!(
            at(rounded),
            "the posted key, its stamp in whole nanoseconds"
        );
        assert!(at(down), "a second look at it");

        note_posted(&[up]).expect("room");
        assert!(at(up), "its key-up");
        assert!(!at(down), "the key-down, once the key-up was found");
    }

    /// System State (or a gateway card) coming to the front after the second key of five stops
    /// the burst, which says how many it posted; every posted key was counted before its post.
    /// Control: with nothing of the person's in front all five post, and a front already up posts
    /// none.
    #[test]
    fn a_running_burst_stops_once_the_persons_buttons_come_to_the_front() {
        let front = AtomicBool::new(false);
        let posted = AtomicU64::new(0);
        let mut sent = 0;
        let out = run_burst(5, 0, &front, &posted, || {
            sent += 1;
            if sent == 2 {
                // The page came to the front: the main thread's word.
                front.store(true, Ordering::Release);
            }
            Ok(())
        });
        let refusal = out.expect_err("stopped at the page");
        assert!(refusal.contains("posted=2"), "{refusal}");
        assert_eq!(sent, 2, "nothing posted after it came to the front");
        assert_eq!(posted.load(Ordering::Acquire), 2);

        let quiet = AtomicBool::new(false);
        let mut sent = 0;
        let all = run_burst(5, 0, &quiet, &posted, || {
            sent += 1;
            Ok(())
        });
        assert_eq!(all, Ok(5));
        assert_eq!(posted.load(Ordering::Acquire), 7);

        let mut sent = 0;
        let none = run_burst(5, 0, &front, &posted, || {
            sent += 1;
            Ok(())
        });
        assert!(none.is_err_and(|e| e.contains("posted=0")));
        assert_eq!(sent, 0);
    }

    #[test]
    fn a_plain_letter_is_its_unshifted_physical_key() {
        let s = parse_hwkey("x").expect("x parses");
        assert_eq!(s.keycode, 7, "kVK_ANSI_X");
        assert_eq!(s.chars, "x");
        assert_eq!(s.chars_ignoring, "x");
        assert_eq!(s.flags, 0);
        assert_eq!(s.count, 1);
    }

    #[test]
    fn an_uppercase_letter_shifts_the_same_physical_key() {
        for (plain, shifted) in [("x", "X"), ("1", "!")] {
            let lower = parse_hwkey(plain).unwrap();
            let upper = parse_hwkey(shifted).unwrap();
            assert_eq!(upper.keycode, lower.keycode, "{shifted}");
            assert_eq!(upper.flags, FLAG_SHIFT, "{shifted}");
            assert_eq!(upper.chars_ignoring, shifted);
        }
    }

    #[test]
    fn a_control_chord_carries_the_c0_byte_and_the_bare_letter() {
        let a = parse_hwkey("ctrl+u").unwrap();
        let b = parse_hwkey("u mods=ctrl").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.flags, FLAG_CONTROL);
        assert_eq!(a.chars, "\u{15}");
        assert_eq!(a.chars_ignoring, "u");
    }

    #[test]
    fn named_keys_match_appkit_codes_and_function_flag() {
        let enter = parse_hwkey("enter").unwrap();
        assert_eq!(enter.keycode, 36);
        assert_eq!(enter.chars, "\r");
        assert_eq!(
            enter.flags & FLAG_FUNCTION,
            0,
            "Return is not a function key"
        );

        let up = parse_hwkey("up").unwrap();
        assert_eq!(up.keycode, 126);
        assert_eq!(up.chars, "\u{f700}");
        assert_eq!(up.flags & FLAG_FUNCTION, FLAG_FUNCTION);

        assert_eq!(parse_hwkey("esc").unwrap(), parse_hwkey("escape").unwrap());
        assert_eq!(
            parse_hwkey("enter").unwrap(),
            parse_hwkey("return").unwrap()
        );
    }

    #[test]
    fn no_accepted_key_yields_empty_character_strings() {
        for spelling in [
            "a",
            "Z",
            "0",
            "!",
            "space",
            "enter",
            "tab",
            "esc",
            "backspace",
            "delete",
            "up",
            "down",
            "left",
            "right",
            "home",
            "end",
            "pageup",
            "pagedown",
            "f1",
            "f12",
            "ctrl+c",
            "cmd+v",
            "shift+alt+q",
        ] {
            let s = parse_hwkey(spelling).unwrap_or_else(|e| panic!("{spelling}: {e}"));
            assert!(!s.chars.is_empty(), "{spelling} had empty characters");
            assert!(
                !s.chars_ignoring.is_empty(),
                "{spelling} had empty charactersIgnoringModifiers"
            );
        }
    }

    #[test]
    fn pacing_and_modifier_tokens_combine_and_enforce_bounds() {
        let s = parse_hwkey("x count=200 interval=25").unwrap();
        assert_eq!(s.count, 200);
        assert_eq!(s.interval_ms, 25);
        assert_eq!(s.keycode, parse_hwkey("x").unwrap().keycode);

        let combined = parse_hwkey("x mods=shift mods=ctrl").unwrap();
        assert_eq!(combined.flags, FLAG_SHIFT | FLAG_CONTROL);
        assert_eq!(
            parse_hwkey("shift+x mods=ctrl").unwrap().flags,
            FLAG_SHIFT | FLAG_CONTROL
        );

        assert!(parse_hwkey("x count=0").is_err());
        assert!(parse_hwkey(&format!("x count={}", MAX_COUNT + 1)).is_err());
        assert!(parse_hwkey("x count=1oo").is_err());
        assert!(parse_hwkey("x interval=100000").is_err());
        assert!(parse_hwkey("x mods=bogus").is_err());
    }

    #[test]
    fn the_press_shape_is_asked_for_by_name_and_keeps_the_key() {
        let built = parse_hwkey("b mods=alt").unwrap();
        assert!(!built.press);
        let press = parse_hwkey("b mods=alt shape=press").unwrap();
        assert!(press.press);
        assert_eq!((press.keycode, press.flags), (11, super::FLAG_OPTION));
        assert_eq!(press.chars_ignoring, built.chars_ignoring);
        assert_eq!(parse_hwkey("b mods=alt shape=built").unwrap(), built);
        assert!(parse_hwkey("b shape=cg").is_err());
    }

    #[test]
    fn a_malformed_body_is_refused() {
        assert!(parse_hwkey("").is_err());
        assert!(parse_hwkey("   ").is_err());
        assert!(parse_hwkey("notakey").is_err());
        assert!(parse_hwkey("ab").is_err());
        assert!(parse_hwkey("ctrl+").is_err());
    }
}
