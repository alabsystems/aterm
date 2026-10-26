// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rewind: a pane frozen on a past screen its recorder replays, read-only until it returns to live.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use aterm_aos::publish::Head;
use aterm_buffer::Ticks;
use aterm_core::terminal::Terminal;

use crate::rewind_worker::{Changed, Done, FoldToken, Folded, Found, RewindWorker, Show};
use crate::temporal::{Fidelity, RewindInstant, Track};
use crate::{App, WindowId};

/// The disabled row's tool tip: what turns Rewind on.
#[cfg(any(target_os = "macos", test))]
pub(crate) const REWIND_OFF_TIP: &str =
    "Turn on Settings > Terminal > Rewind to keep screen history for new tabs.";

/// The most lines of its past scrollback a rewound pane scrolls back through.
pub(crate) const PAST_SCROLLBACK: usize = 2048;

/// What the live witnesses say of the shown state; only `Faithful` claims it is what was on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Faith {
    /// Every live witness that covers the shown instant matched its replay.
    Faithful,
    /// A live witness that covers the shown instant did not match its replay.
    MayDiffer,
    /// No live witness has judged the shown instant.
    Unchecked,
}

impl Faith {
    /// The band's words.
    pub(crate) const fn band(self) -> &'static str {
        match self {
            Self::Faithful => "faithful",
            Self::MayDiffer => "may differ from what was on screen",
            Self::Unchecked => "unchecked",
        }
    }

    /// The faith the recorder's verdict on an instant earns ([`crate::temporal::TemporalRecorder::fidelity_at`]): faithful only when the witnesses that cover it matched.
    pub(crate) const fn of(fidelity: Fidelity) -> Self {
        match fidelity {
            Fidelity::Faithful => Self::Faithful,
            Fidelity::Diverged => Self::MayDiffer,
            Fidelity::Unwitnessed => Self::Unchecked,
        }
    }

    /// A word for this faith in a frame's fingerprint.
    pub(crate) const fn code(self) -> u64 {
        match self {
            Self::Faithful => 1,
            Self::MayDiffer => 2,
            Self::Unchecked => 3,
        }
    }

    /// The `rewind status` value of `faithful=`.
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Faithful => "yes",
            Self::MayDiffer => "no",
            Self::Unchecked => "unchecked",
        }
    }
}

/// Where a view opens: the newest recorded instant, one `secs` ago, or one at or before a tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Latest,
    Ago(u64),
    At(Ticks),
}

/// How far a step goes ([`step_to`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepBy {
    /// `n` recorded instants.
    Slice(i64),
    /// `n` seconds, landing at least one instant away.
    Secs(i64),
    /// `n` command starts (OSC 133).
    Cmd(i64),
    /// The oldest (`false`) or newest (`true`) instant.
    End(bool),
}

/// A `rewind` verb's reply channel: the status line once its fold is shown.
pub(crate) type Reply = Sender<Result<String, String>>;

/// One fold asked for: its instant, its place, the replies it answers once shown, and when a step asked for it.
struct Ask {
    at: Ticks,
    state: (usize, usize),
    replies: Vec<Reply>,
    stepped: Option<Instant>,
    /// Fields the replies carry after the status line.
    suffix: String,
}

/// The next view's id: each view in the process has its own, so a fold names the view that asked for it.
static NEXT_VIEW: AtomicU64 = AtomicU64::new(1);

/// One session's open Rewind view.
pub(crate) struct RewindView {
    /// The replayed past screen every render of the session shows, once the first fold lands.
    pub(crate) shown: Option<Arc<Mutex<Terminal>>>,
    /// The shown instant on the recorder's clock.
    pub(crate) at: Ticks,
    /// The shown instant's 1-based place among the recorded instants, and their count.
    pub(crate) state: (usize, usize),
    pub(crate) faith: Faith,
    /// Whether the shown fold passed a live witness that saw another screen ([`crate::rewind_worker::Folded::differs`]).
    differs: bool,
    /// Whether the shown fold ended a mode-2026 frame open at its instant, so it shows a grid that was never on screen.
    mid_frame: bool,
    /// The reader's byte count as the view opened ([`crate::vitals_jobs::JobFeed::bytes`]).
    pub(crate) live_base: u64,
    /// The shown instant on the wall clock, Unix milliseconds.
    pub(crate) shown_unix_ms: i64,
    /// The band's note: where a picked cell last changed, or why a step went nowhere.
    pub(crate) note: Option<String>,
    /// The cell a click picked on the shown screen: its grid row (negative in the scrollback) and column.
    pub(crate) picked: Option<(i32, u16)>,
    /// How far the shown screen is scrolled into its past scrollback, in lines.
    pub(crate) scrolled: usize,
    /// A trackpad's scroll not yet a whole line, in pixels.
    wheel_px: f64,
    /// The track the band draws, as of the last fold shown.
    pub(crate) track: Track,
    /// The shown instant's place on the track, and whether a slice on it started a command.
    pub(crate) instants: Vec<RewindInstant>,
    /// When the main thread took the step whose fold is shown and not yet presented.
    stepped: Option<Instant>,
    /// The windows whose hosted effects this view paused, and the head each paused at.
    paused: Vec<(WindowId, Head)>,
    /// The fold in flight, by token.
    folding: Option<(FoldToken, Ask)>,
    /// The newest fold asked for while one was in flight.
    queued: Option<Ask>,
    /// The cell search in flight, by token, and the reply it answers once its instant is shown.
    finding: Option<(FoldToken, Option<Reply>)>,
    id: u64,
    serial: u64,
    worker: Option<RewindWorker>,
}

impl RewindView {
    fn new(live_base: u64) -> Self {
        Self {
            shown: None,
            at: Ticks(0),
            state: (0, 0),
            faith: Faith::Unchecked,
            differs: false,
            mid_frame: false,
            live_base,
            shown_unix_ms: 0,
            note: None,
            picked: None,
            scrolled: 0,
            wheel_px: 0.0,
            track: Track::default(),
            instants: Vec::new(),
            stepped: None,
            paused: Vec::new(),
            folding: None,
            queued: None,
            finding: None,
            id: NEXT_VIEW.fetch_add(1, Ordering::Relaxed),
            serial: 0,
            worker: None,
        }
    }

    /// The instant the view is at or headed for: the queued fold's, the one in flight, else the shown one.
    fn heading(&self) -> Ticks {
        self.queued
            .as_ref()
            .or(self.folding.as_ref().map(|(_, ask)| ask))
            .map_or(self.at, |ask| ask.at)
    }

    /// Answer every reply still waiting on a fold with `why`.
    fn abandon(self, why: &str) {
        let waiting = self
            .folding
            .map(|(_, ask)| ask)
            .into_iter()
            .chain(self.queued);
        let finding = self.finding.and_then(|(_, reply)| reply);
        for reply in waiting.flat_map(|ask| ask.replies).chain(finding) {
            let _ = reply.send(Err(why.to_string()));
        }
    }

    /// What changes this view's frame: the shown fold, its note, its picked cell, its scroll and the live output's KiB since it opened.
    pub(crate) fn fingerprint(&self, live_bytes: u64) -> u64 {
        let note = self
            .note
            .as_deref()
            .map_or(0, |n| aterm_hash::fnv1a_64(n.as_bytes()));
        let picked = self.picked.map_or(0, |(row, col)| {
            (u64::from(row.cast_unsigned()) << 16 | u64::from(col)) | 1 << 63
        });
        let words = [
            u64::from(self.shown.is_some()),
            self.id,
            self.serial,
            self.at.0,
            live_bytes.saturating_sub(self.live_base) / 1024,
            note,
            picked,
            u64::try_from(self.scrolled).unwrap_or(u64::MAX),
        ];
        aterm_hash::fnv1a_64(&words.map(u64::to_le_bytes).concat()) | 1
    }
}

/// The `rewind status` fields of `view` (none shown: `view=closed`).
pub(crate) fn status_line(recording: bool, view: Option<&RewindView>, live_bytes: u64) -> String {
    match view.filter(|v| v.shown.is_some()) {
        Some(v) => format!(
            "recording={recording} view=open at={} state={}/{} faithful={} live_bytes={}",
            v.at.0,
            v.state.0,
            v.state.1,
            v.faith.wire(),
            live_bytes.saturating_sub(v.live_base),
        ),
        None => format!("recording={recording} view=closed at=- state=- faithful=- live_bytes=0"),
    }
}

/// The band's one line for `view` at wall-clock `now_unix_ms`, its verdict first so a narrow pane keeps it, with the hosted effects' status when any are attached.
pub(crate) fn band_text(
    view: &RewindView,
    now_unix_ms: i64,
    utc_offset_s: i64,
    live_bytes: u64,
    effects: Option<&str>,
) -> String {
    let ago_s = now_unix_ms.saturating_sub(view.shown_unix_ms).max(0) / 1000;
    let live_kib = live_bytes.saturating_sub(view.live_base).div_ceil(1024);
    let live = if live_kib > 0 {
        format!("  live +{live_kib} KB")
    } else {
        String::new()
    };
    let effects = effects.map(|e| format!("  {e}")).unwrap_or_default();
    let tail = view.note.as_deref().unwrap_or(KEYS_HINT);
    format!(
        "REWIND  {}  {} ({})  state {}/{}{effects}{live}  {tail}",
        view.faith.band(),
        clock(view.shown_unix_ms, utc_offset_s),
        ago(ago_s),
        view.state.0,
        view.state.1,
    )
}

/// The band's key hint while no note is shown.
pub(crate) const KEYS_HINT: &str = "Left/Right step  Esc live";

/// `at_unix_ms` as local `HH:MM:SS`.
pub(crate) fn clock(at_unix_ms: i64, utc_offset_s: i64) -> String {
    let secs = at_unix_ms
        .div_euclid(1000)
        .saturating_add(utc_offset_s)
        .rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// `secs` ago as `-M:SS`, or `-H:MM:SS` from an hour.
pub(crate) fn ago(secs: i64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if h > 0 {
        format!("-{h}:{m:02}:{s:02}")
    } else {
        format!("-{m}:{s:02}")
    }
}

/// The index in `instants` a view at `target` opens on, given the recorder's clock `now`.
pub(crate) fn place(
    instants: &[RewindInstant],
    target: Target,
    now: Ticks,
) -> Result<usize, String> {
    let last = instants
        .len()
        .checked_sub(1)
        .ok_or_else(|| "nothing recorded replays yet".to_string())?;
    let at = match target {
        Target::Latest => return Ok(last),
        Target::Ago(secs) => Ticks(now.0.saturating_sub(secs.saturating_mul(1_000_000))),
        Target::At(tick) => tick,
    };
    match instants.partition_point(|i| i.at <= at) {
        0 => Err(format!(
            "that is before the oldest kept state ({}s ago)",
            now.0.saturating_sub(instants[0].at.0) / 1_000_000
        )),
        n => Ok(n - 1),
    }
}

/// The index among `instants` a step `by` from the instant at or before `at` lands on, clamped to them; `Err` when a command step finds no command start that way, or when `at` is older than every kept instant and the step is not forward.
pub(crate) fn step_to(instants: &[RewindInstant], at: Ticks, by: StepBy) -> Result<usize, String> {
    let last = instants
        .len()
        .checked_sub(1)
        .ok_or_else(|| "nothing recorded replays now".to_string())?;
    let here = instants.partition_point(|i| i.at <= at).checked_sub(1);
    let far = |n: i64| usize::try_from(n.unsigned_abs()).unwrap_or(usize::MAX);
    let forward = match by {
        StepBy::Slice(n) | StepBy::Secs(n) | StepBy::Cmd(n) => n > 0,
        StepBy::End(_) => true,
    };
    let here = match here {
        Some(here) => Some(here),
        None if forward => None,
        None => return Err(EVICTED.to_string()),
    };
    let next = here.map_or(0, |h| h + 1);
    Ok(match (by, here) {
        (StepBy::End(newest), _) => {
            if newest {
                last
            } else {
                0
            }
        }
        (StepBy::Slice(n), Some(here)) if n <= 0 => here.saturating_sub(far(n)),
        (StepBy::Slice(n), _) => next.saturating_add(far(n) - 1).min(last),
        (StepBy::Secs(0) | StepBy::Cmd(0), Some(here)) => here,
        (StepBy::Secs(n), _) => {
            let span = n.unsigned_abs().saturating_mul(1_000_000);
            let from = here.map_or(at, |h| instants[h].at).0;
            if n < 0 {
                let to = Ticks(from.saturating_sub(span));
                let at_or_before = instants.partition_point(|i| i.at <= to).saturating_sub(1);
                at_or_before.min(here.unwrap_or(0).saturating_sub(1))
            } else {
                let to = Ticks(from.saturating_add(span));
                let at_or_before = instants.partition_point(|i| i.at <= to).saturating_sub(1);
                at_or_before.max(next).min(last)
            }
        }
        (StepBy::Cmd(n), _) => {
            let starts: Vec<usize> = (0..instants.len())
                .filter(|&i| instants[i].command)
                .collect();
            if n < 0 {
                let before = starts.partition_point(|&i| here.is_some_and(|h| i < h));
                if before == 0 {
                    return Err("no earlier command start is kept".to_string());
                }
                starts[before.saturating_sub(far(n))]
            } else {
                let after = starts.partition_point(|&i| i < next);
                if after == starts.len() {
                    return Err("no later command start is recorded".to_string());
                }
                starts[after.saturating_add(far(n) - 1).min(starts.len() - 1)]
            }
        }
    })
}

/// Whether `ev` is a key or a mouse report a rewound pane withholds from its program.
pub(crate) const fn withheld(ev: &crate::input::InputEvent) -> bool {
    use crate::input::InputEvent as E;
    matches!(
        ev,
        E::Key { .. }
            | E::Text(_)
            | E::KeySequence(_)
            | E::Paste(..)
            | E::MouseButton { .. }
            | E::MouseMove { .. }
            | E::Wheel { .. }
            | E::ScrollView(_)
    )
}

/// Whether a withheld `ev` was a person reaching for the program (a press, text, a paste, a click or a wheel), which the message band answers.
pub(crate) fn noticed(ev: &crate::input::InputEvent) -> bool {
    use crate::input::InputEvent as E;
    use aterm_types::keyboard::KeyEventType as T;
    match ev {
        E::Key {
            key, event_type, ..
        } => {
            matches!(event_type, T::Press | T::Repeat)
                && !aterm_types::keyboard::is_modifier_or_lock_key(key)
        }
        E::Text(_)
        | E::KeySequence(_)
        | E::Paste(..)
        | E::MouseButton { pressed: true, .. }
        | E::Wheel { .. } => true,
        _ => false,
    }
}

/// What a key does in a rewound pane ([`App::rewind_key`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyUse {
    Step(StepBy),
    Close,
    /// B: show where the picked cell last changed.
    Changed,
    /// Page Up (+1) or Page Down (-1): scroll the past screen a page.
    Page(i32),
    /// A menu or app chord: it goes on to its handler.
    Pass,
    /// Anything else: withheld from the program.
    Withhold,
}

/// The rewind keys: the arrows step (plain a slice, Option a second, Shift a command, Cmd to either end), Home and End go to either end, B finds the picked cell's last change, Page Up and Down scroll, and Esc returns to live; another Cmd chord passes.
pub(crate) fn key_use(
    key: &aterm_window::keyboard::Key,
    mods: aterm_window::keyboard::ModifiersState,
    inert: bool,
) -> KeyUse {
    use aterm_window::keyboard::{Key, NamedKey};
    if inert {
        return KeyUse::Pass;
    }
    let held = (
        mods.super_key(),
        mods.control_key(),
        mods.alt_key(),
        mods.shift_key(),
    );
    let plain = held == (false, false, false, false);
    let dir = match key {
        Key::Named(NamedKey::ArrowLeft) => Some(-1),
        Key::Named(NamedKey::ArrowRight) => Some(1),
        _ => None,
    };
    if let Some(dir) = dir {
        match held {
            (false, false, false, false) => return KeyUse::Step(StepBy::Slice(dir)),
            (false, false, true, false) => return KeyUse::Step(StepBy::Secs(dir)),
            (false, false, false, true) => return KeyUse::Step(StepBy::Cmd(dir)),
            (true, false, false, false) => return KeyUse::Step(StepBy::End(dir > 0)),
            _ => {}
        }
    }
    match key {
        Key::Named(NamedKey::Escape) if plain => KeyUse::Close,
        Key::Named(NamedKey::Home) if plain => KeyUse::Step(StepBy::End(false)),
        Key::Named(NamedKey::End) if plain => KeyUse::Step(StepBy::End(true)),
        Key::Named(NamedKey::PageUp) if plain => KeyUse::Page(1),
        Key::Named(NamedKey::PageDown) if plain => KeyUse::Page(-1),
        Key::Character(c) if !(held.0 || held.1 || held.2) && c.eq_ignore_ascii_case("b") => {
            KeyUse::Changed
        }
        _ if mods.super_key() => KeyUse::Pass,
        _ if !cfg!(target_os = "macos") && mods.control_key() && mods.shift_key() => KeyUse::Pass,
        _ => KeyUse::Withhold,
    }
}

/// `bytes` as one line a person can read: printable text as is and every control byte escaped, at most `max` chars.
pub(crate) fn escaped(bytes: &[u8], max: usize) -> String {
    let mut out = String::new();
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\\' => out.push_str("\\\\"),
                '\u{1b}' => out.push_str("\\e"),
                c if c.is_control() => {
                    for b in c.to_string().bytes() {
                        out.push_str(&format!("\\x{b:02x}"));
                    }
                }
                c => out.push(c),
            }
        }
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    if out.chars().count() > max {
        let keep: String = out.chars().take(max.saturating_sub(1)).collect();
        return keep + "\u{2026}";
    }
    out
}

/// The band's note for a found change ([`Found`]): where the cell is, how long ago, and the bytes.
pub(crate) fn changed_note(found: &Found, now: Ticks) -> String {
    let (row, col) = (found.cell.0 + 1, found.cell.1 + 1);
    let Some(at) = found.at else {
        return format!("row {row} col {col}: nothing recorded replays");
    };
    let ago = now.0.saturating_sub(at.0) as f64 / 1_000_000.0;
    if !found.changed {
        return format!("row {row} col {col} is as at the oldest kept state, {ago:.1} s ago");
    }
    let resize = found
        .resize
        .map(|(rows, cols)| format!(", resize to {cols}x{rows}"))
        .unwrap_or_default();
    format!(
        "row {row} col {col} changed {ago:.1} s ago: {} bytes{resize} \"{}\"",
        found.bytes.len(),
        escaped(&found.bytes, 48)
    )
}

impl App {
    /// Window `wid`'s focused-pane handles with the terminal a render shows ([`crate::SessionPool::shown_term`]).
    pub(crate) fn shown_front(
        &self,
        wid: WindowId,
    ) -> Option<crate::front_content::TerminalMirror> {
        let mut mirror = self.front_terminal_mirror(wid)?;
        if let Some(term) = self.pool.shown_term(mirror.session) {
            mirror.term = term;
        }
        Some(mirror)
    }

    /// Window `wid`'s focused pane: the terminal it shows (the past screen while rewound) and its session, for selection, copy and find.
    pub(crate) fn front_shown(&self, wid: WindowId) -> Option<(Arc<Mutex<Terminal>>, u64)> {
        let terminal = self.front_terminal(wid)?;
        let term = self
            .pool
            .shown_term(terminal.session)
            .unwrap_or_else(|| terminal.term.clone());
        Some((term, terminal.session))
    }

    /// The focused pane's session in window `wid`, when it is rewound.
    pub(crate) fn rewound_front(&self, wid: WindowId) -> Option<u64> {
        let session = self.front_terminal(wid)?.session;
        self.pool.is_rewound(session).then_some(session)
    }

    /// The rewound session the pointer is over in window `wid`, if any.
    pub(crate) fn rewound_under_pointer(&self, wid: WindowId) -> Option<u64> {
        let session = match self.pointer_route(wid, None) {
            crate::app_mouse::PointerRoute::Front => self.front_terminal(wid)?.session,
            crate::app_mouse::PointerRoute::Pane { session, .. } => session,
            crate::app_mouse::PointerRoute::Nowhere => return None,
        };
        self.pool.is_rewound(session).then_some(session)
    }

    /// Whether the focused pane of window `wid` records, so Rewind can open on it.
    pub(crate) fn front_can_rewind(&self, wid: WindowId) -> bool {
        self.front_terminal(wid)
            .and_then(|t| self.pool.get(t.session))
            .is_some_and(recording)
    }

    /// Whether `session` keeps screen history, so Rewind can open on it.
    pub(crate) fn session_records(&self, session: u64) -> bool {
        self.pool.get(session).is_some_and(recording)
    }

    /// Open a tab menu's captured terminal view at the row's instant (`turn_ledger::now_ms`'s clock), refusing a source that left its original window and tab.
    pub(crate) fn dispatch_tab_menu_rewind(
        &mut self,
        wid: WindowId,
        tab: crate::tab_model::TabId,
        origin: crate::session_chrome::TerminalMenuOrigin,
        at_ms: u64,
    ) {
        let Some(index) = self.terminal_menu_origin_index(wid, tab, origin) else {
            return;
        };
        self.switch_tab_in(wid, index);
        self.focus_session_pane(wid, origin.session, origin.view);
        let back_us = crate::turn_ledger::now_ms()
            .saturating_sub(at_ms)
            .saturating_mul(1000);
        let at = self.pool.get(origin.session).map(|s| {
            let now = s
                .ctx
                .temporal
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .now();
            Ticks(now.0.saturating_sub(back_us))
        });
        let Some(at) = at else {
            return;
        };
        if let Err(why) = self.rewind_open(origin.session, Target::At(at), None) {
            self.post_message(crate::message_reporters::rewind_failed(&why));
        }
    }

    /// View > Rewind Pane on window `wid`: open on the focused pane's newest state, or return it to live.
    pub(crate) fn toggle_rewind(&mut self, wid: WindowId) {
        let Some(session) = self.front_terminal(wid).map(|t| t.session) else {
            return;
        };
        if self.pool.is_rewound(session) {
            self.rewind_close(session);
            return;
        }
        if let Err(why) = self.rewind_open(session, Target::Latest, None) {
            self.post_message(crate::message_reporters::rewind_failed(&why));
        }
    }

    /// The key `key` on window `wid` while its focused pane is rewound: whether Rewind took it (never while the find bar or an in-grid rename has the keys).
    pub(crate) fn rewind_key(
        &mut self,
        wid: WindowId,
        mods: aterm_window::keyboard::ModifiersState,
        key: &aterm_window::keyboard::Key,
        inert: bool,
    ) -> bool {
        let Some(session) = self.rewound_front(wid) else {
            return false;
        };
        let typing_elsewhere = self.inline_rename_edit(wid).is_some()
            || self.windows.get(&wid).is_some_and(|ws| ws.search.is_some());
        if typing_elsewhere {
            return false;
        }
        match key_use(key, mods, inert) {
            KeyUse::Step(by) => {
                if let Err(why) = self.rewind_step(session, by, None) {
                    self.rewind_note(session, why);
                }
                true
            }
            KeyUse::Close => {
                self.rewind_close(session);
                true
            }
            KeyUse::Changed => {
                if let Err(why) = self.rewind_changed(session) {
                    self.rewind_note(session, why);
                }
                true
            }
            KeyUse::Page(pages) => {
                let rows = self.windows.get(&wid).map_or(1, |ws| ws.rows.max(2) - 1);
                self.rewind_scroll(session, pages.saturating_mul(i32::from(rows)));
                true
            }
            KeyUse::Pass => false,
            KeyUse::Withhold => {
                self.note_rewind_withheld(session);
                true
            }
        }
    }

    /// Put `note` on `session`'s band.
    fn rewind_note(&mut self, session: u64, note: String) {
        if let Some(view) = self.pool.rewind_mut(session) {
            view.note = Some(note);
            self.request_redraw_all_windows();
        }
    }

    /// A person's key or press the rewound `session` withheld: say so once on the message band.
    pub(crate) fn note_rewind_withheld(&mut self, session: u64) {
        let Some(view) = self.pool.rewind(session) else {
            return;
        };
        let shown = if view.shown.is_some() {
            clock(
                view.shown_unix_ms,
                crate::presence::local_offset_s().unwrap_or(0),
            )
        } else {
            "an earlier screen".to_string()
        };
        let key = read_only_key(session);
        if self.messages.live_by_key(&key).is_some() {
            return;
        }
        self.post_message(crate::message_reporters::rewind_read_only(&shown).key(&key));
    }

    /// A left press (`pressed`) or release on the rewound `session` in window `wid`: a press picks the cell and starts a selection on the past screen, a release ends it.
    pub(crate) fn rewind_press(&mut self, wid: WindowId, session: u64, pressed: bool) {
        if !pressed {
            if self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
                self.finish_selection(wid, false);
            }
            return;
        }
        let Some((term, _)) = self.front_shown(wid) else {
            return;
        };
        let (row, col) = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| ws.last_mouse_cell);
        let offset = crate::term_lock(&term).grid().display_offset();
        let grid_row = i32::from(row) - i32::try_from(offset).unwrap_or(i32::MAX);
        if let Some(view) = self.pool.rewind_mut(session) {
            view.picked = Some((grid_row, col));
        }
        self.begin_selection(wid, aterm_core::selection::SelectionType::Simple);
        self.request_redraw_all_windows();
    }

    /// Scroll the rewound `session`'s past screen `lines` (positive: older), at most [`PAST_SCROLLBACK`] lines back.
    pub(crate) fn rewind_scroll(&mut self, session: u64, lines: i32) {
        let Some(term) = self.pool.rewind(session).and_then(|v| v.shown.clone()) else {
            return;
        };
        let scrolled = {
            let mut term = crate::term_lock(&term);
            let now = term.grid().display_offset();
            let room = term.grid().scrollback_lines().min(PAST_SCROLLBACK);
            let to = i64::try_from(now)
                .unwrap_or(i64::MAX)
                .saturating_add(i64::from(lines))
                .clamp(0, i64::try_from(room).unwrap_or(i64::MAX));
            let delta = to - i64::try_from(now).unwrap_or(i64::MAX);
            term.scroll_display(i32::try_from(delta).unwrap_or(0));
            term.grid().display_offset()
        };
        if let Some(view) = self.pool.rewind_mut(session) {
            view.scrolled = scrolled;
        }
        self.request_redraw_all_windows();
    }

    /// A wheel or trackpad scroll of `dy` lines (`px` when in pixels, at `cell_h` pixels a line) over the rewound `session`.
    pub(crate) fn rewind_wheel(&mut self, session: u64, dy: f64, px: bool, cell_h: f64) {
        let Some(view) = self.pool.rewind_mut(session) else {
            return;
        };
        let lines = if px {
            view.wheel_px += dy;
            let whole = (view.wheel_px / cell_h.max(1.0)).trunc();
            view.wheel_px -= whole * cell_h.max(1.0);
            whole
        } else {
            (dy * 3.0).round()
        };
        if lines != 0.0 && lines.is_finite() {
            self.rewind_scroll(session, lines.clamp(-1e6, 1e6) as i32);
        }
    }

    /// Open (or retarget) `session`'s view at `target`; `reply` answers once the fold is shown.
    pub(crate) fn rewind_open(
        &mut self,
        session: u64,
        target: Target,
        reply: Option<Reply>,
    ) -> Result<(), String> {
        self.rewind_open_with(session, target, reply, String::new())
    }

    /// [`Self::rewind_open`], its reply carrying `suffix` after the status line.
    fn rewind_open_with(
        &mut self,
        session: u64,
        target: Target,
        reply: Option<Reply>,
        suffix: String,
    ) -> Result<(), String> {
        let s = self.pool.get(session).ok_or(NO_SESSION)?;
        if !recording(s) {
            return Err(NOT_RECORDING.to_string());
        }
        let (instants, now) = {
            let rec = s
                .ctx
                .temporal
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            (rec.rewind_instants(), rec.now())
        };
        let idx = place(&instants, target, now)?;
        let live_base = s.ctx.job_feed.bytes();
        if self.pool.rewind(session).is_none() {
            let mut view = RewindView::new(live_base);
            view.paused = self.rewind_pause_effects(session);
            self.pool.set_rewind(session, Some(view));
            self.forget_hover();
            self.palette_refresh_live();
        }
        let ask = Ask {
            at: instants[idx].at,
            state: (idx + 1, instants.len()),
            replies: reply.into_iter().collect(),
            stepped: None,
            suffix,
        };
        let opened = self.rewind_fold(session, ask);
        if opened.is_err() && self.pool.rewind(session).is_some_and(|v| v.shown.is_none()) {
            self.rewind_close(session);
        }
        opened
    }

    /// Step `session`'s open view `by`; `reply` answers once the fold is shown.
    pub(crate) fn rewind_step(
        &mut self,
        session: u64,
        by: StepBy,
        reply: Option<Reply>,
    ) -> Result<(), String> {
        let stepped = Instant::now();
        let at = self.pool.rewind(session).ok_or(NO_VIEW)?.heading();
        let s = self.pool.get(session).ok_or(NO_SESSION)?;
        let instants = s
            .ctx
            .temporal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .rewind_instants();
        let idx = step_to(&instants, at, by)?;
        if let Some(view) = self.pool.rewind_mut(session) {
            view.note = None;
        }
        let ask = Ask {
            at: instants[idx].at,
            state: (idx + 1, instants.len()),
            replies: reply.into_iter().collect(),
            stepped: Some(stepped),
            suffix: String::new(),
        };
        self.rewind_fold(session, ask)
    }

    /// Find where `session`'s picked cell last changed at or before the shown instant, and go there (B).
    pub(crate) fn rewind_changed(&mut self, session: u64) -> Result<(), String> {
        self.rewind_changed_for(session, None, None)
    }

    /// Find where `cell` (0-based; default the picked one) of `session`'s shown screen last changed, and go there; `reply` answers once it is shown.
    pub(crate) fn rewind_changed_for(
        &mut self,
        session: u64,
        cell: Option<(u16, u16)>,
        reply: Option<Reply>,
    ) -> Result<(), String> {
        let view = self.pool.rewind(session).ok_or(NO_VIEW)?;
        let cell = match (cell, view.picked) {
            (Some(cell), _) => cell,
            (None, Some((row, col))) => {
                let row = u16::try_from(row).map_err(|_| {
                    "that cell is in the scrollback; B reads a cell on the screen".to_string()
                })?;
                (row, col)
            }
            (None, None) => return Err("click a cell first, then press B".to_string()),
        };
        let at = view.at;
        let s = self.pool.get(session).ok_or(NO_SESSION)?;
        let (chains, instants) = {
            let rec = s
                .ctx
                .temporal
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let instants: Vec<RewindInstant> = rec
                .rewind_instants()
                .into_iter()
                .filter(|i| i.at <= at)
                .collect();
            (rec.chain_plans(at), instants)
        };
        if chains.is_empty() || instants.is_empty() {
            return Err("that state no longer replays".to_string());
        }
        let proxy = self.proxy.clone();
        let view = self.pool.rewind_mut(session).ok_or(NO_VIEW)?;
        view.serial = view.serial.wrapping_add(1);
        let token = FoldToken {
            view: view.id,
            serial: view.serial,
        };
        let changed = Changed {
            session,
            token,
            chains,
            instants,
            at,
            cell,
        };
        let worker = match (view.worker.take(), proxy) {
            (Some(worker), _) => worker,
            (None, Some(proxy)) => start_worker(proxy)?,
            (None, None) => {
                view.finding = Some((token, reply));
                let found = crate::rewind_worker::changed_inline(changed);
                self.rewind_done(Done::Changed(found));
                return Ok(());
            }
        };
        let sent = worker.changed(changed);
        view.worker = Some(worker);
        sent?;
        view.finding = Some((token, reply));
        view.note = Some(format!(
            "finding where row {} col {} last changed\u{2026}",
            cell.0 + 1,
            cell.1 + 1
        ));
        self.request_redraw_all_windows();
        Ok(())
    }

    /// Return `session` to live: its view and its worker end, and the hosted effects it paused resume.
    pub(crate) fn rewind_close(&mut self, session: u64) -> bool {
        let Some(mut view) = self.pool.set_rewind(session, None) else {
            return false;
        };
        let paused = std::mem::take(&mut view.paused);
        view.abandon("the Rewind view returned to live before that state was shown");
        self.rewind_resume_effects(paused);
        if let Some(id) = self
            .messages
            .live_by_key(&read_only_key(session))
            .map(|l| l.id)
        {
            self.withdraw_message(id);
        }
        self.rewind_refind(session);
        self.request_redraw_all_windows();
        self.palette_refresh_live();
        true
    }

    /// Pause the running hosted effects of each window whose focused pane is `session`: the windows and the heads they paused at.
    fn rewind_pause_effects(&mut self, session: u64) -> Vec<(WindowId, Head)> {
        let now = Instant::now();
        let ids: Vec<WindowId> = self
            .windows
            .keys()
            .copied()
            .filter(|wid| {
                self.front_terminal(*wid)
                    .is_some_and(|t| t.session == session)
            })
            .collect();
        let mut paused = Vec::new();
        for wid in ids {
            let Some(ws) = self.windows.get_mut(&wid) else {
                continue;
            };
            let lane = ws.effect_present_interval();
            let momentum = &mut ws.fx.cursor_mut().momentum;
            if momentum.aos_attached()
                && !momentum.aos_paused()
                && let Ok(head) = momentum.aos_pause(now, lane)
            {
                paused.push((wid, head));
            }
        }
        paused
    }

    /// Resume each of `paused` still paused at the head Rewind paused it at; one paused again since (a repair, `aos pause`) is left paused.
    fn rewind_resume_effects(&mut self, paused: Vec<(WindowId, Head)>) {
        for (wid, head) in paused {
            let Some(ws) = self.windows.get_mut(&wid) else {
                continue;
            };
            let momentum = &mut ws.fx.cursor_mut().momentum;
            if momentum.aos_paused_head() == Some(&head) {
                let _ = momentum.aos_resume();
            }
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// The band's words for window `wid`'s hosted effects: none attached, none.
    pub(crate) fn rewind_effects_status(&self, wid: WindowId) -> Option<&'static str> {
        let momentum = &self.windows.get(&wid)?.fx.cursor().momentum;
        if !momentum.aos_attached() {
            return None;
        }
        Some(if momentum.aos_paused() {
            "effects paused"
        } else {
            "effects running"
        })
    }

    /// Drop every window's link hover, which was probed against a live screen.
    fn forget_hover(&mut self) {
        for ws in self.windows.values_mut() {
            ws.link_hover = None;
            ws.hover_memo = None;
        }
    }

    /// Re-run each open find whose window's focused pane is `session`, now that it shows another terminal.
    fn rewind_refind(&mut self, session: u64) {
        let ids: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| ws.search.is_some())
            .map(|(wid, _)| *wid)
            .filter(|wid| {
                self.front_terminal(*wid)
                    .is_some_and(|t| t.session == session)
            })
            .collect();
        for wid in ids {
            self.search_recompute_in(wid);
        }
    }

    /// The faith the recorder now gives `session`'s shown instant, or the last it gave while the recorder is busy; a fold that saw a witness differ may differ whatever the verdicts say, and a frame the fold had to end is unchecked.
    pub(crate) fn rewind_faith(&self, session: u64) -> Option<Faith> {
        let view = self.pool.rewind(session).filter(|v| v.shown.is_some())?;
        if view.differs {
            return Some(Faith::MayDiffer);
        }
        if view.mid_frame {
            return Some(Faith::Unchecked);
        }
        let s = self.pool.get(session)?;
        Some(match s.ctx.temporal.try_lock() {
            Ok(rec) => Faith::of(rec.fidelity_at(view.at)),
            Err(std::sync::TryLockError::Poisoned(p)) => {
                Faith::of(p.into_inner().fidelity_at(view.at))
            }
            Err(std::sync::TryLockError::WouldBlock) => view.faith,
        })
    }

    /// Bring `session`'s view's faith up to the recorder's verdicts, which later folds (a rekey, `temporal`) may have reached.
    pub(crate) fn refresh_rewind_faith(&mut self, session: u64) {
        if let Some(faith) = self.rewind_faith(session)
            && let Some(view) = self.pool.rewind_mut(session)
        {
            view.faith = faith;
        }
    }

    /// The status `rewind status` answers for `session`.
    pub(crate) fn rewind_status(&mut self, session: u64) -> Result<String, String> {
        self.refresh_rewind_faith(session);
        let s = self.pool.get(session).ok_or(NO_SESSION)?;
        Ok(status_line(
            recording(s),
            self.pool.rewind(session),
            s.ctx.job_feed.bytes(),
        ))
    }

    /// The past screen `session`'s view shows, for `rewind text`.
    pub(crate) fn rewind_shown(&self, session: u64) -> Result<Arc<Mutex<Terminal>>, String> {
        self.pool
            .rewind(session)
            .and_then(|v| v.shown.clone())
            .ok_or_else(|| NO_VIEW.to_string())
    }

    fn rewind_fold(&mut self, session: u64, ask: Ask) -> Result<(), String> {
        let proxy = self.proxy.clone();
        let colors = self
            .pool
            .own_colors(session)
            .cloned()
            .or_else(|| self.session_factory.terminal_config.clone().map(Arc::new));
        let s = self.pool.get(session).ok_or(NO_SESSION)?;
        let (temporal, live_bytes) = (s.ctx.temporal.clone(), s.ctx.job_feed.bytes());
        let view = self.pool.rewind_mut(session).ok_or(NO_VIEW)?;
        if view.folding.is_some() {
            let queued = view.queued.take();
            let stepped = queued.as_ref().and_then(|q| q.stepped).or(ask.stepped);
            let mut replies = queued.map(|q| q.replies).unwrap_or_default();
            replies.extend(ask.replies);
            view.queued = Some(Ask {
                replies,
                stepped,
                ..ask
            });
            return Ok(());
        }
        if view.shown.is_some() && view.at == ask.at {
            view.state = ask.state;
            let line = status_line(true, Some(view), live_bytes) + &ask.suffix;
            for reply in ask.replies {
                let _ = reply.send(Ok(line.clone()));
            }
            return Ok(());
        }
        let (plan, judge) = {
            let rec = temporal.lock().unwrap_or_else(PoisonError::into_inner);
            let plan = rec
                .replay_plan(Some(ask.at))
                .ok_or("that state no longer replays")?;
            let judge = (rec.fidelity_at(ask.at) == Fidelity::Unwitnessed)
                .then(|| rec.covering_witness(ask.at))
                .flatten()
                .filter(|w| *w > ask.at)
                .and_then(|w| rec.replay_plan(Some(w)));
            (plan, judge)
        };
        view.serial = view.serial.wrapping_add(1);
        let token = FoldToken {
            view: view.id,
            serial: view.serial,
        };
        let show = Show {
            session,
            token,
            plan,
            judge,
            colors,
        };
        let worker = match (view.worker.take(), proxy) {
            (Some(worker), _) => worker,
            (None, Some(proxy)) => start_worker(proxy)?,
            (None, None) => {
                view.folding = Some((token, ask));
                let folded = crate::rewind_worker::show_inline(show);
                self.rewind_done(Done::Shown(Box::new(folded)));
                return Ok(());
            }
        };
        let sent = worker.show(show);
        view.worker = Some(worker);
        sent?;
        view.folding = Some((token, ask));
        Ok(())
    }

    /// The fold thread finished something ([`crate::Wake::RewindDone`]).
    pub(crate) fn rewind_done(&mut self, done: Done) {
        match done {
            Done::Shown(folded) => self.rewind_folded(*folded),
            Done::Judged { session, judged } => {
                if let Some(s) = self.pool.get(session) {
                    s.ctx
                        .temporal
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .judge(judged);
                }
                if self.pool.rewind(session).is_some() {
                    self.refresh_rewind_faith(session);
                    self.request_redraw_all_windows();
                    self.palette_refresh_live();
                }
            }
            Done::Changed(found) => self.rewind_found(found),
        }
    }

    /// A cell search landed: note it on the band and go to the instant it found.
    fn rewind_found(&mut self, found: Found) {
        let session = found.session;
        let Some(s) = self.pool.get(session) else {
            return;
        };
        let now = s
            .ctx
            .temporal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .now();
        let Some(view) = self.pool.rewind_mut(session) else {
            return;
        };
        let Some((_, reply)) = view.finding.take_if(|(token, _)| *token == found.token) else {
            return;
        };
        let note = changed_note(&found, now);
        let suffix = format!(
            " cell={},{} changed={} bytes={}",
            found.cell.0,
            found.cell.1,
            if found.changed { "yes" } else { "no" },
            found.bytes.len()
        );
        view.note = Some(note.clone());
        let Some(at) = found.at else {
            if let Some(reply) = reply {
                let _ = reply.send(Err("that state no longer replays".to_string()));
            }
            self.request_redraw_all_windows();
            return;
        };
        if let Err(why) = self.rewind_open_with(session, Target::At(at), reply.clone(), suffix) {
            if let Some(reply) = reply {
                let _ = reply.send(Err(why.clone()));
            }
            self.rewind_note(session, why);
            return;
        }
        if let Some(view) = self.pool.rewind_mut(session) {
            view.note = Some(note);
        }
        self.request_redraw_all_windows();
    }

    /// A fold landed: show it if its token is the open view's fold in flight, answer its replies, and start the queued one.
    fn rewind_folded(&mut self, folded: Folded) {
        let Folded {
            session,
            token,
            term,
            mid_frame,
            differs,
            judged,
        } = folded;
        let Some(s) = self.pool.get(session) else {
            return;
        };
        let at = self
            .pool
            .rewind(session)
            .and_then(|v| v.folding.as_ref())
            .filter(|(want, _)| *want == token)
            .map(|(_, ask)| ask.at);
        let (recorder_now, fidelity, track, instants) = {
            let mut rec = s
                .ctx
                .temporal
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(verdicts) = judged {
                rec.judge(verdicts);
            }
            (
                rec.now(),
                at.map(|at| rec.fidelity_at(at)),
                at.map(|_| rec.rewind_track()),
                at.map(|_| rec.rewind_instants()),
            )
        };
        let live_bytes = s.ctx.job_feed.bytes();
        let Some(view) = self.pool.rewind_mut(session) else {
            return;
        };
        if view.folding.as_ref().is_none_or(|(want, _)| *want != token) {
            return;
        }
        let Some((_, ask)) = view.folding.take() else {
            return;
        };
        let behind_ms = recorder_now.0.saturating_sub(ask.at.0) / 1000;
        view.shown = Some(Arc::new(Mutex::new(term)));
        view.at = ask.at;
        view.state = ask.state;
        view.differs = differs;
        view.mid_frame = mid_frame;
        view.faith = match fidelity {
            _ if differs => Faith::MayDiffer,
            Some(fidelity) if !mid_frame => Faith::of(fidelity),
            _ => Faith::Unchecked,
        };
        view.shown_unix_ms = crate::harness_screen::unix_ms_now()
            .saturating_sub(i64::try_from(behind_ms).unwrap_or(i64::MAX));
        view.track = track.unwrap_or_default();
        view.instants = instants.unwrap_or_default();
        view.scrolled = 0;
        view.wheel_px = 0.0;
        view.stepped = view.stepped.or(ask.stepped);
        let line = status_line(true, Some(view), live_bytes) + &ask.suffix;
        if let Some(stepped) = ask.stepped {
            crate::metrics::note_rewind_shown(stepped.elapsed());
        }
        for reply in ask.replies {
            let _ = reply.send(Ok(line.clone()));
        }
        if let Some(next) = view.queued.take() {
            let replies: Vec<Reply> = next.replies.clone();
            if let Err(why) = self.rewind_fold(session, next) {
                for reply in replies {
                    let _ = reply.send(Err(why.clone()));
                }
            }
        }
        self.rewind_refind(session);
        self.request_redraw_all_windows();
        self.palette_refresh_live();
    }

    /// Window `wid` presented: book each step shown in it since it was asked for (`metrics` `rewind_step_p99_ms`).
    pub(crate) fn rewind_presented(&mut self, wid: WindowId) {
        if !self.pool.any_rewound() {
            return;
        }
        for (session, _) in self.rewound_panes(wid) {
            if let Some(view) = self.pool.rewind_mut(session)
                && let Some(at) = view.stepped.take()
            {
                crate::metrics::note_rewind_step(at.elapsed());
            }
        }
    }
}

/// A fold thread whose results wake the main thread ([`crate::Wake::RewindDone`]).
fn start_worker(
    proxy: aterm_window::event_loop::EventLoopProxy<crate::Wake>,
) -> Result<RewindWorker, String> {
    RewindWorker::start(move |done| {
        proxy
            .send_event(crate::Wake::RewindDone(Box::new(done)))
            .is_ok()
    })
}

/// Whether `session` records its screen ([`crate::temporal::TemporalRecorder`]).
fn recording(session: &crate::Session) -> bool {
    session
        .ctx
        .temporal
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .total_events()
        > 0
}

/// The message-band key of `session`'s read-only notice.
fn read_only_key(session: u64) -> String {
    format!("session.rewind-read-only.{session}")
}

const NO_SESSION: &str = "no such session";

const NOT_RECORDING: &str = "this session keeps no screen history; turn on Settings > Terminal > \
                             Rewind and open a new tab";

const NO_VIEW: &str = "no Rewind view is open on this session (rewind open)";

const EVICTED: &str = "the shown state is no longer kept; Right steps to the oldest kept state";

#[cfg(test)]
#[path = "rewind_tests.rs"]
pub(crate) mod tests;
