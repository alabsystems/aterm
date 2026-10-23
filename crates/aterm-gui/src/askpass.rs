// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The server side of the `sudo` askpass relay: the request the window raises a
//! password sheet for, the answer that comes back, the decline backoff, and the
//! pure helpers (`docs/DESIGN-sudo-askpass-2026-10-15.md` §3.4). The AppKit
//! sheet itself lives in `lib.rs` (modelled on the pastejacking sheet); the
//! control-socket handler is `control_askpass.rs`. Everything OS-touching is
//! passed in, so the park loop and the shaping are tested headless.

#[cfg(target_os = "macos")]
use std::sync::mpsc::Sender;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::control_askpass::public_reply;

/// The process-wide boundary between synthetic key posting and a password
/// sheet. The native presenter and the control input path share this state.
pub(crate) struct PromptInputGate {
    sheets: std::sync::atomic::AtomicUsize,
    posting: std::sync::Mutex<()>,
}

impl PromptInputGate {
    pub(crate) const fn new() -> Self {
        Self {
            sheets: std::sync::atomic::AtomicUsize::new(0),
            posting: std::sync::Mutex::new(()),
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.sheets.load(std::sync::atomic::Ordering::SeqCst) > 0
    }

    pub(crate) fn with_post<T>(&self, post: impl FnOnce() -> T) -> Result<T, ()> {
        let _posting = self.posting_permit()?;
        Ok(post())
    }

    pub(crate) fn posting_permit(&self) -> Result<std::sync::MutexGuard<'_, ()>, ()> {
        let posting = self.posting.lock().unwrap_or_else(|e| e.into_inner());
        if self.is_open() { Err(()) } else { Ok(posting) }
    }

    pub(crate) fn present<T>(&self, present: impl FnOnce() -> T) -> (T, PromptInputFence<'_>) {
        let posting = self.posting.lock().unwrap_or_else(|e| e.into_inner());
        self.sheets
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let fence = PromptInputFence { gate: self };
        drop(posting);
        (present(), fence)
    }
}

pub(crate) struct PromptInputFence<'a> {
    gate: &'a PromptInputGate,
}

impl Drop for PromptInputFence<'_> {
    fn drop(&mut self) {
        self.gate
            .sheets
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(target_os = "macos")]
pub(crate) static PROMPT_INPUT: PromptInputGate = PromptInputGate::new();

/// Keyboard admission for the secure field, shared with its native monitor.
pub(crate) struct PasswordInput {
    clicked: std::sync::atomic::AtomicBool,
}

impl PasswordInput {
    pub(crate) const fn new() -> Self {
        Self {
            clicked: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn field_clicked(&self) {
        self.clicked
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn key_allowed(&self, owns_event: bool, key_code: u16) -> bool {
        !owns_event
            || key_code == 0x35 // Escape remains the owner's Cancel.
            || self.clicked.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The sheet's wait for the owner, matched to `await consent`'s ceiling. The
/// helper waits longer still (360 s), so the server's answer always wins.
#[cfg(target_os = "macos")]
pub(crate) const ASKPASS_DEADLINE: Duration = Duration::from_secs(300);
/// One park between hang-up checks, so a killed `sudo`/helper frees the lane
/// promptly.
pub(crate) const ASKPASS_TICK: Duration = Duration::from_millis(200);

/// One request's identity and lifetime, shared by its wait lane and sheet.
#[derive(Debug)]
pub(crate) struct AskpassTicket {
    #[cfg(target_os = "macos")]
    pub id: u64,
    pub deadline: Instant,
    cancelled: std::sync::atomic::AtomicBool,
}

impl AskpassTicket {
    pub(crate) const fn new(id: u64, deadline: Instant) -> Self {
        #[cfg(not(target_os = "macos"))]
        let _ = id;
        Self {
            #[cfg(target_os = "macos")]
            id,
            deadline,
            cancelled: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn is_live(&self, now: Instant) -> bool {
        !self.cancelled.load(std::sync::atomic::Ordering::SeqCst) && now < self.deadline
    }

    pub(crate) fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

struct RetireTicket<'a>(&'a AskpassTicket);

impl Drop for RetireTicket<'_> {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// What the owner is asked about. Built only from kernel reads, never from the
/// agent's words. The lane attests and reads these; the App (which holds the
/// session pool and windows) attributes `attribute_from` to a local session,
/// applies the backoff, finds the tab, and presents or refuses.
///
/// * `attribute_from` — the pid to start the session walk from: `sudo`'s parent
///   (the invoking shell), because `sudo`'s own `PROC_PIDTBSDINFO` is refused.
/// * `command` — the command line `sudo` was given (from `ps`, escaped).
/// * `invoker` — the command that invoked `sudo` (from `ps`, escaped).
#[derive(Debug)]
#[cfg(target_os = "macos")]
pub(crate) struct AskpassRequest {
    pub ticket: std::sync::Arc<AskpassTicket>,
    pub attribute_from: i32,
    pub command: String,
    pub invoker: String,
    pub reply: Sender<AskpassAnswer>,
}

/// The sheet's answer, sent back on the request's channel.
pub(crate) enum AskpassAnswer {
    /// The owner typed a password and pressed Allow.
    Secret(aterm_uds::sudo::Secret),
    /// The owner pressed Cancel, or left an empty field.
    Cancel,
    /// The window could not present the sheet (not the active tab, headless).
    Refused(String),
}

impl std::fmt::Debug for AskpassAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Never prints the bytes.
            Self::Secret(_) => f.write_str("Secret(<redacted>)"),
            Self::Cancel => f.write_str("Cancel"),
            Self::Refused(why) => write!(f, "Refused({why})"),
        }
    }
}

/// The verb's reply line for an answer (module docs). The secret is hex so the
/// one line carries no control byte; it is the only place the bytes cross the
/// socket, point to point, never logged.
pub(crate) fn reply_line(answer: &AskpassAnswer) -> aterm_uds::sudo::WireBuffer {
    match answer {
        AskpassAnswer::Secret(secret) => aterm_uds::sudo::WireBuffer::secret_reply(secret),
        AskpassAnswer::Cancel => public_reply("ERR askpass declined\n"),
        AskpassAnswer::Refused(why) => public_reply(&format!("ERR askpass refused {why}\n")),
    }
}

/// Escape a command line for a trusted display: any control char, newline, or
/// Unicode bidi/isolate/override is shown as `\u{XXXX}`, so a crafted argument
/// can neither inject rows into the sheet nor reorder what the owner reads
/// (design §3.4 step 4, §4). `sudo`'s own argv is unreadable per-argument by an
/// unprivileged process, so this is applied to the `ps` line — the command line
/// `sudo` was given — which the sheet labels as such.
pub(crate) fn escape_display(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            // C0/C1 controls, DEL, and the bidi/isolate/override ranges.
            c if c.is_control()
                || ('\u{200e}'..='\u{200f}').contains(&c)
                || ('\u{202a}'..='\u{202e}').contains(&c)
                || ('\u{2066}'..='\u{2069}').contains(&c) =>
            {
                out.push_str(&format!("\\u{{{:04x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// The decline backoff, kept per instance and keyed per WINDOW (design §3.4
/// step 5, O5): after a cancel, the next request for that window inside the
/// quiet window is refused at once; it starts at 10 s and doubles to 5 min on
/// each consecutive cancel, and an answer resets it. Keying per window (not per
/// session) is why opening a new tab does not escape the quiet window — the
/// caller passes the hosting window's id.
#[derive(Default)]
pub(crate) struct Backoff {
    per_window: std::collections::HashMap<u64, Quiet>,
}

#[derive(Clone, Copy)]
struct Quiet {
    until: Instant,
    streak: u32,
}

const QUIET_BASE: Duration = Duration::from_secs(10);
const QUIET_MAX: Duration = Duration::from_secs(300);

impl Backoff {
    /// Whether `window` is inside a quiet window now; `Some(remaining)` refuses.
    pub(crate) fn quiet_for(&self, window: u64, now: Instant) -> Option<Duration> {
        self.per_window
            .get(&window)
            .filter(|q| q.until > now)
            .map(|q| q.until - now)
    }

    /// Record a cancel: start or widen the quiet window.
    pub(crate) fn on_cancel(&mut self, window: u64, now: Instant) {
        let streak = self
            .per_window
            .get(&window)
            .map_or(0, |q| q.streak)
            .saturating_add(1);
        let span = QUIET_BASE
            .saturating_mul(1u32 << streak.saturating_sub(1).min(5))
            .min(QUIET_MAX);
        self.per_window.insert(
            window,
            Quiet {
                until: now + span,
                streak,
            },
        );
    }

    /// Record an answer: clear the window's quiet window.
    pub(crate) fn on_answer(&mut self, window: u64) {
        self.per_window.remove(&window);
    }
}

/// Park on the sheet's answer until it comes, the deadline passes, or the
/// caller hangs up. `recv` is the request's receiver; `hung_up` reports the
/// socket peer gone; `now` the clock. Pure over those, so it is tested with a
/// fake clock and channel. Returns the reply line.
pub(crate) fn await_answer(
    recv: &Receiver<AskpassAnswer>,
    ticket: &AskpassTicket,
    tick: Duration,
    hung_up: &mut dyn FnMut() -> bool,
    now: &mut dyn FnMut() -> Instant,
) -> aterm_uds::sudo::WireBuffer {
    let _retire = RetireTicket(ticket);
    let deadline = ticket.deadline;
    loop {
        if now() >= deadline {
            return public_reply("ERR askpass timeout\n");
        }
        if hung_up() {
            return public_reply("ERR askpass declined\n");
        }
        let wait = tick.min(deadline.saturating_duration_since(now()));
        match recv.recv_timeout(wait) {
            Ok(answer) => {
                if !ticket.is_live(now()) {
                    return public_reply("ERR askpass timeout\n");
                }
                if hung_up() {
                    return public_reply("ERR askpass declined\n");
                }
                return reply_line(&answer);
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The App dropped the sender without answering (a window closed,
            // the sheet withdrawn): not a decline, a transient refusal.
            Err(RecvTimeoutError::Disconnected) => {
                return public_reply("ERR askpass busy the window withdrew the prompt\n");
            }
        }
    }
}

// The owner-only OFF switch (design §3.1)

/// The marker file whose presence means the owner has turned the sudo relay
/// OFF. Under the aterm state dir (`aterm_types::dirs::state_dir`), NOT in
/// agent-writable `aterm.toml`. Absent is the default (relay on).
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) fn relay_off_marker() -> Option<std::path::PathBuf> {
    aterm_types::dirs::state_dir().map(|d| d.join("askpass-off"))
}

/// Whether the relay is off. Only a confirmed absent marker permits the
/// default-on state; an unknown state path or metadata error keeps it off.
/// A dangling marker symlink is still an off entry, without following its target.
///
/// Only the owner's confirmed Settings request writes the marker; no control verb or
/// config edit does. A same-uid program can still change the file directly, so the
/// switch is not a filesystem privilege boundary.
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) fn relay_off() -> bool {
    relay_off_from_metadata(relay_off_marker().as_deref(), marker_metadata_at)
}

/// The same presence decision with an explicit marker and metadata probe, so
/// unknown-path and I/O outcomes can be tested without process-global env or
/// permission changes. The native lane uses `marker_metadata_at` below.
fn relay_off_from_metadata(
    marker: Option<&std::path::Path>,
    metadata: impl FnOnce(&std::path::Path) -> std::io::Result<std::fs::Metadata>,
) -> bool {
    !matches!(
        marker.map(metadata),
        Some(Err(error)) if error.kind() == std::io::ErrorKind::NotFound
    )
}

fn marker_metadata_at(path: &std::path::Path) -> std::io::Result<std::fs::Metadata> {
    std::fs::symlink_metadata(path)
}

/// Persist the owner's confirmed preference, consuming its one-use answer. An unknown
/// state directory or failed filesystem operation is reported to the requesting view.
#[cfg(target_os = "macos")]
pub(crate) fn set_relay_off(
    off: bool,
    _owner: crate::native_settings::OwnerYes,
) -> std::io::Result<()> {
    let path = relay_off_marker().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the aterm state directory is unavailable",
        )
    })?;
    set_marker_at(&path, off)
}

/// Change the marker entry itself. An existing entry already means off, including a
/// symlink or FIFO; never open it or follow its target. Only absence means on.
#[cfg(any(target_os = "macos", test))]
fn set_marker_at(path: &std::path::Path, off: bool) -> std::io::Result<()> {
    if off {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(path) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error),
        }
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
#[path = "askpass_tests.rs"]
mod tests;
