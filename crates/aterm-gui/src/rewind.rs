// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rewind: a pane frozen on a past screen its recorder replays, read-only until it returns to live.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};

use aterm_buffer::Ticks;
use aterm_core::terminal::Terminal;

use crate::rewind_worker::{Folded, RewindWorker};
use crate::temporal::Fidelity;
use crate::{App, WindowId};

/// The disabled row's tool tip: what turns Rewind on.
#[cfg(any(target_os = "macos", test))]
pub(crate) const REWIND_OFF_TIP: &str =
    "Turn on Settings > Terminal > Rewind to keep screen history for new tabs.";

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

/// A `rewind` verb's reply channel: the status line once its fold is shown.
pub(crate) type Reply = Sender<Result<String, String>>;

/// One fold asked for: its instant, its place, and the replies it answers once shown.
struct Ask {
    at: Ticks,
    state: (usize, usize),
    replies: Vec<Reply>,
}

/// One session's open Rewind view.
pub(crate) struct RewindView {
    /// The replayed past screen every render of the session shows, once the first fold lands.
    pub(crate) shown: Option<Arc<Mutex<Terminal>>>,
    /// The shown instant on the recorder's clock.
    pub(crate) at: Ticks,
    /// The shown instant's 1-based place among the recorded instants, and their count.
    pub(crate) state: (usize, usize),
    pub(crate) faith: Faith,
    /// The reader's byte count as the view opened ([`crate::vitals_jobs::JobFeed::bytes`]).
    pub(crate) live_base: u64,
    /// The shown instant on the wall clock, Unix milliseconds.
    pub(crate) shown_unix_ms: i64,
    /// The fold in flight, by serial.
    folding: Option<(u64, Ask)>,
    /// The newest fold asked for while one was in flight.
    queued: Option<Ask>,
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
            live_base,
            shown_unix_ms: 0,
            folding: None,
            queued: None,
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
        for reply in waiting.flat_map(|ask| ask.replies) {
            let _ = reply.send(Err(why.to_string()));
        }
    }

    /// What changes this view's frame: the shown fold's serial and the live output's KiB since it opened.
    pub(crate) fn fingerprint(&self, live_bytes: u64) -> u64 {
        let words = [
            u64::from(self.shown.is_some()),
            self.serial,
            self.at.0,
            live_bytes.saturating_sub(self.live_base) / 1024,
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

/// The band's one line for `view` at wall-clock `now_unix_ms`.
pub(crate) fn band_text(
    view: &RewindView,
    now_unix_ms: i64,
    utc_offset_s: i64,
    live_bytes: u64,
) -> String {
    let ago_s = now_unix_ms.saturating_sub(view.shown_unix_ms).max(0) / 1000;
    let live_kib = live_bytes.saturating_sub(view.live_base).div_ceil(1024);
    let live = if live_kib > 0 {
        format!("  live +{live_kib} KB")
    } else {
        String::new()
    };
    format!(
        "REWIND  {} ({})  state {}/{}  {}{live}  Left/Right step  Esc live",
        clock(view.shown_unix_ms, utc_offset_s),
        ago(ago_s),
        view.state.0,
        view.state.1,
        view.faith.band(),
    )
}

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
pub(crate) fn place(instants: &[Ticks], target: Target, now: Ticks) -> Result<usize, String> {
    let last = instants
        .len()
        .checked_sub(1)
        .ok_or_else(|| "nothing recorded replays yet".to_string())?;
    let at = match target {
        Target::Latest => return Ok(last),
        Target::Ago(secs) => Ticks(now.0.saturating_sub(secs.saturating_mul(1_000_000))),
        Target::At(tick) => tick,
    };
    match instants.partition_point(|t| *t <= at) {
        0 => Err(format!(
            "that is before the oldest kept state ({}s ago)",
            now.0.saturating_sub(instants[0].0) / 1_000_000
        )),
        n => Ok(n - 1),
    }
}

/// The index `delta` steps from `at` among `instants`, clamped to them.
pub(crate) fn step_index(instants: &[Ticks], at: Ticks, delta: i64) -> Option<usize> {
    let last = instants.len().checked_sub(1)?;
    let here = instants.partition_point(|t| *t <= at).saturating_sub(1);
    let to = i64::try_from(here).ok()?.saturating_add(delta).max(0);
    Some(usize::try_from(to).unwrap_or(usize::MAX).min(last))
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
    Step(i64),
    Close,
    /// A menu or app chord: it goes on to its handler.
    Pass,
    /// Anything else: withheld from the program.
    Withhold,
}

/// The rewind keys: Left/Right step one recorded slice and Esc returns to live; a Cmd chord passes.
pub(crate) fn key_use(
    key: &aterm_window::keyboard::Key,
    mods: aterm_window::keyboard::ModifiersState,
    inert: bool,
) -> KeyUse {
    use aterm_window::keyboard::{Key, NamedKey};
    if inert {
        return KeyUse::Pass;
    }
    let plain = !(mods.super_key() || mods.control_key() || mods.alt_key() || mods.shift_key());
    match key {
        Key::Named(NamedKey::ArrowLeft) if plain => KeyUse::Step(-1),
        Key::Named(NamedKey::ArrowRight) if plain => KeyUse::Step(1),
        Key::Named(NamedKey::Escape) if plain => KeyUse::Close,
        _ if mods.super_key() => KeyUse::Pass,
        _ if !cfg!(target_os = "macos") && mods.control_key() && mods.shift_key() => KeyUse::Pass,
        _ => KeyUse::Withhold,
    }
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

    /// The key `key` on window `wid` while its focused pane is rewound: whether Rewind took it.
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
        match key_use(key, mods, inert) {
            KeyUse::Step(delta) => {
                if let Err(why) = self.rewind_step(session, delta, None) {
                    self.post_message(crate::message_reporters::rewind_failed(&why));
                }
                true
            }
            KeyUse::Close => {
                self.rewind_close(session);
                true
            }
            KeyUse::Pass => false,
            KeyUse::Withhold => {
                self.note_rewind_withheld(session);
                true
            }
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

    /// Open (or retarget) `session`'s view at `target`; `reply` answers once the fold is shown.
    pub(crate) fn rewind_open(
        &mut self,
        session: u64,
        target: Target,
        reply: Option<Reply>,
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
            self.pool
                .set_rewind(session, Some(RewindView::new(live_base)));
            self.palette_refresh_live();
        }
        let ask = Ask {
            at: instants[idx],
            state: (idx + 1, instants.len()),
            replies: reply.into_iter().collect(),
        };
        let opened = self.rewind_fold(session, ask);
        if opened.is_err() && self.pool.rewind(session).is_some_and(|v| v.shown.is_none()) {
            self.rewind_close(session);
        }
        opened
    }

    /// Step `session`'s open view `delta` recorded instants; `reply` answers once the fold is shown.
    pub(crate) fn rewind_step(
        &mut self,
        session: u64,
        delta: i64,
        reply: Option<Reply>,
    ) -> Result<(), String> {
        let at = self.pool.rewind(session).ok_or(NO_VIEW)?.heading();
        let s = self.pool.get(session).ok_or(NO_SESSION)?;
        let instants = s
            .ctx
            .temporal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .rewind_instants();
        let idx = step_index(&instants, at, delta).ok_or("nothing recorded replays now")?;
        let ask = Ask {
            at: instants[idx],
            state: (idx + 1, instants.len()),
            replies: reply.into_iter().collect(),
        };
        self.rewind_fold(session, ask)
    }

    /// Return `session` to live: its view, and its worker, end.
    pub(crate) fn rewind_close(&mut self, session: u64) -> bool {
        let Some(view) = self.pool.set_rewind(session, None) else {
            return false;
        };
        view.abandon("the Rewind view returned to live before that state was shown");
        if let Some(id) = self
            .messages
            .live_by_key(&read_only_key(session))
            .map(|l| l.id)
        {
            self.withdraw_message(id);
        }
        self.request_redraw_all_windows();
        self.palette_refresh_live();
        true
    }

    /// The faith the recorder now gives `session`'s shown instant, or the last it gave while the recorder is busy.
    pub(crate) fn rewind_faith(&self, session: u64) -> Option<Faith> {
        let view = self.pool.rewind(session).filter(|v| v.shown.is_some())?;
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
            let mut replies = view.queued.take().map(|q| q.replies).unwrap_or_default();
            replies.extend(ask.replies);
            view.queued = Some(Ask { replies, ..ask });
            return Ok(());
        }
        if view.shown.is_some() && view.at == ask.at {
            view.state = ask.state;
            let line = status_line(true, Some(view), live_bytes);
            for reply in ask.replies {
                let _ = reply.send(Ok(line.clone()));
            }
            return Ok(());
        }
        let plans = {
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
        let serial = view.serial;
        let Some(proxy) = proxy else {
            view.folding = Some((serial, ask));
            let folded = crate::rewind_worker::fold(session, serial, plans, colors.as_deref());
            self.rewind_folded(folded);
            return Ok(());
        };
        let worker = match view.worker.take() {
            Some(worker) => worker,
            None => RewindWorker::start(proxy)?,
        };
        let sent = worker.fold(session, serial, plans, colors);
        view.worker = Some(worker);
        sent?;
        view.folding = Some((serial, ask));
        Ok(())
    }

    /// A fold landed ([`crate::Wake::RewindFolded`]): show it, answer its replies, and start the queued one.
    pub(crate) fn rewind_folded(&mut self, folded: Folded) {
        let Folded {
            session,
            serial,
            term,
            judged,
        } = folded;
        let Some(s) = self.pool.get(session) else {
            return;
        };
        let at = self
            .pool
            .rewind(session)
            .and_then(|v| v.folding.as_ref())
            .filter(|(want, _)| *want == serial)
            .map(|(_, ask)| ask.at);
        let (recorder_now, fidelity) = {
            let mut rec = s
                .ctx
                .temporal
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for verdicts in judged {
                rec.judge(verdicts);
            }
            (rec.now(), at.map(|at| rec.fidelity_at(at)))
        };
        let live_bytes = s.ctx.job_feed.bytes();
        let Some(view) = self.pool.rewind_mut(session) else {
            return;
        };
        if view
            .folding
            .as_ref()
            .is_none_or(|(want, _)| *want != serial)
        {
            return;
        }
        let Some((_, ask)) = view.folding.take() else {
            return;
        };
        let behind_ms = recorder_now.0.saturating_sub(ask.at.0) / 1000;
        view.shown = Some(Arc::new(Mutex::new(term)));
        view.at = ask.at;
        view.state = ask.state;
        view.faith = fidelity.map_or(Faith::Unchecked, Faith::of);
        view.shown_unix_ms = crate::harness_screen::unix_ms_now()
            .saturating_sub(i64::try_from(behind_ms).unwrap_or(i64::MAX));
        let line = status_line(true, Some(view), live_bytes);
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
        self.request_redraw_all_windows();
        self.palette_refresh_live();
    }
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

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
