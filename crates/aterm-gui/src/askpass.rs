// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The server side of the `sudo` askpass relay: the request the window raises a password sheet for, the answer that comes back, the spacing between sheets, and the pure helpers (`docs/DESIGN-sudo-askpass-2026-10-15.md` §3.4).

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
    /// The app-owned operation (CPU policy, a root step) whose bound sudo asked, if one did.
    pub owned: Option<crate::root_step_authorization::OwnedOperation>,
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
    /// The spacing ([`Backoff::refusal`]) raises no sheet for this long.
    Quiet(Duration),
}

impl std::fmt::Debug for AskpassAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Secret(_) => f.write_str("Secret(<redacted>)"),
            Self::Cancel => f.write_str("Cancel"),
            Self::Refused(why) => write!(f, "Refused({why})"),
            Self::Quiet(wait) => write!(f, "Quiet({}s)", whole_secs_up(*wait)),
        }
    }
}

/// `wait` in whole seconds, rounded up, so a wait of part of a second still names one.
fn whole_secs_up(wait: Duration) -> u64 {
    u64::try_from(wait.as_millis().div_ceil(1000)).unwrap_or(u64::MAX)
}

/// The verb's reply line for an answer.
pub(crate) fn reply_line(answer: &AskpassAnswer) -> aterm_uds::sudo::WireBuffer {
    match answer {
        AskpassAnswer::Secret(secret) => aterm_uds::sudo::WireBuffer::secret_reply(secret),
        AskpassAnswer::Cancel => public_reply("ERR askpass declined\n"),
        AskpassAnswer::Refused(why) => public_reply(&format!("ERR askpass refused {why}\n")),
        AskpassAnswer::Quiet(wait) => {
            public_reply(&aterm_uds::sudo::quiet_refusal(whole_secs_up(*wait)))
        }
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

/// The password sheet's spacing (design §3.4 step 5, O5), kept per instance in three parts. A
/// request inside any of them is refused at once with no sheet ([`Self::refusal`]).
/// - Per window: fed by every sheet that ends with no password and is not taken down with its
///   window or its page's view: a person's Cancel or empty field, and a sheet left to its
///   deadline, its caller's hang-up, a withdrawn request, its session closing or the update
///   backstop.
/// - Per session: fed by those same ends, keyed by the session that asked, so its spacing
///   follows that session into another window (a tab torn off). A sheet a page's view stands for
///   has no session, and feeds the other two parts alone.
/// - Unattended, instance-wide: fed by every end of a sheet nobody touched
///   ([`PasswordInput::touched`] false at its end), its window closing included, and by a sheet
///   found standing at launch after a crash. It spaces every window and session, new ones too, so
///   programs that re-ask from two windows cannot keep a sheet standing by turns. A request is
///   not held by it when a person's gesture came after that end (`person_at`: in the asking
///   session, or the key or press that started an owned operation), since a person is then there
///   to answer. A person's Cancel never feeds it: a person's no is to that program, not to every
///   window.
///
/// A sheet a page's button raised (the System Resources page's CPU policy collection or root
/// step, its origin the page's view) is held by the unattended part alone
/// ([`Self::unattended_refusal`]): only a person's own press raises it, and its operation asks
/// once (a second request cancels it), so no program re-asks through it, and a person's own
/// press never waits out a program's spacing of its window (`App::present_askpass_sheet`). How it
/// ends still feeds the spacing as any sheet's end does.
///
/// Each quiet window starts at [`QUIET_BASE`] and doubles to [`QUIET_MAX`] on each consecutive
/// such end. An answer clears all three. Each whole [`QUIET_MAX`] that a part stood with no sheet
/// after its quiet window ran out takes one end off its streak ([`Quiet::after`]), so sheets a
/// program let lapse long ago do not make a person wait.
///
/// Why an update that refused a sheet still lands: each sheet nobody touched doubles every
/// window's quiet window, so by the k-th in a row it lasts `10·2^(k-1)` s (to 300 s) and outlasts
/// any handoff's switch, and a request refused with no sheet moves neither the handoff's epoch
/// nor its cancel (its wake is Exempt). Nothing bounds a switch by a constant here: the doubling
/// is the argument.
///
/// An update's successor takes up the per-window and per-session quiet windows still running
/// ([`Self::carried`], [`Self::carried_session`], [`Self::adopt`], [`Self::adopt_session`]); the
/// unattended part, and a sheet standing, cross an update, a crash and a relaunch through the
/// per-account record ([`Self::unattended_line`], [`settle_record`], [`unattended_left`],
/// [`Self::take_unattended`]).
#[derive(Default)]
pub(crate) struct Backoff {
    per_window: std::collections::HashMap<u64, Quiet>,
    per_session: std::collections::HashMap<u64, Quiet>,
    unattended: Option<Unattended>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Quiet {
    until: Instant,
    streak: u32,
}

/// The instance-wide part of [`Backoff`]: its quiet window, when its last sheet ended (what a
/// person's gesture must come after to be free of it), and the wall-clock milliseconds the record
/// names for it, from which the record's reader rebuilds `quiet.until` as that time plus the
/// streak's span.
#[derive(Clone, Copy, Debug)]
struct Unattended {
    quiet: Quiet,
    ended: Instant,
    ended_wall_ms: u64,
}

pub(crate) const QUIET_BASE: Duration = Duration::from_secs(10);
pub(crate) const QUIET_MAX: Duration = Duration::from_secs(aterm_uds::sudo::QUIET_MAX_SECS);

/// The streak whose quiet window first reaches [`QUIET_MAX`]; a longer one waits no longer.
const STREAK_MAX: u32 = 6;

/// The quiet window a streak of `streak` consecutive unanswered sheets sets.
fn quiet_span(streak: u32) -> Duration {
    QUIET_BASE
        .saturating_mul(1u32 << streak.saturating_sub(1).min(STREAK_MAX - 1))
        .min(QUIET_MAX)
}

/// `d` in whole milliseconds, saturating.
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

impl Quiet {
    /// The quiet window after a sheet, raised at `raised`, ended at `now` with no password,
    /// following `prev`: one more end on the streak. Each whole [`QUIET_MAX`] between `prev`'s
    /// quiet window running out and this sheet being raised takes one end off the streak first.
    /// The time is read to `raised`, not to `now`: a sheet that stood to its deadline (as long as
    /// `QUIET_MAX`) was no quiet time. And it steps down rather than starting again, so waiting
    /// is never the faster way to the next sheet: a pause of `QUIET_MAX` buys back at most the
    /// one step it is as long as.
    fn after(prev: Option<Self>, raised: Instant, now: Instant) -> Self {
        let streak = prev
            .map_or(0, |q| {
                let quiet = raised.saturating_duration_since(q.until);
                let eased = quiet.as_secs() / QUIET_MAX.as_secs();
                q.streak
                    .min(STREAK_MAX)
                    .saturating_sub(u32::try_from(eased).unwrap_or(u32::MAX))
            })
            .saturating_add(1);
        Self {
            until: now + quiet_span(streak),
            streak,
        }
    }

    /// What is left of the quiet window at `now`; `None` once it ran out.
    fn left(self, now: Instant) -> Option<Duration> {
        (self.until > now).then(|| self.until - now)
    }
}

/// Take up a carried `(streak, remaining)` for `key` in `map`: the streak held to
/// [`STREAK_MAX`] and the time left to what that streak sets, keeping a quiet window already
/// running longer.
fn adopt_into(
    map: &mut std::collections::HashMap<u64, Quiet>,
    key: u64,
    streak: u32,
    remaining: Duration,
    now: Instant,
) {
    if streak == 0 || remaining.is_zero() {
        return;
    }
    let streak = streak.min(STREAK_MAX);
    let until = now + remaining.min(quiet_span(streak));
    if map.get(&key).is_none_or(|own| own.until < until) {
        map.insert(key, Quiet { until, streak });
    }
}

impl Backoff {
    /// What is left of `window`'s own quiet window now, the window part alone of
    /// [`Self::refusal`].
    #[cfg(test)]
    pub(crate) fn quiet_for(&self, window: u64, now: Instant) -> Option<Duration> {
        self.per_window.get(&window).and_then(|q| q.left(now))
    }

    /// How long a request from `session`, shown in `window`, is refused at `now`: the longest of
    /// what is left of the window's, the session's and the unattended quiet windows. The
    /// unattended one does not hold a request whose `person_at` (the later of the session's last
    /// person's gesture and the moment a person's own key or press started an owned operation)
    /// is later than its last end. `None` raises a sheet.
    pub(crate) fn refusal(
        &self,
        window: u64,
        session: u64,
        person_at: Option<Instant>,
        now: Instant,
    ) -> Option<Duration> {
        let window = self.per_window.get(&window).and_then(|q| q.left(now));
        let session = self.per_session.get(&session).and_then(|q| q.left(now));
        let unattended = self.unattended_refusal(person_at, now);
        [window, session, unattended].into_iter().flatten().max()
    }

    /// The unattended part alone of [`Self::refusal`]: what is left of its quiet window at `now`,
    /// unless `person_at` is later than its last end. A request a page's view stands for is held
    /// by this alone.
    pub(crate) fn unattended_refusal(
        &self,
        person_at: Option<Instant>,
        now: Instant,
    ) -> Option<Duration> {
        self.unattended
            .filter(|u| !person_at.is_some_and(|at| at > u.ended))
            .and_then(|u| u.quiet.left(now))
    }

    /// Record a sheet raised in `window` at `raised` that ended at `now` with no password: start
    /// or widen the window's quiet window, and the quiet window of `session`, the session that
    /// asked (`None` for a sheet a page's view stands for, which has none) ([`Quiet::after`]).
    pub(crate) fn on_cancel(
        &mut self,
        window: u64,
        session: Option<u64>,
        raised: Instant,
        now: Instant,
    ) {
        let stepped = Quiet::after(self.per_window.get(&window).copied(), raised, now);
        self.per_window.insert(window, stepped);
        if let Some(session) = session {
            let stepped = Quiet::after(self.per_session.get(&session).copied(), raised, now);
            self.per_session.insert(session, stepped);
        }
    }

    /// Record a sheet nobody touched, raised at `raised`, that ended at `now` (`now_wall_ms` on
    /// the wall clock): start or widen the unattended quiet window, which spaces every window.
    pub(crate) fn on_unattended(&mut self, raised: Instant, now: Instant, now_wall_ms: u64) {
        let quiet = Quiet::after(self.unattended.map(|u| u.quiet), raised, now);
        self.unattended = Some(Unattended {
            quiet,
            ended: now,
            ended_wall_ms: now_wall_ms,
        });
    }

    /// Record an answer in `window` from `session` (`None` for a page's view): a person is
    /// there, so every part clears.
    pub(crate) fn on_answer(&mut self, window: u64, session: Option<u64>) {
        self.per_window.remove(&window);
        if let Some(session) = session {
            self.per_session.remove(&session);
        }
        self.unattended = None;
    }

    /// Forget a session that retired.
    pub(crate) fn forget_session(&mut self, session: u64) {
        self.per_session.remove(&session);
    }

    /// What an update's successor takes up for `window`: the streak and what is left of a
    /// quiet window still running. One that ran out carries nothing.
    pub(crate) fn carried(&self, window: u64, now: Instant) -> Option<(u32, Duration)> {
        let q = self.per_window.get(&window)?;
        q.left(now).map(|left| (q.streak, left))
    }

    /// [`Self::carried`] for `session`'s own quiet window.
    pub(crate) fn carried_session(&self, session: u64, now: Instant) -> Option<(u32, Duration)> {
        let q = self.per_session.get(&session)?;
        q.left(now).map(|left| (q.streak, left))
    }

    /// Take up the spacing an outgoing process carried for `window` ([`Self::carried`]), read
    /// from a file a program of the same user could have written: the streak is held to
    /// [`STREAK_MAX`] and the time left to what that streak sets, so a carried row can refuse
    /// no longer than that many unanswered sheets would have, and a window already quiet for
    /// longer keeps its own.
    pub(crate) fn adopt(&mut self, window: u64, streak: u32, remaining: Duration, now: Instant) {
        adopt_into(&mut self.per_window, window, streak, remaining, now);
    }

    /// [`Self::adopt`] for `session`'s own quiet window.
    pub(crate) fn adopt_session(
        &mut self,
        session: u64,
        streak: u32,
        remaining: Duration,
        now: Instant,
    ) {
        adopt_into(&mut self.per_session, session, streak, remaining, now);
    }

    /// Take the unattended part from what the record leaves ([`unattended_left`]): `streak`, the
    /// time `remaining` and how long ago it ended (`end_age`), read at `now` (`now_wall_ms` on the
    /// wall clock), or nothing where it leaves nothing. It replaces the part held here: a take-up
    /// is made only where the record is this instance's whole account of it, at a launch that is
    /// no update's successor (nothing is held yet) and at an update's activation (the outgoing
    /// process was its only writer until the Commit, and this one raised no sheet before it). The
    /// streak is held to [`STREAK_MAX`] and the time left to what that streak sets. A streak
    /// whose quiet window ran out (`remaining` zero) is kept for the next end's step.
    pub(crate) fn take_unattended(
        &mut self,
        taken: Option<(u32, Duration, Duration)>,
        now: Instant,
        now_wall_ms: u64,
    ) {
        self.unattended =
            taken
                .filter(|&(streak, ..)| streak > 0)
                .map(|(streak, remaining, end_age)| {
                    let streak = streak.min(STREAK_MAX);
                    let left = remaining.min(quiet_span(streak));
                    Unattended {
                        quiet: Quiet {
                            until: now + left,
                            streak,
                        },
                        ended: now.checked_sub(end_age).unwrap_or(now),
                        ended_wall_ms: now_wall_ms
                            .saturating_add(millis(left))
                            .saturating_sub(millis(quiet_span(streak))),
                    }
                });
    }

    /// The record's line for the unattended part and a sheet standing since `standing_wall_ms`
    /// ([`crate::askpass_record::line`]); `None` when there is neither, and the record goes.
    pub(crate) fn unattended_line(&self, standing_wall_ms: Option<u64>) -> Option<String> {
        if self.unattended.is_none() && standing_wall_ms.is_none() {
            return None;
        }
        let (streak, ended) = self
            .unattended
            .map_or((0, None), |u| (u.quiet.streak, Some(u.ended_wall_ms)));
        Some(crate::askpass_record::line(streak, ended, standing_wall_ms))
    }
}

/// A record with a sheet it names as standing counted as one more end: the streak and the
/// wall-clock end of the unattended part, and whether the standing raise was counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settled {
    pub(crate) streak: u32,
    pub(crate) ended: Option<u64>,
    /// The record named a standing raise that this settling counted: a process that owns the
    /// record writes the settled line back, with no standing field, so no later reading counts
    /// that sheet again.
    pub(crate) counted_standing: bool,
}

/// Settle a record's `streak`, `ended` and `standing` raise at `now_ms` on the wall clock.
/// 1. The end is `ended` where it is no later than `now_ms` and `streak` is at least 1; a later
///    one (the clock set back, or a record from the future) is no end.
/// 2. A sheet `standing` since a raise no later than `now_ms` and no earlier than that end went
///    down unanswered with the process that raised it (a crash, a kill, a power loss): it is one
///    more end, stepped from its raise as [`Quiet::after`] steps and held to [`STREAK_MAX`]. It ended no later than its
///    deadline ([`ASKPASS_DEADLINE`] after its raise, when the waiting lane withdraws a sheet),
///    nor than `now_ms`, so a sheet raised long ago settles into a quiet window long over. A raise
///    from the future is ignored.
#[must_use]
pub(crate) fn settle_record(
    streak: u32,
    ended: Option<u64>,
    standing: Option<u64>,
    now_ms: u64,
) -> Settled {
    let held = streak.min(STREAK_MAX);
    let end = ended.filter(|&end| end <= now_ms && streak >= 1);
    let raise = standing.filter(|&raised| raised <= now_ms && end.is_none_or(|end| raised >= end));
    let Some(raised) = raise else {
        return Settled {
            streak: if end.is_some() { held } else { 0 },
            ended: end,
            counted_standing: false,
        };
    };
    let streak = end
        .map_or(0, |end| {
            let quiet = raised.saturating_sub(end.saturating_add(millis(quiet_span(held))));
            let eased = quiet / millis(QUIET_MAX);
            held.saturating_sub(u32::try_from(eased).unwrap_or(u32::MAX))
        })
        .saturating_add(1)
        .min(STREAK_MAX);
    Settled {
        streak,
        ended: Some(now_ms.min(raised.saturating_add(millis(ASKPASS_DEADLINE)))),
        counted_standing: true,
    }
}

/// What a settled unattended part ([`settle_record`]) leaves at `now_ms` on the wall clock: the
/// streak, the time its quiet window has left and how long ago it ended; `None` for nothing that
/// spaces or steps.
/// 3. The quiet window runs to the end plus the span of the streak, held to [`STREAK_MAX`].
/// 4. A quiet window still running gives its streak and what is left, at most [`QUIET_MAX`]; one
///    that ran out gives its streak less one end per whole `QUIET_MAX` since, and no time.
#[must_use]
pub(crate) fn unattended_left(
    streak: u32,
    ended: Option<u64>,
    now_ms: u64,
) -> Option<(u32, Duration, Duration)> {
    let end = ended.filter(|&end| end <= now_ms && streak >= 1)?;
    let streak = streak.min(STREAK_MAX);
    let end_age = Duration::from_millis(now_ms - end);
    let until = end.saturating_add(millis(quiet_span(streak)));
    if until > now_ms {
        let left = Duration::from_millis(until - now_ms).min(QUIET_MAX);
        return Some((streak, left, end_age));
    }
    let eased = (now_ms - until) / millis(QUIET_MAX);
    let streak = streak.saturating_sub(u32::try_from(eased).unwrap_or(u32::MAX));
    (streak > 0).then_some((streak, Duration::ZERO, end_age))
}

/// What a record leaves at `now_ms`: [`unattended_left`] of [`settle_record`].
#[cfg(test)]
#[must_use]
pub(crate) fn unattended_from_record(
    streak: u32,
    ended: Option<u64>,
    standing: Option<u64>,
    now_ms: u64,
) -> Option<(u32, Duration, Duration)> {
    let settled = settle_record(streak, ended, standing, now_ms);
    unattended_left(settled.streak, settled.ended, now_ms)
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
