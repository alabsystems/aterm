// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The server side of the `sudo` askpass relay: the request the window raises a password sheet for, the answer that comes back, the decline backoff, and the pure helpers (`docs/DESIGN-sudo-askpass-2026-10-15.md` §3.4).

#[cfg(target_os = "macos")]
use std::sync::mpsc::Sender;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::control_askpass::public_reply;

/// The process-wide boundary between synthetic key posting and a password sheet.
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

/// Keyboard admission for the secure field, shared with its native monitor, and whether a person
/// has touched the sheet at all.
pub(crate) struct PasswordInput {
    clicked: std::sync::atomic::AtomicBool,
    touched: std::sync::atomic::AtomicBool,
}

impl PasswordInput {
    pub(crate) const fn new() -> Self {
        Self {
            clicked: std::sync::atomic::AtomicBool::new(false),
            touched: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn field_clicked(&self) {
        self.clicked
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.person_touched();
    }

    /// A person's own click on the sheet or key at its window (never a posted one): the sheet is
    /// theirs from now on. It admits no typing; only [`Self::field_clicked`] does.
    pub(crate) fn person_touched(&self) {
        self.touched
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn key_allowed(&self, owns_event: bool, key_code: u16) -> bool {
        !owns_event
            || key_code == 0x35 // Escape remains the owner's Cancel.
            || self.clicked.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Whether a person has clicked on the sheet or pressed a key at its window.
    pub(crate) fn touched(&self) -> bool {
        self.touched.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The sheet's wait for the owner, matched to `await consent`'s default timeout. The update
/// lane's `password-prompt` hold is bounded by it, since every sheet is withdrawn by then.
pub(crate) const ASKPASS_DEADLINE: Duration = Duration::from_secs(300);

/// How long aterm's own password sheet may hold a waiting update while nobody has touched it:
/// past this, with an update waiting, aterm refuses it, as an unanswered prompt is recorded and
/// the rest goes on. A sheet a person clicked on or typed at is theirs until it is answered or
/// its [`ASKPASS_DEADLINE`].
pub(crate) const ASKPASS_UNTOUCHED_BACKSTOP: Duration = Duration::from_secs(120);

const _: () = assert!(
    ASKPASS_UNTOUCHED_BACKSTOP.as_secs() > crate::native_update_auto_intent::LANDS_WITHIN.as_secs()
        && ASKPASS_UNTOUCHED_BACKSTOP.as_secs() < ASKPASS_DEADLINE.as_secs()
);

/// Whether an update waiting on aterm's own password sheet, raised at `raised` and `touched` or
/// not, refuses it at `now` ([`ASKPASS_UNTOUCHED_BACKSTOP`]).
#[must_use]
pub(crate) fn untouched_sheet_overdue(raised: Instant, touched: bool, now: Instant) -> bool {
    !touched && now.saturating_duration_since(raised) >= ASKPASS_UNTOUCHED_BACKSTOP
}
/// One park between hang-up checks, so a killed `sudo`/helper frees the lane promptly.
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

/// What the owner is asked about.
#[derive(Debug)]
#[cfg(target_os = "macos")]
pub(crate) struct AskpassRequest {
    pub ticket: std::sync::Arc<AskpassTicket>,
    pub attribute_from: i32,
    pub owned: Option<std::sync::Arc<crate::cpu_policy_authorization::Operation>>,
    /// aterm started this sudo itself (`control_sudo`): the words are exact and the password
    /// authorizes them alone, since that sudo runs with `-k`.
    pub exact: bool,
    pub command: String,
    pub purpose: Option<String>,
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
            Self::Secret(_) => f.write_str("Secret(<redacted>)"),
            Self::Cancel => f.write_str("Cancel"),
            Self::Refused(why) => write!(f, "Refused({why})"),
        }
    }
}

/// The verb's reply line for an answer.
pub(crate) fn reply_line(answer: &AskpassAnswer) -> aterm_uds::sudo::WireBuffer {
    match answer {
        AskpassAnswer::Secret(secret) => aterm_uds::sudo::WireBuffer::secret_reply(secret),
        AskpassAnswer::Cancel => public_reply("ERR askpass declined\n"),
        AskpassAnswer::Refused(why) => public_reply(&format!("ERR askpass refused {why}\n")),
    }
}

/// Escape a command line for a trusted display: any control char, newline, line or paragraph separator (U+2028, U+2029), or Unicode bidi mark/isolate/override (U+061C included) is shown as `\u{XXXX}`, so a crafted argument can neither inject rows into the sheet nor reorder what the owner reads (design §3.4 step 4, §4).
pub(crate) fn escape_display(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            c if c.is_control()
                || c == '\u{061c}'
                || ('\u{200e}'..='\u{200f}').contains(&c)
                || ('\u{2028}'..='\u{2029}').contains(&c)
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

/// The decline backoff, kept per instance and keyed per WINDOW (design §3.4 step 5, O5): after a
/// sheet ends with no password (a Cancel, an empty field, its deadline, its caller's hang-up, a
/// withdrawn request, its tab closing, the update backstop), the next request for that window
/// inside the quiet window is refused at once. The quiet window starts at 10 s and doubles to
/// 5 min on each consecutive such end. An answer resets it, and each [`QUIET_MAX`] the window
/// stood with no sheet after a quiet window ran out takes one end off the streak, so sheets a
/// program let lapse long ago do not make a person wait. An update's successor takes up a quiet
/// window still running ([`Self::carried`], [`Self::adopt`]).
#[derive(Default)]
pub(crate) struct Backoff {
    per_window: std::collections::HashMap<u64, Quiet>,
}

#[derive(Clone, Copy)]
struct Quiet {
    until: Instant,
    streak: u32,
}

pub(crate) const QUIET_BASE: Duration = Duration::from_secs(10);
pub(crate) const QUIET_MAX: Duration = Duration::from_secs(300);

/// The streak whose quiet window first reaches [`QUIET_MAX`]; a longer one waits no longer.
const STREAK_MAX: u32 = 6;

/// The quiet window a streak of `streak` consecutive unanswered sheets sets.
fn quiet_span(streak: u32) -> Duration {
    QUIET_BASE
        .saturating_mul(1u32 << streak.saturating_sub(1).min(STREAK_MAX - 1))
        .min(QUIET_MAX)
}

impl Backoff {
    /// Whether `window` is inside a quiet window now; `Some(remaining)` refuses.
    pub(crate) fn quiet_for(&self, window: u64, now: Instant) -> Option<Duration> {
        self.per_window
            .get(&window)
            .filter(|q| q.until > now)
            .map(|q| q.until - now)
    }

    /// Record a sheet, raised at `raised`, that ended at `now` with no password: start or widen
    /// the quiet window. Each whole [`QUIET_MAX`] between the last quiet window running out and
    /// this sheet being raised takes one end off the streak. The time is read to `raised`, not
    /// to `now`: a sheet that stood to its deadline (as long as `QUIET_MAX`) was no quiet time.
    /// And it steps down rather than starting again, so waiting is never the faster way to the
    /// next sheet: a pause of `QUIET_MAX` buys back at most the one step it is as long as.
    pub(crate) fn on_cancel(&mut self, window: u64, raised: Instant, now: Instant) {
        let streak = self
            .per_window
            .get(&window)
            .map_or(0, |q| {
                let quiet = raised.saturating_duration_since(q.until);
                let eased = quiet.as_secs() / QUIET_MAX.as_secs();
                q.streak
                    .min(STREAK_MAX)
                    .saturating_sub(u32::try_from(eased).unwrap_or(u32::MAX))
            })
            .saturating_add(1);
        self.per_window.insert(
            window,
            Quiet {
                until: now + quiet_span(streak),
                streak,
            },
        );
    }

    /// What an update's successor takes up for `window`: the streak and what is left of a
    /// quiet window still running. One that ran out carries nothing.
    pub(crate) fn carried(&self, window: u64, now: Instant) -> Option<(u32, Duration)> {
        self.per_window
            .get(&window)
            .filter(|q| q.until > now)
            .map(|q| (q.streak, q.until - now))
    }

    /// Take up the spacing an outgoing process carried for `window` ([`Self::carried`]), read
    /// from a file a program of the same user could have written: the streak is held to
    /// [`STREAK_MAX`] and the time left to what that streak sets, so a carried row can refuse
    /// no longer than that many unanswered sheets would have, and a window already quiet for
    /// longer keeps its own.
    pub(crate) fn adopt(&mut self, window: u64, streak: u32, remaining: Duration, now: Instant) {
        if streak == 0 || remaining.is_zero() {
            return;
        }
        let streak = streak.min(STREAK_MAX);
        let until = now + remaining.min(quiet_span(streak));
        if self
            .per_window
            .get(&window)
            .is_none_or(|own| own.until < until)
        {
            self.per_window.insert(window, Quiet { until, streak });
        }
    }

    /// Record an answer: clear the window's quiet window.
    pub(crate) fn on_answer(&mut self, window: u64) {
        self.per_window.remove(&window);
    }
}

/// Park on the sheet's answer until it comes, the ticket is cancelled, the deadline
/// passes, or the caller hangs up.
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
        let observed_at = now();
        if observed_at >= deadline {
            return public_reply("ERR askpass timeout\n");
        }
        if !ticket.is_live(observed_at) || hung_up() {
            return public_reply("ERR askpass declined\n");
        }
        let wait = tick.min(deadline.saturating_duration_since(now()));
        match recv.recv_timeout(wait) {
            Ok(answer) => {
                let answered_at = now();
                if answered_at >= deadline {
                    return public_reply("ERR askpass timeout\n");
                }
                if !ticket.is_live(answered_at) || hung_up() {
                    return public_reply("ERR askpass declined\n");
                }
                return reply_line(&answer);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return public_reply("ERR askpass busy the window withdrew the prompt\n");
            }
        }
    }
}

/// The marker file whose presence means the owner has turned the sudo relay OFF.
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) fn relay_off_marker() -> Option<std::path::PathBuf> {
    aterm_types::dirs::state_dir().map(|d| d.join("askpass-off"))
}

/// Whether the relay is off.
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) fn relay_off() -> bool {
    relay_off_from_metadata(relay_off_marker().as_deref(), marker_metadata_at)
}

/// The same presence decision with an explicit marker and metadata probe, so unknown-path and I/O outcomes can be tested without process-global env or permission changes.
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

/// Persist the owner's confirmed preference, consuming its one-use answer.
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

/// Change the marker entry itself.
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
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
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
